#![allow(dead_code)]

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str =
    "https://api.github.com/repos/MauroProto/terminal-canvas/releases/latest";
const REQUEST_TIMEOUT: u64 = 15;
const MAX_RELEASE_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: usize = 1024;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
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
    PreparedInstallation,
    Installed,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct UpdateState {
    pub latest_version: Option<String>,
    pub download_url: Option<String>,
    pub installer_path: Option<PathBuf>,
    pub verified_hash: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub cancellation_requested: bool,
    pub install_available: bool,
    pub status: UpdateStatus,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            latest_version: None,
            download_url: None,
            installer_path: None,
            verified_hash: None,
            downloaded_bytes: 0,
            total_bytes: None,
            cancellation_requested: false,
            install_available: false,
            status: UpdateStatus::Disabled,
        }
    }
}

pub struct UpdateChecker {
    state: Arc<Mutex<UpdateState>>,
    cancellation: Arc<AtomicBool>,
}

impl UpdateChecker {
    /// Checker inerte para tests, previews y cualquier construcción que haya
    /// prometido no iniciar red ni workers de fondo.
    pub fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::default())),
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn new(ctx: &egui::Context) -> Self {
        if update_checker_disabled() {
            return Self::disabled();
        }
        let checker = Self::disabled();
        checker.check_again(ctx);
        checker
    }

    pub fn check_again(&self, ctx: &egui::Context) {
        if update_checker_disabled() {
            return;
        }
        let Ok(mut current) = self.state.lock() else {
            return;
        };
        if matches!(
            current.status,
            UpdateStatus::Checking
                | UpdateStatus::Downloading
                | UpdateStatus::Installing
                | UpdateStatus::PreparedInstallation
        ) {
            return;
        }
        current.status = UpdateStatus::Checking;
        if let Some(previous) = current.installer_path.take() {
            remove_staged_download(&previous);
        }
        drop(current);
        let state_clone = Arc::clone(&self.state);
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
    }

    pub fn snapshot(&self) -> UpdateState {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Downloads only the exact official platform asset, verifies its sidecar,
    /// and publishes the completed file only after all checks succeed.
    pub fn download(&self, ctx: &egui::Context) {
        let Ok(mut current) = self.state.lock() else {
            return;
        };
        if !matches!(
            current.status,
            UpdateStatus::Available | UpdateStatus::Error(_)
        ) {
            return;
        }
        let (Some(url), Some(version)) =
            (current.download_url.clone(), current.latest_version.clone())
        else {
            return;
        };
        if !version_newer(&version, CURRENT_VERSION) || !platform_asset_matches(&url, &version) {
            current.status = UpdateStatus::Error(
                "The release does not match this version and platform".to_owned(),
            );
            return;
        }
        self.cancellation.store(false, Ordering::Release);
        current.status = UpdateStatus::Downloading;
        current.downloaded_bytes = 0;
        current.total_bytes = None;
        current.cancellation_requested = false;
        if let Some(previous) = current.installer_path.as_deref() {
            remove_staged_download(previous);
        }
        current.installer_path = None;
        current.verified_hash = None;
        current.install_available = false;
        drop(current);
        let state = Arc::clone(&self.state);
        let cancellation = Arc::clone(&self.cancellation);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = download_release_asset(&url, &state, &cancellation, &ctx);
            if let Ok(mut current) = state.lock() {
                current.cancellation_requested = false;
                match result {
                    Ok((path, hash)) => {
                        if cancellation.load(Ordering::Acquire) {
                            if let Some(directory) = path.parent() {
                                let _ = std::fs::remove_dir_all(directory);
                            }
                            current.status = UpdateStatus::Available;
                            current.downloaded_bytes = 0;
                            current.total_bytes = None;
                            ctx.request_repaint();
                            return;
                        }
                        current.install_available = cfg!(target_os = "macos")
                            && crate::update_install::automatic_install_supported()
                            || cfg!(target_os = "windows")
                                && path.to_string_lossy().ends_with("-setup.exe");
                        current.installer_path = Some(path);
                        current.verified_hash = Some(hash);
                        current.status = UpdateStatus::Ready;
                    }
                    Err(_) if cancellation.load(Ordering::Acquire) => {
                        current.status = UpdateStatus::Available;
                        current.downloaded_bytes = 0;
                        current.total_bytes = None;
                    }
                    Err(error) => current.status = UpdateStatus::Error(error),
                }
            }
            ctx.request_repaint();
        });
    }

    pub fn cancel_download(&self) {
        if let Ok(mut current) = self.state.lock() {
            if matches!(current.status, UpdateStatus::Downloading) {
                current.cancellation_requested = true;
                self.cancellation.store(true, Ordering::Release);
            }
        }
    }

    /// The caller refuses installation while terminal sessions are still live.
    /// Installation revalidates both the staged bytes and publisher identity.
    pub fn install(&self, ctx: &egui::Context) {
        let Ok(mut current) = self.state.lock() else {
            return;
        };
        if !current.install_available
            || !matches!(current.status, UpdateStatus::Ready | UpdateStatus::Error(_))
        {
            return;
        }
        let (Some(path), Some(hash), Some(version)) = (
            current.installer_path.clone(),
            current.verified_hash.clone(),
            current.latest_version.clone(),
        ) else {
            return;
        };
        current.status = UpdateStatus::Installing;
        drop(current);
        let state = Arc::clone(&self.state);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = if verify_checksum(&path, &hash) {
                crate::update_install::prepare_or_install_verified_update(&path, &version)
                    .map_err(|error| error.to_string())
            } else {
                Err(
                    "The downloaded update changed after verification; download it again"
                        .to_owned(),
                )
            };
            if let Ok(mut current) = state.lock() {
                current.status = match result {
                    Ok(()) if cfg!(target_os = "windows") => UpdateStatus::PreparedInstallation,
                    Ok(()) => UpdateStatus::Installed,
                    Err(error) => UpdateStatus::Error(error),
                };
            }
            ctx.request_repaint();
        });
    }

    pub fn refuse_prepared_installation(&self, error: String) {
        if let Ok(mut current) = self.state.lock() {
            if matches!(current.status, UpdateStatus::PreparedInstallation) {
                current.status = UpdateStatus::Error(error);
            }
        }
    }

    pub fn launch_prepared_installation(&self, ctx: &egui::Context) -> Result<(), String> {
        let current = self.snapshot();
        if !matches!(current.status, UpdateStatus::PreparedInstallation) {
            return Err("The installer has not been prepared and verified".to_owned());
        }
        let path = current
            .installer_path
            .ok_or_else(|| "The prepared installer is missing".to_owned())?;
        let hash = current
            .verified_hash
            .ok_or_else(|| "The prepared checksum is missing".to_owned())?;
        let version = current
            .latest_version
            .ok_or_else(|| "The prepared version is missing".to_owned())?;
        if let Ok(mut current) = self.state.lock() {
            current.status = UpdateStatus::Installing;
        }
        let state = Arc::clone(&self.state);
        let ctx = ctx.clone();
        thread::spawn(move || {
            match crate::update_install::launch_verified_windows_installer(&path, &version, &hash) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => {
                    if let Ok(mut current) = state.lock() {
                        current.status = UpdateStatus::Error(error.to_string());
                    }
                }
            }
            ctx.request_repaint();
        });
        Ok(())
    }
}

