//! Real-process regressions for the private Windows verification Job.
//!
//! Fixtures communicate through a UUID directory and stay alive until the
//! supervisor has retained process handles. Cleanup runs only after the
//! assertions, so neither PID reuse nor a fixture guard can hide an escape.

use super::windows_command::{Command, Process};
use super::CommandCapture;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FIXTURE_MODE: &str = "TC_WINDOWS_VERIFY_FIXTURE_MODE";
const FIXTURE_MARKER: &str = "TC_WINDOWS_VERIFY_FIXTURE_MARKER";
const FIXTURE_TEST: &str = "update_install::windows_command_tests::subprocess_fixture";
const ROUNDTRIP_OVERRIDE: &str = "TC_WINDOWS_VERIFY_ROUNDTRIP_OVERRIDE";
const ROUNDTRIP_REMOVED: &str = "TC_WINDOWS_VERIFY_ROUNDTRIP_REMOVED";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_LIFETIME: Duration = Duration::from_secs(30);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> RawHandle;
    fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
    fn TerminateProcess(handle: RawHandle, exit_code: u32) -> i32;
    fn IsProcessInJob(process: RawHandle, job: RawHandle, result: *mut i32) -> i32;
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "windows-update-command-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn marker(&self) -> PathBuf {
        self.0.join("fixture")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture_command(mode: &str, marker: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
        .env(FIXTURE_MODE, mode)
        .env(FIXTURE_MARKER, marker);
    command
}

