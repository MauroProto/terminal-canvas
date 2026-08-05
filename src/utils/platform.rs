use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

pub fn default_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())
    }
    #[cfg(not(target_os = "windows"))]
    {
        portable_pty::CommandBuilder::new_default_prog().get_shell()
    }
}

pub fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Carpeta de descargas del usuario, el destino menos sorpresivo para los
/// archivos que la app genera (exports). Si el SO no la reporta, cae al
/// directorio temporal en vez de escribir dentro del repo del usuario.
pub fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
}

#[allow(dead_code)]
pub fn open_in_file_manager(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(path);
        command
    };

    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Failed to open {}", path.display())
    }
}

/// Abre un archivo con la aplicación default del SO (el editor asociado al
/// tipo de archivo). Best-effort: devuelve Err si el SO rechaza el pedido.
pub fn open_path_external(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command.arg(path);
        command
    };

    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Failed to open {}", path.display())
    }
}

/// Notificación del sistema best-effort (macOS: osascript, Linux: notify-send).
/// El fallo es silencioso: nunca debe romper la app.
pub fn notify(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            message.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("notify-send")
            .arg(title)
            .arg(message)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // Sin CLI simple de notificaciones; no-op.
        let _ = (title, message);
    }
}

pub fn default_share_base_url(port: u16) -> Option<String> {
    let host = local_network_host().unwrap_or_else(|| "127.0.0.1".to_owned());
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    Some(format!("https://{host}:{port}"))
}

pub fn local_network_host() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    match ip {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        IpAddr::V6(v6) if !v6.is_loopback() => Some(v6.to_string()),
        _ => None,
    }
}

/// Bell sonoro best-effort con throttle (mínimo 150 ms entre sonidos): un
/// stream de bells no puede ametrallar el speaker. El fallo es silencioso;
/// el bell visual del borde del panel siempre acompaña.
pub fn play_bell_sound() {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    static LAST_AT_MS: AtomicI64 = AtomicI64::new(i64::MIN / 2);
    let epoch = *EPOCH.get_or_init(Instant::now);
    let now_ms = Instant::now().saturating_duration_since(epoch).as_millis() as i64;
    let last = LAST_AT_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last) < 150 {
        return;
    }
    if LAST_AT_MS
        .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    spawn_bell_player();
}

fn spawn_bell_player() {
    fn silence(command: &mut Command) -> &mut Command {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("afplay");
        command.arg("/System/Library/Sounds/Glass.aiff");
        let _ = silence(&mut command).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        const SOUND: &str = "/usr/share/sounds/freedesktop/stereo/bell.oga";
        for player in ["pw-play", "paplay", "aplay"] {
            let mut command = Command::new(player);
            command.arg(SOUND);
            if silence(&mut command).spawn().is_ok() {
                return;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", "[System.Console]::Beep(880, 120)"]);
        let _ = silence(&mut command).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::default_share_base_url;

    #[test]
    fn default_share_base_url_formats_ipv4_loopback() {
        let url = default_share_base_url(8787).expect("share url");
        assert!(url.starts_with("https://"));
        assert!(url.ends_with(":8787"));
    }
}
