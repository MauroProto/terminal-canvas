//! Prompt-bearing launches cross the shell using only an opaque UUID. The
//! helper reads a private request and passes the prompt as a native argument.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AgentProvider;

const MAX_REQUEST_BYTES: u64 = 2 * 1024 * 1024;
const STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

#[derive(Serialize, Deserialize)]
struct LaunchRequest {
    provider: AgentProvider,
    brief: String,
}

#[derive(Debug)]
struct Lease {
    path: PathBuf,
    handed_off: AtomicBool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.handed_off.load(Ordering::Acquire) {
            let path = self.path.clone();
            // Cancellation can occur in an egui handler. Even cleanup stays
            // off that thread; an unsuccessful spawn leaves bounded TTL cleanup.
            let _ = std::thread::Builder::new()
                .name("launch-request-cleanup".into())
                .spawn(move || {
                    let _ = std::fs::remove_file(path);
                });
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedAgentLaunch {
    pub command: String,
    lease: Arc<Lease>,
}

impl PreparedAgentLaunch {
    pub fn hand_off(&self) {
        self.lease.handed_off.store(true, Ordering::Release);
    }
}

fn request_dir() -> anyhow::Result<PathBuf> {
    let directories = directories::ProjectDirs::from("", "", "terminal-app")
        .ok_or_else(|| anyhow::anyhow!("No se pudo resolver el directorio de lanzamientos"))?;
    Ok(directories.data_dir().join("agent-launches"))
}

pub fn prepare(
    provider: AgentProvider,
    brief: &str,
) -> anyhow::Result<Option<PreparedAgentLaunch>> {
    if brief.trim().is_empty()
        || !matches!(
            provider,
            AgentProvider::ClaudeCode
                | AgentProvider::CodexCli
                | AgentProvider::GeminiCli
                | AgentProvider::OpenCode
        )
    {
        return Ok(None);
    }
    let directory = request_dir()?;
    let token = write_request(&directory, provider, brief)?;
    let path = directory.join(format!("{token}.json"));
    let lease = Arc::new(Lease {
        path,
        handed_off: AtomicBool::new(false),
    });
    let executable = std::env::current_exe()?;
    let shell = crate::config::runtime_config()
        .shell
        .unwrap_or_else(crate::utils::platform::default_shell);
    let command = helper_command(&executable, token, &shell);
    Ok(Some(PreparedAgentLaunch { command, lease }))
}

fn write_request(directory: &Path, provider: AgentProvider, brief: &str) -> anyhow::Result<Uuid> {
    anyhow::ensure!(
        !brief.contains('\0'),
        "El prompt contiene un carácter NUL no válido"
    );
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(directory)?
            .file_type()
            .is_symlink(),
        "El directorio de lanzamientos no puede ser un enlace"
    );
    cleanup_stale(directory, SystemTime::now());
    let token = Uuid::new_v4();
    let bytes = serde_json::to_vec(&LaunchRequest {
        provider,
        brief: brief.to_owned(),
    })?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_REQUEST_BYTES,
        "El prompt supera el límite de 2 MiB"
    );
    let path = directory.join(format!("{token}.json"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = result {
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    Ok(token)
}

fn cleanup_stale(directory: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.take(256).flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| Uuid::parse_str(stem).ok())
                .is_none()
        {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        if metadata
            .modified()
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age >= STALE_AFTER)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn consume_request(directory: &Path, token: Uuid) -> anyhow::Result<LaunchRequest> {
    let path = directory.join(format!("{token}.json"));
    let metadata = std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Solicitud de lanzamiento inválida"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_REQUEST_BYTES,
        "Solicitud de lanzamiento demasiado grande"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&path)?
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    // Do not retain a successfully read prompt, even if its JSON is invalid.
    std::fs::remove_file(path)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_REQUEST_BYTES,
        "Solicitud de lanzamiento demasiado grande"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn helper_command(executable: &Path, token: Uuid, shell: &str) -> String {
    let path = executable.to_string_lossy();
    let shell = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase();
    if shell.starts_with("powershell") || shell.starts_with("pwsh") {
        return format!(
            "Start-Process -NoNewWindow -Wait -FilePath {} -ArgumentList '--run-agent', '{token}'",
            powershell_quote(&path)
        );
    }
    if shell == "cmd" || shell == "cmd.exe" {
        if !path.contains(['%', '!', '\r', '\n', '"']) {
            return format!("start \"\" /b /wait \"{path}\" --run-agent {token}");
        }
        // Percent/! expansion cannot preserve every Windows path in cmd.
        // Only the fixed helper invocation is encoded, never the prompt.
        use base64::Engine;
        let script = format!(
            "Start-Process -NoNewWindow -Wait -FilePath {} -ArgumentList '--run-agent', '{token}'",
            powershell_quote(&path)
        );
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        return format!(
            "powershell.exe -NoLogo -NoProfile -EncodedCommand {}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    }
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.into_owned()
    };
    format!(
        "{} --run-agent {token}",
        crate::terminal::shell_quote::quote_path(&path)
    )
}

/// Called before GUI/config initialization. No arbitrary executable is accepted
/// from the command line or request: only a supported provider resolved in PATH.
pub fn run_from_cli(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Option<i32>> {
    let args: Vec<_> = args.into_iter().skip(1).collect();
    if args.first().is_none_or(|arg| arg != "--run-agent") {
        return Ok(None);
    }
    attach_parent_console();
    anyhow::ensure!(args.len() == 2, "Uso interno: --run-agent UUID");
    let token = args[1]
        .to_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| anyhow::anyhow!("Identificador de lanzamiento inválido"))?;
    let request = consume_request(&request_dir()?, token)?;
    let command = request
        .provider
        .launch_command()
        .filter(|_| {
            matches!(
                request.provider,
                AgentProvider::ClaudeCode
                    | AgentProvider::CodexCli
                    | AgentProvider::GeminiCli
                    | AgentProvider::OpenCode
            )
        })
        .ok_or_else(|| anyhow::anyhow!("Proveedor de lanzamiento no admitido"))?;
    let executable =
        super::agent_detect::resolve_in_path(command, &std::env::var("PATH").unwrap_or_default())
            .ok_or_else(|| anyhow::anyhow!("No se encontró {command} en PATH"))?;
    let command = native_provider_command(&executable)?;
    execute_request(request, command).map(Some)
}

fn execute_request(request: LaunchRequest, mut command: Command) -> anyhow::Result<i32> {
    append_prompt_args(&mut command, request.provider, &request.brief);
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn append_prompt_args(command: &mut Command, provider: AgentProvider, brief: &str) {
    match provider {
        AgentProvider::GeminiCli => {
            command.arg(format!("--prompt-interactive={brief}"));
        }
        AgentProvider::OpenCode => {
            command.arg(format!("--prompt={brief}"));
        }
        _ => {
            // A prompt beginning with `--` must remain positional text.
            command.arg("--").arg(brief);
        }
    }
}

fn native_provider_command(executable: &Path) -> anyhow::Result<Command> {
    #[cfg(windows)]
    if executable.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        // npm's cmd shim would reparse a multiline argument. Resolve its data
        // entry point and invoke node.exe directly, without evaluating the shim.
        let text = std::fs::read_to_string(executable)?;
        if let Some(entry) = npm_entry_point(&text) {
            let parent = executable
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Shim sin directorio"))?;
            let script = parent.join(entry);
            anyhow::ensure!(
                script.is_file(),
                "No existe el entry point del agente: {}",
                script.display()
            );
            let local_node = parent.join("node.exe");
            let node = if local_node.is_file() {
                local_node
            } else {
                super::agent_detect::resolve_in_path(
                    "node.exe",
                    &std::env::var("PATH").unwrap_or_default(),
                )
                .ok_or_else(|| anyhow::anyhow!("No se encontró node.exe"))?
            };
            let mut command = Command::new(node);
            command.arg(script);
            return Ok(command);
        }
    }
    Ok(Command::new(executable))
}

#[cfg(any(windows, test))]
fn npm_entry_point(shim: &str) -> Option<PathBuf> {
    if !shim.to_ascii_lowercase().contains("set \"_prog=node\"") {
        return None;
    }
    shim.lines()
        .filter(|line| line.contains("%*") && line.contains("\"%_prog%\""))
        .find_map(|line| {
            line.split('"').find_map(|part| {
                let entry = part
                    .strip_prefix("%dp0%\\")
                    .or_else(|| part.strip_prefix("%~dp0\\"))?;
                (entry.starts_with("node_modules\\")
                    && !entry.contains(['%', '!', '\r', '\n'])
                    && !entry
                        .split(['/', '\\'])
                        .any(|component| matches!(component, "." | "..")))
                .then(|| PathBuf::from(entry))
            })
        })
}

fn attach_parent_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        // SAFETY: ATTACH_PARENT_PROCESS is a documented sentinel, has no
        // pointer arguments, and failure leaves inherited redirected I/O intact.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("tc-native-launch-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn executable(&self) -> PathBuf {
            let directory = self.0.join("agent ' & tools");
            std::fs::create_dir_all(&directory).unwrap();
            let source = directory.join("fixture.rs");
            std::fs::write(
                &source,
                r#"
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    use std::io::Write;
    std::thread::sleep(std::time::Duration::from_millis(100));
    let path = std::env::var_os("TC_LAUNCH_CAPTURE").unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    for arg in std::env::args().skip(1) {
        file.write_all(&(arg.len() as u64).to_le_bytes()).unwrap();
        file.write_all(arg.as_bytes()).unwrap();
    }
}
"#,
            )
            .unwrap();
            let binary = directory.join(if cfg!(windows) {
                "fixture.exe"
            } else {
                "fixture"
            });
            let output = Command::new("rustc")
                .args(["--edition=2021", "--crate-name", "launch_fixture"])
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "fixture compile failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            binary
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn captured(path: &Path) -> Vec<String> {
        let bytes = std::fs::read(path).unwrap();
        let mut rest = bytes.as_slice();
        let mut args = Vec::new();
        while !rest.is_empty() {
            let count = u64::from_le_bytes(rest[..8].try_into().unwrap()) as usize;
            args.push(String::from_utf8(rest[8..8 + count].to_vec()).unwrap());
            rest = &rest[8 + count..];
        }
        args
    }

    #[test]
    fn private_request_round_trips_and_is_consumed_once() {
        let fixture = Fixture::new();
        let brief = "a'\" & echo NOT_A_COMMAND | < > ^ %PATH% !name! $(x) `x`\nsegunda línea 中文";
        let token = write_request(&fixture.0, AgentProvider::ClaudeCode, brief).unwrap();
        let path = fixture.0.join(format!("{token}.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let request = consume_request(&fixture.0, token).unwrap();
        assert_eq!(request.provider, AgentProvider::ClaudeCode);
        assert_eq!(request.brief, brief);
        assert!(!path.exists());
        assert!(consume_request(&fixture.0, token).is_err());
    }

    #[test]
    fn native_helper_delivers_metacharacters_as_one_argument_for_each_provider() {
        let fixture = Fixture::new();
        let executable = fixture.executable();
        let capture = fixture.0.join("captured.bin");
        let brief = "--version quotes '\" & echo NOT_A_COMMAND | < > ^ %PATH% !name! $(echo x) `x`\nsegunda línea 中文";
        for (provider, expected) in [
            (
                AgentProvider::ClaudeCode,
                vec!["--".to_owned(), brief.to_owned()],
            ),
            (
                AgentProvider::CodexCli,
                vec!["--".to_owned(), brief.to_owned()],
            ),
            (
                AgentProvider::GeminiCli,
                vec![format!("--prompt-interactive={brief}")],
            ),
            (AgentProvider::OpenCode, vec![format!("--prompt={brief}")]),
        ] {
            let token = write_request(&fixture.0, provider, brief).unwrap();
            let request = consume_request(&fixture.0, token).unwrap();
            let mut command = native_provider_command(&executable).unwrap();
            command.env("TC_LAUNCH_CAPTURE", &capture);
            assert_eq!(execute_request(request, command).unwrap(), 0);
            assert_eq!(captured(&capture), expected);
        }
    }

    #[test]
    fn stale_cleanup_only_removes_expired_uuid_requests() {
        let fixture = Fixture::new();
        let token = write_request(&fixture.0, AgentProvider::ClaudeCode, "old").unwrap();
        let request = fixture.0.join(format!("{token}.json"));
        let ordinary = fixture.0.join("keep.json");
        std::fs::write(&ordinary, "keep").unwrap();
        cleanup_stale(&fixture.0, SystemTime::now());
        assert!(request.exists());
        cleanup_stale(
            &fixture.0,
            SystemTime::now() + STALE_AFTER + Duration::from_secs(1),
        );
        assert!(!request.exists());
        assert!(ordinary.exists());
    }

    #[test]
    fn cancellation_retires_the_request_but_handoff_leaves_it_for_the_helper() {
        let fixture = Fixture::new();
        for handed_off in [false, true] {
            let token = write_request(&fixture.0, AgentProvider::ClaudeCode, "draft").unwrap();
            let path = fixture.0.join(format!("{token}.json"));
            let launch = PreparedAgentLaunch {
                command: String::new(),
                lease: Arc::new(Lease {
                    path: path.clone(),
                    handed_off: AtomicBool::new(false),
                }),
            };
            if handed_off {
                launch.hand_off();
            }
            drop(launch);
            if handed_off {
                assert!(path.exists());
            } else {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while path.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(!path.exists());
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn cmd_and_powershell_start_the_gui_helper_and_wait_with_only_an_opaque_token() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let executable = fixture.executable();
        let capture = fixture.0.join("shell-capture.bin");
        let token = Uuid::new_v4();
        for shell in ["cmd.exe", "powershell.exe"] {
            let mut command = Command::new(shell);
            if shell == "cmd.exe" {
                command.args(["/d", "/c"]);
            } else {
                command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            }
            command
                .arg(helper_command(&executable, token, shell))
                .env("TC_LAUNCH_CAPTURE", &capture)
                .creation_flags(0x08000000);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                captured(&capture),
                ["--run-agent".to_owned(), token.to_string()]
            );
            std::fs::remove_file(&capture).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn npm_shims_are_bypassed_so_multiline_prompts_never_reach_cmd() {
        let fixture = Fixture::new();
        let binary = fixture.executable();
        let node = fixture.0.join("node.exe");
        std::fs::copy(binary, &node).unwrap();
        let script = fixture.0.join("node_modules/agent/cli.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "fixture only").unwrap();
        let shim = fixture.0.join("claude.cmd");
        std::fs::write(&shim, "@echo off\r\nSET \"_prog=node\"\r\n\"%_prog%\" \"%dp0%\\node_modules\\agent\\cli.js\" %*\r\n").unwrap();
        let capture = fixture.0.join("npm-args.bin");
        let brief = "'\" & echo not a command\n%PATH% !foo!";
        let mut command = native_provider_command(&shim).unwrap();
        command.env("TC_LAUNCH_CAPTURE", &capture);
        assert_eq!(
            execute_request(
                LaunchRequest {
                    provider: AgentProvider::ClaudeCode,
                    brief: brief.to_owned()
                },
                command
            )
            .unwrap(),
            0
        );
        assert_eq!(
            captured(&capture),
            [
                script.to_string_lossy().into_owned(),
                "--".to_owned(),
                brief.to_owned()
            ]
        );
    }

    #[test]
    fn arbitrary_batch_text_is_not_mistaken_for_an_npm_entry_point() {
        assert!(npm_entry_point("echo \"%dp0%\\node_modules\\agent\\cli.js\" %*").is_none());
    }
}