fn uncontained_fixture_command(mode: &str, marker: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
        .env(FIXTURE_MODE, mode)
        .env(FIXTURE_MARKER, marker)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; no additional test Job.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let started = Instant::now();
    while !path.is_file() {
        assert!(
            started.elapsed() < timeout,
            "Missing fixture signal: {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn marker_pid(path: &Path) -> u32 {
    let started = Instant::now();
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.parse() {
                return pid;
            }
        }
        assert!(
            started.elapsed() < HANDSHAKE_TIMEOUT,
            "The fixture did not publish a complete PID: {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn signal(marker: &Path, extension: &str) {
    std::fs::write(marker.with_extension(extension), []).unwrap();
}

fn write_path(path: &Path, value: &Path) {
    let bytes: Vec<u8> = value
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(path, bytes).unwrap();
}

fn read_path(path: &Path) -> PathBuf {
    let bytes = std::fs::read(path).unwrap();
    let (pairs, remainder) = bytes.as_chunks::<2>();
    assert!(remainder.is_empty());
    let wide: Vec<u16> = pairs
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    PathBuf::from(OsString::from_wide(&wide))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

fn roundtrip_fixture(marker: &Path) {
    let report = serde_json::json!({
        "argv": std::env::args_os().map(|argument| wide(&argument)).collect::<Vec<_>>(),
        "directory": wide(std::env::current_dir().unwrap().as_os_str()),
        "override": std::env::var_os(ROUNDTRIP_OVERRIDE).map(|value| wide(&value)),
        "removed": std::env::var_os(ROUNDTRIP_REMOVED).map(|value| wide(&value)),
        "system_root": std::env::var_os("SystemRoot").map(|value| wide(&value)),
    });
    std::fs::write(
        marker.with_extension("argv.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
}

/// The field order closes the Job/process before closing captures and removing
/// their directory. The Job handle is never duplicated into a test observer.
struct CapturedProcess {
    process: Process,
    _stdout: File,
    _stderr: File,
    capture: CommandCapture,
}

impl CapturedProcess {
    fn spawn(mode: &str, marker: &Path) -> Self {
        let capture = CommandCapture::new().unwrap();
        let stdout = capture.create_file("stdout").unwrap();
        let stderr = capture.create_file("stderr").unwrap();
        let process = fixture_command(mode, marker)
            .spawn_captured(&stdout, &stderr)
            .unwrap();
        Self {
            process,
            _stdout: stdout,
            _stderr: stderr,
            capture,
        }
    }
}

/// Owned handles are acquired while fixtures are confirmed alive. They remain
/// valid even after exit, and never accidentally target a reused PID.
struct ObservedProcess(OwnedHandle);

fn in_private_job(process: RawHandle, job: RawHandle) -> bool {
    assert!(!process.is_null());
    assert!(!job.is_null());
    let mut result = 0;
    // SAFETY: callers keep the exact process and private Job owners alive
    // through this query. Querying does not retain either handle.
    let queried = unsafe { IsProcessInJob(process, job, &mut result) };
    assert_ne!(queried, 0, "{}", std::io::Error::last_os_error());
    result != 0
}

impl ObservedProcess {
    fn open(pid: u32) -> Self {
        // SAFETY: the PID is published by our UUID-directory fixture. The
        // requested rights are query, synchronization, and failure cleanup.
        let handle = unsafe { OpenProcess(0x0010_1001, 0, pid) };
        assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
        // SAFETY: OpenProcess returned a valid owned handle, not yet wrapped
        // or closed by any other owner.
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn stopped(&self, milliseconds: u32) -> bool {
        // SAFETY: this observer owns a live process handle for the entire call.
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), milliseconds) } {
            0 => true,
            258 => false, // WAIT_TIMEOUT
            other => panic!(
                "Cannot wait for fixture ({other}): {}",
                std::io::Error::last_os_error()
            ),
        }
    }

    fn in_job(&self, job: RawHandle) -> bool {
        in_private_job(self.0.as_raw_handle(), job)
    }
}

impl Drop for ObservedProcess {
    fn drop(&mut self) {
        // SAFETY: cleanup targets this exact owned fixture handle, never a PID.
        // It runs after assertions, and covers failures without leaking helpers.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
            WaitForSingleObject(self.0.as_raw_handle(), 1_000);
        }
    }
}

struct ObservedTree {
    child: ObservedProcess,
    grandchild: ObservedProcess,
}

impl ObservedTree {
    fn ready(marker: &Path) -> Self {
        wait_for_file(&marker.with_extension("ready"), HANDSHAKE_TIMEOUT);
        let child = ObservedProcess::open(marker_pid(&marker.with_extension("child")));
        let grandchild = ObservedProcess::open(marker_pid(&marker.with_extension("grandchild")));
        let tree = Self { child, grandchild };
        tree.assert_alive();
        tree
    }

    fn assert_alive(&self) {
        assert!(!self.child.stopped(0), "The fixture child already exited");
        assert!(
            !self.grandchild.stopped(0),
            "The fixture grandchild already exited"
        );
    }

    fn assert_in_job(&self, job: RawHandle) {
        assert!(self.child.in_job(job), "Child is outside the private Job");
        assert!(
            self.grandchild.in_job(job),
            "Grandchild is outside the private Job"
        );
    }

    fn assert_stopped(&self) {
        assert!(
            self.child.stopped(5_000),
            "The fixture child survived cleanup"
        );
        assert!(
            self.grandchild.stopped(5_000),
            "The fixture grandchild survived cleanup"
        );
    }
}

struct BoundedWorker(Option<JoinHandle<anyhow::Result<Output>>>);

impl BoundedWorker {
    fn spawn(mode: &'static str, marker: &Path, timeout: Duration) -> Self {
        let marker = marker.to_owned();
        Self(Some(std::thread::spawn(move || {
            super::bounded_output(&mut fixture_command(mode, &marker), timeout)
        })))
    }

    fn finish(mut self) -> anyhow::Result<Output> {
        self.0.take().unwrap().join().unwrap()
    }
}

impl Drop for BoundedWorker {
    fn drop(&mut self) {
        if let Some(worker) = self.0.take() {
            let _ = worker.join();
        }
    }
}

struct FixtureOwner {
    child: Child,
    capture_directory: Option<PathBuf>,
}

impl FixtureOwner {
    fn spawn(marker: &Path) -> Self {
        let child = uncontained_fixture_command("owner-exit", marker)
            .spawn()
            .unwrap();
        Self {
            child,
            capture_directory: None,
        }
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                started.elapsed() < HANDSHAKE_TIMEOUT,
                "The owner did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for FixtureOwner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(directory) = self.capture_directory.as_ref() {
            // process::exit deliberately skipped capture Drop in the owner.
            // Closing its Job may finish asynchronously; retry cleanup briefly.
            for _ in 0..100 {
                if std::fs::remove_dir_all(directory).is_ok() || !directory.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// A normal test invocation is inert; recursive fixture invocations run exactly
/// this test and never require PowerShell, signatures, installers, or a GUI.
#[test]
fn subprocess_fixture() {
    let Some(mode) = std::env::var_os(FIXTURE_MODE) else {
        return;
    };
    let marker = PathBuf::from(std::env::var_os(FIXTURE_MARKER).unwrap());
    match mode.to_str().unwrap() {
        "exit-259" => {
            std::fs::write(
                marker.with_extension("child"),
                std::process::id().to_string(),
            )
            .unwrap();
            std::process::exit(259);
        }
        "roundtrip" => roundtrip_fixture(&marker),
        "suspended" => {
            std::fs::write(
                marker.with_extension("child"),
                std::process::id().to_string(),
            )
            .unwrap();
            wait_for_file(&marker.with_extension("release"), FIXTURE_LIFETIME);
        }
        "grandchild" => {
            std::fs::write(
                marker.with_extension("grandchild"),
                std::process::id().to_string(),
            )
            .unwrap();
            // Safety net if a regression prevents the supervisor from cleaning
            // up. Every termination assertion expires well before this delay.
            std::thread::sleep(FIXTURE_LIFETIME);
        }
        "tree-success" | "tree-deadline" | "tree-output" | "tree-wait" => {
            tree_fixture(mode.to_str().unwrap(), &marker);
        }
        "owner-exit" => owner_exit_fixture(&marker),
        other => panic!("Unknown Windows verification fixture mode: {other}"),
    }
}

fn tree_fixture(mode: &str, marker: &Path) {
    // Spawn the grandchild immediately, before publishing the child's PID or
    // any readiness signal. Its std::process launch inherits the private Job.
    // This Windows-only fixture must leave the grandchild running when its
    // parent exits. Waiting here would mask a missing production Job cleanup;
    // the supervisor owns a process handle and a bounded failure cleanup.
    #[allow(clippy::zombie_processes)]
    let grandchild = uncontained_fixture_command("grandchild", marker)
        .spawn()
        .unwrap();
    assert_eq!(
        marker_pid(&marker.with_extension("grandchild")),
        grandchild.id()
    );
    std::fs::write(
        marker.with_extension("child"),
        std::process::id().to_string(),
    )
    .unwrap();
    signal(marker, "ready");
    wait_for_file(&marker.with_extension("release"), FIXTURE_LIFETIME);
    match mode {
        "tree-success" => {
            println!("tree_stdout");
            eprintln!("tree_stderr");
            // Child::drop only closes its process handle. Do not terminate the
            // grandchild: the production verification Job must do that.
        }
        "tree-output" => {
            let line = "x".repeat(1024);
            for _ in 0..1024 {
                println!("{line}");
                eprintln!("{line}");
            }
            std::thread::sleep(FIXTURE_LIFETIME);
        }
        "tree-deadline" | "tree-wait" => std::thread::sleep(FIXTURE_LIFETIME),
        _ => unreachable!(),
    }
}

fn owner_exit_fixture(marker: &Path) -> ! {
    let captured = CapturedProcess::spawn("tree-wait", marker);
    wait_for_file(&marker.with_extension("ready"), HANDSHAKE_TIMEOUT);
    write_path(
        &marker.with_extension("capture"),
        &captured.capture.directory,
    );
    signal(marker, "owner-ready");
    wait_for_file(&marker.with_extension("owner-exit"), FIXTURE_LIFETIME);
    // Rust destructors never run. Only OS closure of the owner's private Job
    // can terminate the assigned child and its already-running grandchild.
    std::process::exit(0);
}

#[test]
fn suspended_process_is_in_its_exact_private_job_before_executing_fixture_code() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let capture = CommandCapture::new().unwrap();
    let capture_directory = capture.directory.clone();
    let stdout = capture.create_file("stdout").unwrap();
    let stderr = capture.create_file("stderr").unwrap();
    let suspended = fixture_command("suspended", &marker)
        .spawn_suspended_for_test(&stdout, &stderr)
        .unwrap();
    assert!(suspended.is_in_job_for_test().unwrap());
    assert!(!marker.with_extension("child").exists());
    let captured = CapturedProcess {
        process: suspended.resume().unwrap(),
        _stdout: stdout,
        _stderr: stderr,
        capture,
    };
    assert_eq!(
        marker_pid(&marker.with_extension("child")),
        captured.process.id()
    );
    let child = ObservedProcess::open(captured.process.id());
    assert!(!child.stopped(0));
    assert!(in_private_job(
        captured.process.process_handle(),
        captured.process.job_handle()
    ));
    assert!(child.in_job(captured.process.job_handle()));
    drop(captured);
    assert!(child.stopped(5_000));
    assert!(!capture_directory.exists());
}

#[test]
fn normal_completion_stops_the_immediate_grandchild_and_captures_both_streams() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let worker = BoundedWorker::spawn("tree-success", &marker, Duration::from_secs(20));
    let tree = ObservedTree::ready(&marker);
    signal(&marker, "release");
    let output = worker.finish().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("tree_stdout"));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("tree_stderr"));
    tree.assert_stopped();
}

#[test]
fn abandoning_a_suspended_launch_stops_it_without_executing_fixture_code() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let capture = CommandCapture::new().unwrap();
    let capture_directory = capture.directory.clone();
    let stdout = capture.create_file("stdout").unwrap();
    let stderr = capture.create_file("stderr").unwrap();
    let suspended = fixture_command("suspended", &marker)
        .spawn_suspended_for_test(&stdout, &stderr)
        .unwrap();
    assert!(suspended.is_in_job_for_test().unwrap());
    let child = ObservedProcess::open(suspended.id_for_test());
    assert!(!child.stopped(0));
    drop(suspended);
    assert!(child.stopped(5_000));
    assert!(!marker.with_extension("child").exists());
    drop(stdout);
    drop(stderr);
    drop(capture);
    assert!(!capture_directory.exists());
}

