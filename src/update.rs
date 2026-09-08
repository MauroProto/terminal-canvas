#![allow(dead_code)]

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str =
    "https://api.github.com/repos/MauroProto/terminal-canvas/releases/latest";
const REQUEST_TIMEOUT: u64 = 15;
const MAX_RELEASE_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const RELEASE_ASSET_PREFIX: &str =
    "https://github.com/MauroProto/terminal-canvas/releases/download/";
// La terminal enfocada tiene prioridad de repintado sobre el fondo: el
// usuario percibe el throughput del stream enfocado, así que su ventana es
// más corta (~60 fps) que la de fondo (~30 fps). Antes era al revés (80 ms
// foco vs 33 ms fondo) y el stream enfocado se veía a ~12 fps.
const FOCUSED_REPAINT_WINDOW: Duration = Duration::from_millis(16);

#[derive(Debug, Clone)]
pub struct RepaintPolicy {
    batch_window: Duration,
    pending_runtime_event: bool,
    focused_runtime_event: bool,
    last_repaint_at: Option<Instant>,
}

impl RepaintPolicy {
    pub fn new(batch_window: Duration) -> Self {
        Self {
            batch_window,
            pending_runtime_event: false,
            focused_runtime_event: false,
            last_repaint_at: None,
        }
    }

    pub fn note_runtime_event(&mut self) {
        self.pending_runtime_event = true;
    }

    pub fn note_focused_runtime_event(&mut self) {
        self.pending_runtime_event = true;
        self.focused_runtime_event = true;
    }

    pub fn should_repaint_now(&mut self) -> bool {
        self.should_repaint_now_at(Instant::now())
    }

    pub fn should_repaint_now_at(&mut self, now: Instant) -> bool {
        if !self.pending_runtime_event {
            return false;
        }

        let repaint_window = self.current_window();
        let ready = self
            .last_repaint_at
            .map(|last| now.saturating_duration_since(last) >= repaint_window)
            .unwrap_or(true);

        if ready {
            self.last_repaint_at = Some(now);
            self.pending_runtime_event = false;
            self.focused_runtime_event = false;
        }

        ready
    }

    pub fn next_repaint_delay(&self, now: Instant) -> Option<Duration> {
        if !self.pending_runtime_event {
            return None;
        }

        let repaint_window = self.current_window();
        let elapsed = self
            .last_repaint_at
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or(repaint_window);

        Some(repaint_window.saturating_sub(elapsed))
    }

    fn current_window(&self) -> Duration {
        if self.focused_runtime_event {
            FOCUSED_REPAINT_WINDOW
        } else {
            self.batch_window
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateStatus {
    Disabled,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Installing,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct UpdateState {
    pub latest_version: Option<String>,
    pub download_url: Option<String>,
    pub installer_path: Option<PathBuf>,
    pub status: UpdateStatus,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            latest_version: None,
            download_url: None,
            installer_path: None,
            status: UpdateStatus::Disabled,
        }
    }
}

pub struct UpdateChecker {
    state: Arc<Mutex<UpdateState>>,
}

impl UpdateChecker {
    /// Checker inerte para tests, previews y cualquier construcción que haya
    /// prometido no iniciar red ni workers de fondo.
    pub fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::default())),
        }
    }

    pub fn new(ctx: &egui::Context) -> Self {
        if update_checker_disabled() {
            return Self::disabled();
        }
        let state = Arc::new(Mutex::new(UpdateState::default()));
        if let Ok(mut current) = state.lock() {
            current.status = UpdateStatus::Checking;
        }
        let state_clone = Arc::clone(&state);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let next = match check_latest_release() {
                Ok(update) => update,
                Err(err) => UpdateState {
                    status: UpdateStatus::Error(err),
                    ..UpdateState::default()
                },
            };
            if let Ok(mut state) = state_clone.lock() {
                *state = next;
            }
            ctx.request_repaint();
        });
        Self { state }
    }

    pub fn snapshot(&self) -> UpdateState {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

fn update_checker_disabled() -> bool {
    std::env::var_os("TERMINALCANVAS_DISABLE_UPDATE_CHECK").is_some()
}

pub fn version_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let parts: Vec<_> = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then_some(parts)
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT))
        .user_agent("mi-terminal")
        .https_only(true)
        .build()
        .map_err(|e| format!("HTTP client build failed: {e}"))
}

pub fn check_latest_release() -> Result<UpdateState, String> {
    let resp = http_client()?
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!("GitHub API returned {}", resp.status().as_u16()));
    }

    if resp
        .content_length()
        .is_some_and(|len| len > MAX_RELEASE_METADATA_BYTES)
    {
        return Err("GitHub release metadata is too large".to_owned());
    }
    let mut body = Vec::new();
    resp.take(MAX_RELEASE_METADATA_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("Failed to read response body: {e}"))?;
    if body.len() as u64 > MAX_RELEASE_METADATA_BYTES {
        return Err("GitHub release metadata is too large".to_owned());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("JSON parse failed: {e}"))?;

    let tag = json
        .get("tag_name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "No tag_name in response".to_owned())?;

    let latest = tag.strip_prefix('v').unwrap_or(tag);
    if parse_version(latest).is_none() {
        return Err("GitHub release tag is not a valid version".to_owned());
    }
    let update_available = version_newer(latest, CURRENT_VERSION);
    let download_url = find_platform_asset(&json);

    Ok(UpdateState {
        latest_version: Some(latest.to_owned()),
        download_url,
        installer_path: None,
        status: if update_available {
            UpdateStatus::Available
        } else {
            UpdateStatus::UpToDate
        },
    })
}

