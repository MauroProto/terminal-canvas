use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
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

/// Cosechador de procesos auxiliares.
///
/// `std::process::Child` **no** espera al proceso hijo cuando se suelta, así
/// que todo proceso lanzado y olvidado queda como zombi en la tabla de
/// procesos hasta que la app termina. En una sesión larga con muchas campanas
/// o notificaciones eso se acumula. Un único hilo los espera a todos; una
/// alternativa por-proceso costaría un hilo por campana.
fn reaper() -> &'static Mutex<std::sync::mpsc::Sender<(String, Child)>> {
    static REAPER: OnceLock<Mutex<std::sync::mpsc::Sender<(String, Child)>>> = OnceLock::new();
    REAPER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(String, Child)>();
        // Si el hilo no arranca, el canal igual acepta envíos y los procesos
        // simplemente no se cosechan: peor, pero nunca un fallo para el que llama.
        let _ = std::thread::Builder::new()
            .name("process-reaper".to_owned())
            .spawn(move || {
                while let Ok((label, mut child)) = rx.recv() {
                    match child.wait() {
                        Ok(status) if !status.success() => {
                            log::debug!("{label} terminó con estado {status}");
                        }
                        Err(err) => log::debug!("no se pudo esperar a {label}: {err}"),
                        _ => {}
                    }
                }
            });
        Mutex::new(tx)
    })
}

/// Lanza un proceso auxiliar **sin bloquear** al que llama y lo deja a cargo
/// del cosechador.
///
/// Antes esto usaba `Command::status()`, que espera a que el hijo termine. Al
/// llamarse desde el hilo de UI (abrir un archivo en el editor, Cmd+click en
/// un enlace) la ventana se congelaba mientras el SO levantaba la aplicación
/// destino, que puede tardar segundos.
///
/// Sólo se reporta el fallo al lanzar (comando inexistente); el código de
/// salida se registra en el log porque para "abrir con la app default" no hay
/// nada útil que hacer con él.
pub fn spawn_detached(label: &str, command: &mut Command) -> anyhow::Result<()> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    match reaper().lock() {
        Ok(tx) => {
            let _ = tx.send((label.to_owned(), child));
        }
        // Mutex envenenado: preferimos filtrar un zombi antes que entrar en
        // pánico por no poder abrir un archivo.
        Err(_) => log::debug!("reaper no disponible; {label} no será cosechado"),
    }
    Ok(())
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

    spawn_detached("open", &mut command)
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

    spawn_detached("open", &mut command)
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
        let mut command = Command::new("osascript");
        command.arg("-e").arg(script);
        let _ = spawn_detached("osascript", &mut command);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("notify-send");
        command.arg(title).arg(message);
        let _ = spawn_detached("notify-send", &mut command);
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
    // Los descriptores los silencia `spawn_detached`.
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("afplay");
        command.arg("/System/Library/Sounds/Glass.aiff");
        let _ = spawn_detached("afplay", &mut command);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        const SOUND: &str = "/usr/share/sounds/freedesktop/stereo/bell.oga";
        for player in ["pw-play", "paplay", "aplay"] {
            let mut command = Command::new(player);
            command.arg(SOUND);
            if spawn_detached(player, &mut command).is_ok() {
                return;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-Command", "[System.Console]::Beep(880, 120)"]);
        let _ = spawn_detached("powershell", &mut command);
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
    #[test]
    #[cfg(unix)]
    fn spawn_detached_returns_immediately_instead_of_waiting() {
        // Regresión: antes se usaba Command::status(), que espera al hijo. Si
        // esto vuelve a bloquear, abrir un archivo congela la ventana.
        let mut command = std::process::Command::new("sleep");
        command.arg("1");

        let started = std::time::Instant::now();
        super::spawn_detached("sleep-test", &mut command).expect("spawn");
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "spawn_detached blocked for {elapsed:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn spawned_children_are_reaped_and_do_not_become_zombies() {
        // Un Child soltado sin wait() queda zombi hasta que muere el proceso.
        let pid = std::process::id();
        let mut command = std::process::Command::new("true");
        super::spawn_detached("true-test", &mut command).expect("spawn");

        // El cosechador es asíncrono: le damos margen.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let out = std::process::Command::new("ps")
                .args(["-axo", "ppid=,stat="])
                .output()
                .expect("ps");
            let listing = String::from_utf8_lossy(&out.stdout);
            let zombies = listing
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    let ppid = parts.next()?;
                    let stat = parts.next()?;
                    (ppid == pid.to_string() && stat.starts_with('Z')).then_some(())
                })
                .count();
            if zombies == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child was never reaped: {zombies} zombie(s) left"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn spawn_detached_reports_a_missing_binary() {
        let mut command = std::process::Command::new("no-such-binary-cbf1f0");
        assert!(super::spawn_detached("missing", &mut command).is_err());
    }
}