impl Drop for UpdateChecker {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
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

fn parse_version(version: &str) -> Option<[u64; 3]> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let parts: Vec<_> = version
        .split('.')
        .map(|part| {
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                None
            } else {
                part.parse::<u64>().ok()
            }
        })
        .collect::<Option<_>>()?;
    parts.try_into().ok()
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT))
        .user_agent("mi-terminal")
        .https_only(true)
        .redirect(release_redirect_policy())
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
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(UpdateState {
                status: UpdateStatus::UpToDate,
                ..UpdateState::default()
            });
        }
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

    if json.get("draft").and_then(serde_json::Value::as_bool) == Some(true)
        || json.get("prerelease").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return Err("The latest release is not a published stable release".to_owned());
    }
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
        ..UpdateState::default()
    })
}

pub fn find_platform_asset(json: &serde_json::Value) -> Option<String> {
    if cfg!(target_os = "windows") && crate::update_install::automatic_install_supported() {
        if let Some(url) = find_windows_installer_asset(json, std::env::consts::ARCH) {
            return Some(url);
        }
    }
    find_platform_asset_for(json, std::env::consts::OS, std::env::consts::ARCH)
}

fn find_windows_installer_asset(json: &serde_json::Value, arch: &str) -> Option<String> {
    if arch != "x86_64" {
        return None;
    }
    let tag = json.get("tag_name")?.as_str()?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    parse_version(version)?;
    let name = format!("TerminalCanvas-{version}-windows-{arch}-setup.exe");
    let expected = format!("{RELEASE_ASSET_PREFIX}{tag}/{name}");
    let assets = json.get("assets")?.as_array()?;
    let matches = |name: &str, url: &str| {
        assets.iter().any(|asset| {
            asset.get("name").and_then(serde_json::Value::as_str) == Some(name)
                && asset
                    .get("browser_download_url")
                    .and_then(serde_json::Value::as_str)
                    == Some(url)
        })
    };
    (matches(&name, &expected) && matches(&format!("{name}.sha256"), &format!("{expected}.sha256")))
        .then_some(expected)
}

