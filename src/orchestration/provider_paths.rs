//! Provider-owned profiles are independent of the application's state profile.

use std::ffi::OsString;
use std::path::PathBuf;

pub(super) fn claude_config_dir() -> Option<PathBuf> {
    resolve_claude_config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        crate::utils::platform::home_dir(),
    )
}

pub(super) fn resolve_claude_config_dir(
    configured: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match configured.filter(|path| !path.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => home.map(|path| path.join(".claude")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_profile_does_not_require_a_home_directory() {
        let profile = std::env::temp_dir().join("Claude profile with spaces");
        assert_eq!(
            resolve_claude_config_dir(Some(profile.clone().into_os_string()), None),
            Some(profile)
        );
    }

    #[test]
    fn an_unset_or_empty_override_uses_the_provider_default() {
        let home = std::env::temp_dir().join("provider home");
        for configured in [None, Some(OsString::new())] {
            assert_eq!(
                resolve_claude_config_dir(configured, Some(home.clone())),
                Some(home.join(".claude"))
            );
        }
        assert_eq!(resolve_claude_config_dir(None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn profile_paths_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let configured = OsString::from_vec(b"/tmp/claude-\xff".to_vec());
        assert_eq!(
            resolve_claude_config_dir(Some(configured.clone()), None),
            Some(PathBuf::from(configured))
        );
    }
}
