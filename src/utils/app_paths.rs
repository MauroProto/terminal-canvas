//! One process-wide profile for all application state and bundled helpers.
//! The optional profile root never changes the user's home or agent settings.

use std::ffi::OsString;
use std::path::{Component, PathBuf};
use std::sync::OnceLock;

pub const PROFILE_ENV: &str = "TERMINAL_CANVAS_HOME";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub isolated_root: Option<PathBuf>,
}

impl AppPaths {
    fn resolve(override_root: Option<OsString>) -> anyhow::Result<Self> {
        if let Some(root) = override_root {
            let root = PathBuf::from(root);
            if !root.is_absolute()
                || !root
                    .components()
                    .any(|part| matches!(part, Component::Normal(_)))
                || root
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
            {
                anyhow::bail!("{PROFILE_ENV} must be an absolute profile directory without '..'");
            }
            return Ok(Self {
                config: root.join("config"),
                data: root.join("data"),
                cache: root.join("cache"),
                isolated_root: Some(root),
            });
        }
        let dirs = directories::ProjectDirs::from("", "", "terminal-app")
            .ok_or_else(|| anyhow::anyhow!("Cannot resolve application directories"))?;
        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            isolated_root: None,
        })
    }
}

/// Invalid overrides fail closed, including in helpers; they never fall back
/// to the user's normal profile. Resolve once so workers share the same root.
pub fn get() -> anyhow::Result<&'static AppPaths> {
    static PATHS: OnceLock<Result<AppPaths, String>> = OnceLock::new();
    PATHS
        .get_or_init(|| AppPaths::resolve(std::env::var_os(PROFILE_ENV)).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

pub fn data_dir() -> Option<PathBuf> {
    get().ok().map(|paths| paths.data.clone())
}

pub fn config_dir() -> Option<PathBuf> {
    get().ok().map(|paths| paths.config.clone())
}

pub fn panic_log_path() -> Option<PathBuf> {
    let paths = get().ok()?;
    if paths.isolated_root.is_some() {
        Some(paths.data.join("logs").join("panic.log"))
    } else {
        super::platform::home_dir().map(|home| super::platform::panic_log_path(&home))
    }
}

pub fn exports_dir() -> Option<PathBuf> {
    let paths = get().ok()?;
    Some(match &paths.isolated_root {
        Some(root) => root.join("exports"),
        None => super::platform::downloads_dir(),
    })
}

/// Isolated profiles must not rewrite a real agent's global hooks.
pub fn permits_global_agent_configuration() -> bool {
    get().is_ok_and(|paths| paths.isolated_root.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn all_isolated_directories_stay_under_the_chosen_profile() {
        let root = std::env::temp_dir().join("tc-profile with spaces");
        let paths = AppPaths::resolve(Some(root.as_os_str().to_owned())).unwrap();
        assert_eq!(paths.config, root.join("config"));
        assert_eq!(paths.data, root.join("data"));
        assert_eq!(paths.cache, root.join("cache"));
        assert_eq!(paths.isolated_root.as_deref(), Some(root.as_path()));
    }

    #[test]
    fn invalid_profiles_cannot_fall_back_to_real_user_data() {
        for root in [
            PathBuf::new(),
            PathBuf::from("relative"),
            std::env::temp_dir().join("../other"),
        ] {
            assert!(AppPaths::resolve(Some(root.into_os_string())).is_err());
        }
        let root = if cfg!(windows) {
            Path::new("C:\\")
        } else {
            Path::new("/")
        };
        assert!(AppPaths::resolve(Some(root.as_os_str().to_owned())).is_err());
    }

    #[test]
    fn default_profile_matches_existing_platform_directories() {
        let dirs = directories::ProjectDirs::from("", "", "terminal-app").unwrap();
        let paths = AppPaths::resolve(None).unwrap();
        assert_eq!(paths.config, dirs.config_dir());
        assert_eq!(paths.data, dirs.data_dir());
        assert_eq!(paths.cache, dirs.cache_dir());
        assert!(paths.isolated_root.is_none());
    }
}