fn find_platform_asset_for(json: &serde_json::Value, os: &str, arch: &str) -> Option<String> {
    let extension = match os {
        "windows" => "zip",
        "macos" => "dmg",
        "linux" => "tar.gz",
        _ => return None,
    };
    if !matches!(arch, "x86_64" | "aarch64") {
        return None;
    }
    let tag = json.get("tag_name")?.as_str()?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    // Release artifacts have an explicit OS/architecture and exact tag version.
    // Unlabelled old DMGs were native builds, never proven universal binaries.
    if parse_version(version).is_none()
        || !version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
    {
        return None;
    }
    let expected_name = format!("TerminalCanvas-{version}-{os}-{arch}.{extension}");
    let expected_url = format!("{RELEASE_ASSET_PREFIX}{tag}/{expected_name}");
    json.get("assets")?.as_array()?.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?;
        let url = asset.get("browser_download_url")?.as_str()?;
        let checksum_name = format!("{expected_name}.sha256");
        let checksum_url = format!("{expected_url}.sha256");
        let has_checksum = json.get("assets")?.as_array()?.iter().any(|sidecar| {
            sidecar.get("name").and_then(serde_json::Value::as_str) == Some(checksum_name.as_str())
                && sidecar
                    .get("browser_download_url")
                    .and_then(serde_json::Value::as_str)
                    == Some(checksum_url.as_str())
        });
        (name == expected_name
            && url == expected_url
            && allowed_release_asset_url(url)
            && has_checksum)
            .then(|| url.to_owned())
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
            && url
                .path()
                .starts_with("/MauroProto/terminal-canvas/releases/download/")
            && raw_url.starts_with(RELEASE_ASSET_PREFIX)
    })
}

fn release_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let url = attempt.url();
        if attempt.previous().len() >= 5
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(
                url.host_str(),
                Some(
                    "github.com"
                        | "api.github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            )
        {
            attempt.error("Update redirect left the official HTTPS release hosts")
        } else {
            attempt.follow()
        }
    })
}

fn platform_asset_matches(url: &str, version: &str) -> bool {
    let extension = match std::env::consts::OS {
        "windows" => "zip",
        "macos" => "dmg",
        "linux" => "tar.gz",
        _ => return false,
    };
    if parse_version(version).is_none() || !allowed_release_asset_url(url) {
        return false;
    }
    let mut names = vec![format!(
        "TerminalCanvas-{version}-{}-{}.{extension}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    if cfg!(target_os = "windows") && std::env::consts::ARCH == "x86_64" {
        names.push(format!("TerminalCanvas-{version}-windows-x86_64-setup.exe"));
    }
    names.iter().any(|name| {
        [version.to_owned(), format!("v{version}")]
            .iter()
            .any(|tag| url == format!("{RELEASE_ASSET_PREFIX}{tag}/{name}"))
    })
}

fn parse_checksum_manifest(text: &str, expected_name: &str) -> Option<String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let mut fields = line.split_whitespace();
    let hash = fields.next()?;
    let filename = fields.next()?;
    let filename = filename.strip_prefix('*').unwrap_or(filename);
    if fields.next().is_some()
        || filename != expected_name
        || hash.len() != 64
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(hash.to_ascii_lowercase())
}

fn remove_staged_download(path: &Path) {
    let Ok(paths) = crate::utils::app_paths::get() else {
        return;
    };
    let root = paths.cache.join("updates");
    let Some(directory) = path.parent() else {
        return;
    };
    if directory.parent() != Some(root.as_path())
        || !directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
    {
        return;
    }
    let (Ok(root), Ok(directory)) = (
        std::fs::canonicalize(&root),
        std::fs::canonicalize(directory),
    ) else {
        return;
    };
    if directory.parent() == Some(root.as_path()) {
        let _ = std::fs::remove_dir_all(directory);
    }
}