#[test]
fn a_missing_working_directory_fails_without_executing_the_fixture() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let capture = CommandCapture::new().unwrap();
    let capture_directory = capture.directory.clone();
    let stdout = capture.create_file("stdout").unwrap();
    let stderr = capture.create_file("stderr").unwrap();
    let mut command = fixture_command("suspended", &marker);
    command.current_dir(directory.0.join("absent directory Mauro ñ 東京"));
    assert!(command.spawn_captured(&stdout, &stderr).is_err());
    assert!(!marker.with_extension("child").exists());
    drop(stdout);
    drop(stderr);
    drop(capture);
    assert!(!capture_directory.exists());
}

#[test]
fn deadline_stops_both_the_child_and_its_immediate_grandchild() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let started = Instant::now();
    let worker = BoundedWorker::spawn("tree-deadline", &marker, Duration::from_secs(8));
    let tree = ObservedTree::ready(&marker);
    signal(&marker, "release");
    let error = worker.finish().unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(20));
    tree.assert_stopped();
}

#[test]
fn output_limit_stops_both_the_child_and_its_immediate_grandchild() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let started = Instant::now();
    let worker = BoundedWorker::spawn("tree-output", &marker, Duration::from_secs(20));
    let tree = ObservedTree::ready(&marker);
    signal(&marker, "release");
    let error = worker.finish().unwrap_err();
    assert!(error.to_string().contains("output limit"));
    assert!(started.elapsed() < Duration::from_secs(15));
    tree.assert_stopped();
}

