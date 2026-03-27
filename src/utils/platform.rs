use std::path::PathBuf;

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