struct DownloadDirectory {
    path: PathBuf,
    keep: bool,
}

impl DownloadDirectory {
    fn create() -> Result<Self, String> {
        let root = crate::utils::app_paths::get()
            .map_err(|error| format!("The update cache directory is unavailable: {error}"))?
            .cache
            .join("updates");
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("Cannot create update cache: {error}"))?;
        let path = root.join(uuid::Uuid::new_v4().to_string());
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder
            .create(&path)
            .map_err(|error| format!("Cannot create private download directory: {error}"))?;
        Ok(Self { path, keep: false })
    }
}

impl Drop for DownloadDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

async fn wait_for_download_cancellation(cancellation: &AtomicBool) {
    while !cancellation.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn download_release_asset(
    url: &str,
    state: &Mutex<UpdateState>,
    cancellation: &AtomicBool,
    ctx: &egui::Context,
) -> Result<(PathBuf, String), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Cannot start update downloader: {error}"))?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent("TerminalCanvas")
            .connect_timeout(Duration::from_secs(REQUEST_TIMEOUT))
            .read_timeout(Duration::from_secs(REQUEST_TIMEOUT))
            .timeout(Duration::from_secs(10 * 60))
            .redirect(release_redirect_policy())
            .build().map_err(|error| format!("Cannot create update downloader: {error}"))?;
        let name = url.rsplit('/').next().ok_or_else(|| "Invalid update filename".to_owned())?;
        let checksum_url = format!("{url}.sha256");
        let request = client.get(&checksum_url).send();
        let mut response = tokio::select! {
            response = request => response.map_err(|error| format!("Cannot download update checksum: {error}"))?,
            _ = wait_for_download_cancellation(cancellation) => return Err("Download cancelled".to_owned()),
        };
        if response.status() != reqwest::StatusCode::OK || response.content_length().is_some_and(|len| len > MAX_CHECKSUM_BYTES as u64) {
            return Err("The release checksum is missing or too large".to_owned());
        }
        let mut checksum_bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|error| format!("Cannot read update checksum: {error}"))?,
                _ = wait_for_download_cancellation(cancellation) => return Err("Download cancelled".to_owned()),
            };
            let Some(chunk) = chunk else { break };
            if checksum_bytes.len().saturating_add(chunk.len()) > MAX_CHECKSUM_BYTES {
                return Err("The release checksum is too large".to_owned());
            }
            checksum_bytes.extend_from_slice(&chunk);
        }
        let hash = std::str::from_utf8(&checksum_bytes).ok()
            .and_then(|text| parse_checksum_manifest(text, name))
            .ok_or_else(|| "The checksum does not name this exact release file".to_owned())?;
        let mut response = tokio::select! {
            response = client.get(url).send() => response.map_err(|error| format!("Cannot download update: {error}"))?,
            _ = wait_for_download_cancellation(cancellation) => return Err("Download cancelled".to_owned()),
        };
        if response.status() != reqwest::StatusCode::OK {
            return Err(format!("Release download returned HTTP {}", response.status().as_u16()));
        }
        let expected_length = response.content_length();
        if expected_length.is_some_and(|len| len == 0 || len > MAX_DOWNLOAD_BYTES) {
            return Err("The update file size is invalid or exceeds 512 MiB".to_owned());
        }
        if let Ok(mut current) = state.lock() { current.total_bytes = expected_length; }
        let mut directory = DownloadDirectory::create()?;
        let partial = directory.path.join(format!("{name}.part"));
        let completed = directory.path.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&partial).map_err(|error| format!("Cannot write update: {error}"))?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        let mut last_repaint = Instant::now();
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|error| format!("Update download interrupted: {error}"))?,
                _ = wait_for_download_cancellation(cancellation) => return Err("Download cancelled".to_owned()),
            };
            let Some(chunk) = chunk else { break };
            length = length.saturating_add(chunk.len() as u64);
            if length > MAX_DOWNLOAD_BYTES { return Err("The update exceeds 512 MiB".to_owned()); }
            file.write_all(&chunk).map_err(|error| format!("Cannot save update: {error}"))?;
            hasher.update(&chunk);
            if let Ok(mut current) = state.lock() { current.downloaded_bytes = length; }
            if last_repaint.elapsed() >= Duration::from_millis(100) {
                ctx.request_repaint();
                last_repaint = Instant::now();
            }
        }
        if cancellation.load(Ordering::Acquire) { return Err("Download cancelled".to_owned()); }
        if length == 0 || expected_length.is_some_and(|expected| expected != length) {
            return Err("The update file is incomplete".to_owned());
        }
        if format!("{:x}", hasher.finalize()) != hash {
            return Err("Update checksum mismatch; the partial file was removed".to_owned());
        }
        file.sync_all().map_err(|error| format!("Cannot finish saving update: {error}"))?;
        drop(file);
        std::fs::rename(&partial, &completed).map_err(|error| format!("Cannot stage verified update: {error}"))?;
        std::fs::write(directory.path.join(format!("{name}.sha256")), &checksum_bytes)
            .map_err(|error| format!("Cannot save release checksum: {error}"))?;
        // Retain only completed verified files; the guard cleans every error/cancel path.
        directory.keep = true;
        Ok((completed, hash))
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

    if resp
        .content_length()
        .is_some_and(|len| len > MAX_CHECKSUM_BYTES as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    resp.take(MAX_CHECKSUM_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_CHECKSUM_BYTES {
        return None;
    }
    let name = url.rsplit('/').next()?.strip_suffix(".sha256")?;
    parse_checksum_manifest(std::str::from_utf8(&bytes).ok()?, name)
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

    use super::{checksum_string, find_platform_asset_for, verify_checksum, version_newer};

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

    #[test]
    fn release_selection_matches_every_platform_and_architecture_exactly() {
        let mut assets = Vec::new();
        for (os, extension) in [("macos", "dmg"), ("windows", "zip"), ("linux", "tar.gz")] {
            for arch in ["x86_64", "aarch64"] {
                let name = format!("TerminalCanvas-1.3.0-{os}-{arch}.{extension}");
                assets.push(serde_json::json!({"name":name,
                    "browser_download_url":format!("{}{}/{}", super::RELEASE_ASSET_PREFIX, "v1.3.0", name)}));
                assets.push(serde_json::json!({"name":format!("{name}.sha256"),
                    "browser_download_url":format!("{}{}/{}.sha256", super::RELEASE_ASSET_PREFIX, "v1.3.0", name)}));
            }
        }
        let release = serde_json::json!({"tag_name":"v1.3.0", "assets":assets});
        for (os, extension) in [("macos", "dmg"), ("windows", "zip"), ("linux", "tar.gz")] {
            for arch in ["x86_64", "aarch64"] {
                let selected = find_platform_asset_for(&release, os, arch).unwrap();
                assert!(selected.ends_with(&format!("-{os}-{arch}.{extension}")));
            }
        }
        assert!(find_platform_asset_for(&release, "windows", "x86").is_none());
        assert!(find_platform_asset_for(&release, "unknown", "x86_64").is_none());
    }

    #[test]
    fn release_selection_rejects_ambiguous_or_mislabeled_assets() {
        for (name, path) in [
            ("TerminalCanvas-1.3.0.dmg", "TerminalCanvas-1.3.0.dmg"),
            (
                "TerminalCanvas-1.3.0-macos-universal.dmg",
                "TerminalCanvas-1.3.0-macos-universal.dmg",
            ),
            (
                "TerminalCanvas-1.3.0-windows-x86_64.exe",
                "TerminalCanvas-1.3.0-windows-x86_64.exe",
            ),
            (
                "TerminalCanvas-1.3.0-macos-aarch64.dmg",
                "TerminalCanvas-1.3.0-macos-x86_64.dmg",
            ),
            (
                "TerminalCanvas-9.9.9-macos-aarch64.dmg",
                "TerminalCanvas-9.9.9-macos-aarch64.dmg",
            ),
        ] {
            let release = serde_json::json!({"tag_name":"v1.3.0", "assets":[{
                "name":name,"browser_download_url":format!("{}v1.3.0/{path}", super::RELEASE_ASSET_PREFIX)
            }]});
            assert!(find_platform_asset_for(&release, "macos", "aarch64").is_none());
            assert!(find_platform_asset_for(&release, "windows", "aarch64").is_none());
        }
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

    #[test]
    fn macos_rejects_an_asset_whose_version_does_not_match_the_release_tag() {
        let release = serde_json::json!({
            "tag_name": "v1.3.0",
            "assets": [{
                "name": "TerminalCanvas-9.9.9.dmg",
                "browser_download_url": "https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-9.9.9.dmg"
            }]
        });
        assert!(find_platform_asset_for(&release, "macos", "aarch64").is_none());
    }

    #[test]
    fn stable_version_comparison_rejects_prereleases_and_malformed_tags() {
        for latest in [
            "1.3",
            "1.3.0.1",
            "1.3.0-beta.1",
            "1.3.0+build",
            "01.3.0",
            "+1.3.0",
            "1.3.0/other",
        ] {
            assert!(!version_newer(latest, "1.2.0"), "accepted {latest}");
        }
        assert!(version_newer("v1.10.0", "1.9.9"));
    }

    #[test]
    fn release_selection_requires_the_checksum_sidecar_in_the_same_release() {
        let name = "TerminalCanvas-1.3.0-windows-x86_64.zip";
        let download_url = format!("{}v1.3.0/{name}", super::RELEASE_ASSET_PREFIX);
        let mut release = serde_json::json!({"tag_name":"v1.3.0", "assets":[{
            "name": name, "browser_download_url": download_url
        }]});
        assert!(find_platform_asset_for(&release, "windows", "x86_64").is_none());
        release["assets"].as_array_mut().unwrap().push(serde_json::json!({
            "name":format!("{name}.sha256"), "browser_download_url":format!("{download_url}.sha256")
        }));
        assert_eq!(
            find_platform_asset_for(&release, "windows", "x86_64"),
            Some(download_url)
        );
    }

    #[test]
    fn checksum_manifest_requires_one_exact_filename_and_sha256() {
        let name = "TerminalCanvas-1.3.0-windows-x86_64.zip";
        let hash = checksum_string(b"release");
        assert_eq!(
            super::parse_checksum_manifest(&format!("{hash}  {name}\n"), name),
            Some(hash.clone())
        );
        assert_eq!(
            super::parse_checksum_manifest(&format!("{} *{name}\n", hash.to_uppercase()), name),
            Some(hash.clone())
        );
        for text in [
            hash.clone(),
            format!("{hash}  other.zip"),
            format!("{hash}  {name}\n{hash} other.zip"),
            format!("{hash} {name} extra"),
            format!("{} {name}", "z".repeat(64)),
        ] {
            assert!(
                super::parse_checksum_manifest(&text, name).is_none(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn cancelled_or_failed_download_directories_are_removed() {
        let path = tempfile_dir();
        std::fs::write(path.join("release.zip.part"), b"partial").unwrap();
        drop(super::DownloadDirectory {
            path: path.clone(),
            keep: false,
        });
        assert!(!path.exists());
        let path = tempfile_dir();
        std::fs::write(path.join("release.zip"), b"verified").unwrap();
        drop(super::DownloadDirectory {
            path: path.clone(),
            keep: true,
        });
        assert!(path.join("release.zip").exists());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn cancellation_terminates_an_idle_download_without_network_or_file_side_effects() {
        let cancellation = std::sync::atomic::AtomicBool::new(true);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(super::wait_for_download_cancellation(&cancellation));
    }

    #[test]
    fn windows_installer_selection_requires_exact_setup_and_checksum_for_supported_architecture() {
        let name = "TerminalCanvas-1.3.0-windows-x86_64-setup.exe";
        let url = format!("{}v1.3.0/{name}", super::RELEASE_ASSET_PREFIX);
        let mut release = serde_json::json!({"tag_name":"v1.3.0", "assets":[{"name":name,"browser_download_url":url}]});
        assert!(super::find_windows_installer_asset(&release, "x86_64").is_none());
        release["assets"].as_array_mut().unwrap().push(serde_json::json!({"name":format!("{name}.sha256"),"browser_download_url":format!("{url}.sha256")}));
        assert_eq!(
            super::find_windows_installer_asset(&release, "x86_64"),
            Some(url)
        );
        assert!(super::find_windows_installer_asset(&release, "aarch64").is_none());
    }

    #[test]
    fn incorrect_platform_downloads_are_rejected_before_starting_a_worker() {
        let checker = super::UpdateChecker::disabled();
        {
            let mut state = checker.state.lock().unwrap();
            state.status = super::UpdateStatus::Available;
            state.latest_version = Some("1.3.0".to_owned());
            state.download_url = Some("https://github.com/MauroProto/terminal-canvas/releases/download/v1.3.0/TerminalCanvas-1.3.0-unknown-x86_64.zip".to_owned());
        }
        checker.download(&egui::Context::default());
        assert!(matches!(
            checker.snapshot().status,
            super::UpdateStatus::Error(_)
        ));
        assert!(checker.snapshot().installer_path.is_none());
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mi-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