pub fn find_platform_asset(json: &serde_json::Value) -> Option<String> {
    let assets = json.get("assets")?.as_array()?;
    #[cfg(target_os = "macos")]
    let expected_macos_name = format!(
        "terminalcanvas-{}.dmg",
        json.get("tag_name")?
            .as_str()?
            .strip_prefix('v')
            .unwrap_or(json.get("tag_name")?.as_str()?)
            .to_ascii_lowercase()
    );
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let arch_aliases: &[&str] = if cfg!(target_arch = "aarch64") {
        &["aarch64", "arm64"]
    } else {
        &["x86_64", "amd64"]
    };

    assets.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?.to_lowercase();
        let url = asset
            .get("browser_download_url")
            .and_then(|value| value.as_str())
            .filter(|url| allowed_release_asset_url(url))
            .map(str::to_owned);

        #[cfg(target_os = "windows")]
        {
            if name.ends_with(".exe")
                && (arch_aliases.iter().any(|arch| name.contains(arch)) || !name.contains("arm"))
            {
                return url;
            }
        }
        #[cfg(target_os = "macos")]
        {
            // El bundle oficial se llama `TerminalCanvas-<version>.dmg`; un
            // artefacto universal no lleva arquitectura en el nombre.
            if name == expected_macos_name {
                return url;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if name.contains("linux")
                && name.ends_with(".tar.gz")
                && arch_aliases.iter().any(|arch| name.contains(arch))
            {
                return url;
            }
        }

        None
    })
}

fn allowed_release_asset_url(raw_url: &str) -> bool {
    url::Url::parse(raw_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && raw_url.starts_with(RELEASE_ASSET_PREFIX)
    })
}

pub fn verify_checksum(file_path: &Path, expected_hash: &str) -> bool {
    let Ok(mut file) = std::fs::File::open(file_path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buf[..read]),
            Err(_) => return false,
        }
    }
    let computed = format!("{:x}", hasher.finalize());
    computed == expected_hash.trim().to_lowercase()
}

pub fn download_checksum(url: &str) -> Option<String> {
    if !allowed_release_asset_url(url) || !url.ends_with(".sha256") {
        return None;
    }
    let resp = http_client().ok()?.get(url).send().ok()?;

    if resp.status() != reqwest::StatusCode::OK {
        return None;
    }

    let text = resp.text().ok()?.trim().to_owned();
    let hash = text.split_whitespace().next()?;
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_lowercase())
    } else {
        None
    }
}

pub fn checksum_string(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{checksum_string, find_platform_asset, verify_checksum, version_newer};

    #[test]
    fn version_comparison() {
        assert!(version_newer("1.3.0", "1.2.0"));
        assert!(!version_newer("1.2.0", "1.2.0"));
        assert!(!version_newer("1.1.9", "1.2.0"));
        assert!(!version_newer("1.x.0", "1.2.0"));
        assert!(!version_newer("release", "1.2.0"));
    }

    #[test]
    fn checksum_verification_succeeds_for_matching_hash() {
        let dir = tempfile_dir();
        let path = dir.join("checksum.txt");
        std::fs::write(&path, b"terminal").unwrap();
        let hash = checksum_string(b"terminal");
        assert!(verify_checksum(&path, &hash));
    }

    #[test]
    fn checksum_verification_fails_for_wrong_hash() {
        let dir = tempfile_dir();
        let path = dir.join("checksum.txt");
        std::fs::write(&path, b"terminal").unwrap();
        assert!(!verify_checksum(
            &path,
            "0000000000000000000000000000000000000000000000000000000000000000"
        ));
    }

    #[test]
    fn checksum_verification_handles_missing_file() {
        assert!(!verify_checksum(
            Path::new("/definitely/missing/file"),
            "deadbeef"
        ));
    }

    #[test]
    fn configured_release_url_enables_update_checker() {
        assert!(super::RELEASES_URL.contains("MauroProto/terminal-canvas"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_selects_the_bundle_asset_published_by_the_release_script() {
        let release = serde_json::json!({
            "tag_name": "v1.3.0",
            "assets": [{
                "name": "TerminalCanvas-1.3.0.dmg",
                "browser_download_url": "https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-1.3.0.dmg"
            }]
        });
        assert_eq!(
            find_platform_asset(&release).as_deref(),
            Some("https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-1.3.0.dmg")
        );
    }

    #[test]
    fn release_assets_reject_plaintext_and_untrusted_hosts() {
        assert!(!super::allowed_release_asset_url(
            "http://github.com/example/release.dmg"
        ));
        assert!(!super::allowed_release_asset_url(
            "https://example.test/release.dmg"
        ));
        assert!(!super::allowed_release_asset_url(
            "https://github.com/example/release.dmg"
        ));
        assert!(!super::allowed_release_asset_url(
            "https://objects.githubusercontent.com/release.dmg"
        ));
        assert!(super::allowed_release_asset_url(
            "https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-1.3.0.dmg"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_rejects_an_asset_whose_version_does_not_match_the_release_tag() {
        let release = serde_json::json!({
            "tag_name": "v1.3.0",
            "assets": [{
                "name": "TerminalCanvas-9.9.9.dmg",
                "browser_download_url": "https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-9.9.9.dmg"
            }]
        });
        assert!(find_platform_asset(&release).is_none());
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mi-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