#[test]
fn dropping_the_command_guard_stops_the_whole_tree_and_removes_its_capture() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let captured = CapturedProcess::spawn("tree-wait", &marker);
    let capture_directory = captured.capture.directory.clone();
    let tree = ObservedTree::ready(&marker);
    tree.assert_in_job(captured.process.job_handle());
    drop(captured);
    tree.assert_stopped();
    assert!(!capture_directory.exists());
}

#[test]
fn closing_the_last_job_handle_without_explicit_termination_stops_the_whole_tree() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let mut captured = CapturedProcess::spawn("tree-wait", &marker);
    let tree = ObservedTree::ready(&marker);
    tree.assert_in_job(captured.process.job_handle());
    captured
        .process
        .close_job_without_terminate_for_test()
        .unwrap();
    // The command guard is still alive. Its destructor cannot mask a missing
    // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE policy.
    tree.assert_stopped();
    assert!(captured.process.try_wait().unwrap().is_some());
}

#[test]
fn owner_process_exit_without_drop_stops_its_child_and_immediate_grandchild() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let mut owner = FixtureOwner::spawn(&marker);
    wait_for_file(&marker.with_extension("owner-ready"), HANDSHAKE_TIMEOUT);
    owner.capture_directory = Some(read_path(&marker.with_extension("capture")));
    let tree = ObservedTree::ready(&marker);
    signal(&marker, "owner-exit");
    assert!(owner.wait().success());
    // The owner used plain std::process::Command; no additional test Job can
    // kill the tree on its behalf. Observers retain process handles only.
    tree.assert_stopped();
}

