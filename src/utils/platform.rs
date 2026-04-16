use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;

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
