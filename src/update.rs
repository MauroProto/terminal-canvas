use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str = "https://api.github.com/repos/owner/repo/releases/latest";
const REQUEST_TIMEOUT: u64 = 15;
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
            self.batch_window.min(FOCUSED_REPAINT_WINDOW)
        } else {
            self.batch_window
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateStatus {
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
            status: UpdateStatus::Checking,
        }
    }
}

pub struct UpdateChecker {
    state: Arc<Mutex<UpdateState>>,
}

impl UpdateChecker {
    pub fn new(ctx: &egui::Context) -> Self {
        let state = Arc::new(Mutex::new(UpdateState::default()));
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
            *state_clone.lock().unwrap() = next;
            ctx.request_repaint();
        });
        Self { state }
    }

    pub fn snapshot(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }
}

pub fn version_newer(latest: &str, current: &str) -> bool {
    let parse = |version: &str| -> Vec<u32> {
        version
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(current)
}

pub fn check_latest_release() -> Result<UpdateState, String> {
    let resp = minreq::get(RELEASES_URL)
        .with_header("User-Agent", "mi-terminal")
        .with_header("Accept", "application/vnd.github+json")
        .with_timeout(REQUEST_TIMEOUT)
        .send()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if resp.status_code != 200 {
        return Err(format!("GitHub API returned {}", resp.status_code));
    }

    let json: serde_json::Value =
        serde_json::from_str(resp.as_str().map_err(|e| format!("UTF-8 error: {e}"))?)
            .map_err(|e| format!("JSON parse failed: {e}"))?;

    let tag = json
        .get("tag_name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "No tag_name in response".to_owned())?;

    let latest = tag.strip_prefix('v').unwrap_or(tag);
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
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };

    assets.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?.to_lowercase();
        let url = asset
            .get("browser_download_url")
            .and_then(|value| value.as_str())
            .map(str::to_owned);

        if !name.contains(arch) || !name.contains("setup") {
            return None;
        }

        #[cfg(target_os = "windows")]
        {
            if name.ends_with(".exe") {
                return url;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if name.ends_with(".dmg") && (name.contains("darwin") || name.contains("apple")) {
                return url;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if name.contains("linux") && name.ends_with(".tar.gz") {
                return url;
            }
        }

        None
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
    let resp = minreq::get(url)
        .with_header("User-Agent", "mi-terminal")
        .with_timeout(REQUEST_TIMEOUT)
        .send()
        .ok()?;

    if resp.status_code != 200 {
        return None;
    }

    let text = resp.as_str().ok()?.trim().to_owned();
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

    use super::{checksum_string, verify_checksum, version_newer};

    #[test]
    fn version_comparison() {
        assert!(version_newer("1.3.0", "1.2.0"));
        assert!(!version_newer("1.2.0", "1.2.0"));
        assert!(!version_newer("1.1.9", "1.2.0"));
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

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mi-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