#[test]
fn missing_executable_fails_without_running_the_fixture_or_retaining_capture_files() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let capture = CommandCapture::new().unwrap();
    let capture_directory = capture.directory.clone();
    let stdout = capture.create_file("stdout").unwrap();
    let stderr = capture.create_file("stderr").unwrap();
    let mut command = Command::new(directory.0.join("missing-verification-fixture.exe"));
    command
        .env(FIXTURE_MODE, "suspended")
        .env(FIXTURE_MARKER, &marker);
    assert!(command.spawn_captured(&stdout, &stderr).is_err());
    assert!(!marker.with_extension("child").exists());
    assert_eq!(stdout.metadata().unwrap().len(), 0);
    assert_eq!(stderr.metadata().unwrap().len(), 0);
    drop(stdout);
    drop(stderr);
    drop(capture);
    assert!(!capture_directory.exists());
}

#[test]
fn completed_process_can_return_the_still_active_numeric_exit_code() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let output = super::bounded_output(
        &mut fixture_command("exit-259", &marker),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(259));
    assert!(!output.status.success());
    assert!(marker.with_extension("child").is_file());
}

#[test]
fn real_powershell_verification_preserves_utf8_environment_and_system_directory() {
    let value = "Mauro ñ 東京 🦀: \"comillas\", $() y barras\\";
    let mut command = super::windows_system_command(
        r#"[pscustomobject]@{
            value = $env:TC_WINDOWS_VERIFY_POWERSHELL_VALUE
            directory = [Environment]::CurrentDirectory
        } | ConvertTo-Json -Compress"#,
    )
    .unwrap();
    command.env("TC_WINDOWS_VERIFY_POWERSHELL_VALUE", value);
    let output = super::bounded_output(&mut command, Duration::from_secs(15)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["value"], value);
    let system_directory = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let actual_directory = Path::new(report["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(actual_directory).unwrap(),
        std::fs::canonicalize(system_directory).unwrap()
    );
}

#[test]
fn real_crt_preserves_arguments_unicode_directory_and_case_insensitive_environment_edits() {
    let directory = TestDirectory::new();
    let marker = directory.marker();
    let working_directory = directory.0.join("Directorio Mauro ñ 東京 🦀 con espacios");
    std::fs::create_dir(&working_directory).unwrap();
    let inherited_system_root = std::env::var_os("SystemRoot").unwrap();
    let overridden = OsString::from("valor reemplazado ñ 東京 🦀");
    let arguments = vec![
        OsString::new(),
        OsString::from("argumento ñ 東京 🦀"),
        OsString::from("con espacios"),
        OsString::from(r#"comillas "dobles" y \barras\"#),
        OsString::from(r"trailing\"),
        OsString::from(r"two trailing\\"),
        OsString::from(r#"backslashes before quote \\"quoted""#),
        OsString::from("--literal-option"),
        OsString::from("$() `literal` & ;"),
    ];
    let mut command = fixture_command("roundtrip", &marker);
    command
        .arg("--")
        .args(&arguments)
        .current_dir(&working_directory)
        .env("tc_windows_verify_roundtrip_override", "obsolete")
        .env(ROUNDTRIP_OVERRIDE, &overridden)
        .env("tc_windows_verify_roundtrip_removed", "remove-me")
        .env_remove(ROUNDTRIP_REMOVED);
    let output = super::bounded_output(&mut command, Duration::from_secs(10)).unwrap();
    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(marker.with_extension("argv.json")).unwrap())
            .unwrap();
    let mut expected_arguments = vec![
        std::env::current_exe().unwrap().into_os_string(),
        OsString::from("--exact"),
        OsString::from(FIXTURE_TEST),
        OsString::from("--nocapture"),
        OsString::from("--test-threads=1"),
        OsString::from("--"),
    ];
    expected_arguments.extend(arguments);
    assert_eq!(
        report["argv"],
        serde_json::json!(expected_arguments
            .iter()
            .map(|argument| wide(argument))
            .collect::<Vec<_>>())
    );
    assert_eq!(
        report["directory"],
        serde_json::json!(wide(working_directory.as_os_str()))
    );
    assert_eq!(report["override"], serde_json::json!(wide(&overridden)));
    assert!(report["removed"].is_null());
    assert_eq!(
        report["system_root"],
        serde_json::json!(wide(&inherited_system_root))
    );
}
