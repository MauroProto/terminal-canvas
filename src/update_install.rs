//! macOS updates require the running installed app's Developer ID publisher,
//! Gatekeeper assessment, exact version, bundle identifier, and executable
//! architecture. New code is copied to a fresh sibling before any rename;
//! signed executable files are never overwritten in place. Portable unsigned
//! Windows/Linux archives are downloaded and verified but opened for manual
//! extraction, never executed as installers.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_BUNDLE_NAME: &str = "TerminalCanvas.app";
pub const APP_BUNDLE_ID: &str = "com.terminalcanvas.app";
const APPLICATIONS_DIR: &str = "/Applications";

/// Salida de `hdiutil attach -plist`: nos quedamos con el primer
/// `mount-point`. Puro para poder testear el parseo sin montar nada.
pub fn parse_mount_point(plist: &str) -> Option<PathBuf> {
    // <key>mount-point</key>\n<string>/Volumes/TerminalCanvas</string>
    let key_index = plist.find("<key>mount-point</key>")?;
    let rest = &plist[key_index..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    let path = rest[start..end].trim();
    let path = path
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    if path.is_empty() || path.contains(['\0', '\n', '\r']) {
        return None;
    }
    Some(PathBuf::from(path))
}

/// ¿Es un destino aceptable para el swap? Solo un `.app` dentro de
/// `/Applications`: nunca se toca nada afuera de ahí.
pub fn is_safe_swap_target(path: &Path) -> bool {
    path == installed_app_path()
}

/// Dónde vive (o va a vivir) la app instalada.
pub fn installed_app_path() -> PathBuf {
    Path::new(APPLICATIONS_DIR).join(APP_BUNDLE_NAME)
}

/// Path del bundle dentro del volumen montado.
pub fn bundle_in_volume(mount_point: &Path) -> PathBuf {
    mount_point.join(APP_BUNDLE_NAME)
}

/// Monta el dmg y devuelve el punto de montaje.
pub fn mount_dmg(dmg: &Path) -> Option<PathBuf> {
    let output = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-plist"])
        .arg(dmg)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_mount_point(&String::from_utf8_lossy(&output.stdout))
        .filter(|path| path.starts_with("/Volumes") && path.parent() == Some(Path::new("/Volumes")))
}

pub fn detach_dmg(mount_point: &Path) {
    if !mount_point.starts_with("/Volumes") || mount_point.parent() != Some(Path::new("/Volumes")) {
        return;
    }
    let _ = Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet"])
        .arg(mount_point)
        .output();
}

/// Verifica la firma del bundle. Sin firma válida, no se instala.
pub fn verify_bundle_signature(app: &Path) -> bool {
    trusted_team_id().is_ok_and(|team| verify_bundle_signature_for_team(app, &team))
}

fn verify_bundle_signature_for_team(app: &Path, team: &str) -> bool {
    if !valid_team_id(team) {
        return false;
    }
    let mut codesign = Command::new("/usr/bin/codesign");
    codesign
        .args(["--verify", "--deep", "--strict", "-R"])
        .arg(format!(
            "=identifier \"{APP_BUNDLE_ID}\" and anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{team}\""
        ))
        .arg(app);
    let mut gatekeeper = Command::new("/usr/sbin/spctl");
    gatekeeper.args(["--assess", "--type", "execute"]).arg(app);
    command_succeeds(&mut codesign) && command_succeeds(&mut gatekeeper)
}

fn valid_team_id(team: &str) -> bool {
    team.len() == 10
        && team
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn parse_team_identifier(output: &str) -> Option<&str> {
    let mut teams = output
        .lines()
        .filter_map(|line| line.strip_prefix("TeamIdentifier="));
    let team = teams.next()?;
    (teams.next().is_none() && valid_team_id(team)).then_some(team)
}

fn running_installed_bundle() -> anyhow::Result<PathBuf> {
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let target = installed_app_path();
    let metadata = std::fs::symlink_metadata(&target)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("The installed application must be a real bundle in /Applications");
    }
    let canonical = std::fs::canonicalize(&target)?;
    if !executable.starts_with(canonical.join("Contents/MacOS")) {
        anyhow::bail!(
            "Install TerminalCanvas in /Applications before using automatic installation"
        );
    }
    Ok(canonical)
}

fn trusted_team_id() -> anyhow::Result<String> {
    let current = running_installed_bundle()?;
    let details = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(&current)
        .output()?;
    if !details.status.success() {
        anyhow::bail!("The running app has no trusted Developer ID signature");
    }
    let output = String::from_utf8_lossy(&details.stderr);
    let team = parse_team_identifier(&output)
        .ok_or_else(|| anyhow::anyhow!("The running app has no valid publisher Team ID"))?;
    if !verify_bundle_signature_for_team(&current, team) {
        anyhow::bail!("The running app's Developer ID signature or Gatekeeper assessment failed");
    }
    Ok(team.to_owned())
}

pub fn automatic_install_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::env::current_exe()
            .ok()
            .is_some_and(|path| windows_publisher(&path, None).is_ok())
    }
    #[cfg(not(target_os = "windows"))]
    {
        cfg!(target_os = "macos") && running_installed_bundle().is_ok()
    }
}

/// Windows preparation validates OS trust, the current certificate, and the
/// installer's signed version resource before asking the UI to close safely.
pub fn prepare_or_install_verified_update(path: &Path, version: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        verify_windows_installer(path, version)
    }
    #[cfg(not(target_os = "windows"))]
    {
        install_verified_update(path, version)
    }
}

#[cfg(target_os = "windows")]
const WINDOWS_SIGNATURE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1') -ErrorAction Stop
$signature = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $env:TC_UPDATE_SIGNATURE_PATH
if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) {
    throw 'The executable has no valid trusted Authenticode signature. Install a signed release manually first.'
}
if ($env:TC_UPDATE_EXPECTED_VERSION) {
    $version = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($env:TC_UPDATE_SIGNATURE_PATH).ProductVersion
    if ($version -ne $env:TC_UPDATE_EXPECTED_VERSION) { throw 'The signed installer version does not match this release.' }
}
$sha = [System.Security.Cryptography.SHA256]::Create()
try { [BitConverter]::ToString($sha.ComputeHash($signature.SignerCertificate.RawData)).Replace('-', '') }
finally { $sha.Dispose() }
"#;

#[cfg(target_os = "windows")]
fn windows_publisher(path: &Path, version: Option<&str>) -> anyhow::Result<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Windows system directory is unavailable"))?;
    if !system_root.is_absolute() {
        anyhow::bail!("Windows system directory must be absolute");
    }
    let mut command =
        Command::new(system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_SIGNATURE_SCRIPT,
        ])
        .current_dir(system_root.join("System32"))
        .env("TC_UPDATE_SIGNATURE_PATH", path)
        .env("TC_UPDATE_EXPECTED_VERSION", version.unwrap_or_default())
        .env_remove("PSModulePath")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000);
    let mut child = command.spawn()?;
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() >= Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Authenticode verification timed out; the update was not launched");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "Authenticode verification failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let publisher = String::from_utf8(output.stdout)?.trim().to_owned();
    if publisher.len() != 64 || !publisher.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("Authenticode verification returned an invalid certificate identity");
    }
    Ok(publisher)
}

#[cfg(target_os = "windows")]
fn verify_windows_installer(path: &Path, version: &str) -> anyhow::Result<()> {
    if !crate::update::version_newer(version, env!("CARGO_PKG_VERSION")) {
        anyhow::bail!("An update must be newer than the running application");
    }
    let expected = format!(
        "TerminalCanvas-{version}-windows-{}-setup.exe",
        std::env::consts::ARCH
    );
    if std::env::consts::ARCH != "x86_64"
        || path.file_name().and_then(|name| name.to_str()) != Some(expected.as_str())
    {
        anyhow::bail!("The installer does not match this system, architecture, and version");
    }
    let running = std::env::current_exe()?;
    let installed_publisher = windows_publisher(&running, None)?;
    let downloaded_publisher = windows_publisher(path, Some(version))?;
    if installed_publisher != downloaded_publisher {
        anyhow::bail!("The update signing certificate differs from the running application; install this publisher change manually");
    }
    Ok(())
}

/// Called only after the app has drained accepted saves and checked again that
/// no terminal or sharing session is live. The installer never closes apps.
pub fn launch_verified_windows_installer(
    path: &Path,
    version: &str,
    hash: &str,
) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ: verification and CreateProcess may read, while all
        // writers, deletion, and replacement are denied until launch completes.
        let locked_file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(path)?;
        if !crate::update::verify_checksum(path, hash) {
            anyhow::bail!("The prepared update changed; download it again");
        }
        verify_windows_installer(path, version)?;
        let mut command = Command::new(path);
        command.args(["/NOCLOSEAPPLICATIONS", "/NORESTART", "/SP-"]);
        let result =
            crate::utils::platform::spawn_detached("verified-update-installer", &mut command);
        drop(locked_file);
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (path, version, hash);
        anyhow::bail!("Windows installer launch is unavailable on this platform");
    }
}

fn plist_value(bundle: &Path, key: &str) -> anyhow::Result<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")])
        .arg(bundle.join("Contents/Info.plist"))
        .output()?;
    if !output.status.success() {
        anyhow::bail!("The downloaded app is missing {key}");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn verify_bundle_metadata(bundle: &Path, version: &str) -> anyhow::Result<()> {
    if plist_value(bundle, "CFBundleIdentifier")? != APP_BUNDLE_ID
        || plist_value(bundle, "CFBundleShortVersionString")? != version
        || plist_value(bundle, "CFBundleVersion")? != version
        || plist_value(bundle, "CFBundleExecutable")? != "TerminalCanvas"
    {
        anyhow::bail!(
            "The downloaded app identifier, executable, or version does not match this release"
        );
    }
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => anyhow::bail!("This architecture does not support automatic installation"),
    };
    let mut lipo = Command::new("/usr/bin/lipo");
    lipo.args(["-verify_arch", architecture])
        .arg(bundle.join("Contents/MacOS/TerminalCanvas"));
    if !command_succeeds(&mut lipo) {
        anyhow::bail!("The update executable does not contain this system architecture");
    }
    Ok(())
}

fn command_succeeds(command: &mut Command) -> bool {
    command
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Reemplaza el bundle instalado por el nuevo. Deja el viejo a un costado
/// hasta que la copia termina bien: si `ditto` falla a mitad, se restaura.
pub fn swap_bundle(new_bundle: &Path, target: &Path) -> anyhow::Result<()> {
    if !is_safe_swap_target(target) {
        anyhow::bail!("destino de instalación inseguro: {}", target.display());
    }
    if !new_bundle.join("Contents/MacOS").is_dir() {
        anyhow::bail!("el bundle nuevo no tiene Contents/MacOS");
    }
    let version = plist_value(new_bundle, "CFBundleShortVersionString")?;
    let team = trusted_team_id()?;
    swap_verified_bundle(new_bundle, target, &version, &team)
}

fn swap_verified_bundle(
    new_bundle: &Path,
    target: &Path,
    version: &str,
    team: &str,
) -> anyhow::Result<()> {
    if !is_safe_swap_target(target) {
        anyhow::bail!("Unsafe installation target: {}", target.display());
    }
    if std::fs::symlink_metadata(target)?.file_type().is_symlink() {
        anyhow::bail!("The installed application cannot be a symbolic link");
    }
    let canonical_new = std::fs::canonicalize(new_bundle)?;
    let canonical_target = std::fs::canonicalize(APPLICATIONS_DIR)?.join(APP_BUNDLE_NAME);
    if canonical_new == canonical_target || canonical_new.starts_with(&canonical_target) {
        anyhow::bail!("el bundle nuevo no puede ser el destino instalado");
    }
    verify_bundle_metadata(new_bundle, version)?;
    if !verify_bundle_signature_for_team(new_bundle, team) {
        anyhow::bail!(
            "The update publisher or notarization does not match the installed application"
        );
    }
    // Unique siblings preserve previous recovery copies and use the same filesystem.
    let operation = uuid::Uuid::new_v4();
    let staging_root = target.with_file_name(format!(".TerminalCanvas-update-{operation}"));
    std::fs::create_dir(&staging_root)?;
    let staging = staging_root.join(APP_BUNDLE_NAME);
    let backup = target.with_file_name(format!(".TerminalCanvas-previous-{operation}.app"));
    // `ditto` preserva permisos, symlinks y metadata del bundle; un copy
    // recursivo común rompe la firma.
    let copied = Command::new("/usr/bin/ditto")
        .arg(new_bundle)
        .arg(&staging)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !copied
        || verify_bundle_metadata(&staging, version).is_err()
        || !verify_bundle_signature_for_team(&staging, team)
    {
        let _ = std::fs::remove_dir_all(&staging_root);
        anyhow::bail!(
            "The staged update could not be copied or verified; the installed app was preserved"
        );
    }
    if let Err(error) = std::fs::rename(target, &backup) {
        let _ = std::fs::remove_dir_all(&staging_root);
        return Err(anyhow::anyhow!(
            "Cannot replace the installed application: {error}"
        ));
    }
    if let Err(error) = std::fs::rename(&staging, target) {
        let restored = std::fs::rename(&backup, target);
        let _ = std::fs::remove_dir_all(&staging_root);
        match restored {
            Ok(()) => anyhow::bail!("Cannot install update; the previous app was restored: {error}"),
            Err(restore_error) => anyhow::bail!("Cannot install update: {error}; the previous app is preserved at {} (restore failed: {restore_error})", backup.display()),
        }
    }
    let _ = std::fs::remove_dir(&staging_root);
    // The old running executable keeps its inode until this process exits.
    // Retaining the previous bundle also makes a crash/power loss recoverable.
    Ok(())
}

/// Instala un dmg ya descargado: montar → verificar firma → swap → desmontar.
pub fn install_verified_update(dmg: &Path, version: &str) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("This platform uses portable updates; extract the verified archive manually");
    }
    if !crate::update::version_newer(version, env!("CARGO_PKG_VERSION")) {
        anyhow::bail!("An update must be newer than the running application");
    }
    let team = trusted_team_id()?;
    let mount_point = mount_dmg(dmg).ok_or_else(|| anyhow::anyhow!("no se pudo montar el dmg"))?;
    let result = (|| {
        let bundle = bundle_in_volume(&mount_point);
        if !bundle.exists() {
            anyhow::bail!("el dmg no contiene {APP_BUNDLE_NAME}");
        }
        verify_bundle_metadata(&bundle, version)?;
        if !verify_bundle_signature_for_team(&bundle, &team) {
            anyhow::bail!("la firma del bundle descargado no verifica");
        }
        swap_verified_bundle(&bundle, &installed_app_path(), version, &team)
    })();
    detach_dmg(&mount_point);
    result
}

#[cfg(test)]
mod tests {
    use super::{
        bundle_in_volume, installed_app_path, is_safe_swap_target, parse_mount_point, swap_bundle,
        APP_BUNDLE_NAME,
    };
    use std::path::{Path, PathBuf};

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>system-entities</key>
  <array>
    <dict>
      <key>content-hint</key><string>GUID_partition_scheme</string>
    </dict>
    <dict>
      <key>dev-entry</key><string>/dev/disk4s1</string>
      <key>mount-point</key><string>/Volumes/TerminalCanvas</string>
    </dict>
  </array>
</dict>
</plist>"#;

    #[test]
    fn the_mount_point_is_parsed_from_the_hdiutil_plist() {
        assert_eq!(
            parse_mount_point(PLIST),
            Some(PathBuf::from("/Volumes/TerminalCanvas"))
        );
    }

    #[test]
    fn a_plist_without_a_mount_point_yields_none() {
        assert_eq!(parse_mount_point("<plist></plist>"), None);
        assert_eq!(parse_mount_point(""), None);
    }

    #[test]
    fn publisher_team_identifier_must_be_unambiguous_and_well_formed() {
        assert_eq!(
            super::parse_team_identifier(
                "Identifier=com.terminalcanvas.app\nTeamIdentifier=ABCDE12345\n"
            ),
            Some("ABCDE12345")
        );
        for output in [
            "TeamIdentifier=not set",
            "TeamIdentifier=ABCDE12345\nTeamIdentifier=ZYXWV98765",
            "TeamIdentifier=ABCDE12345\" or true",
            "TeamIdentifier=abcde12345",
        ] {
            assert!(super::parse_team_identifier(output).is_none());
        }
    }

    #[test]
    fn mount_point_decodes_xml_and_rejects_control_characters() {
        assert_eq!(
            parse_mount_point(
                "<key>mount-point</key><string>/Volumes/Terminal&amp;Canvas</string>"
            ),
            Some(PathBuf::from("/Volumes/Terminal&Canvas"))
        );
        assert!(
            parse_mount_point("<key>mount-point</key><string>/Volumes/Term\ninal</string>")
                .is_none()
        );
    }

    #[test]
    fn unsupported_platforms_never_run_downloaded_archives_as_installers() {
        if !cfg!(target_os = "macos") {
            let error =
                super::install_verified_update(Path::new("untrusted.zip"), "9.9.9").unwrap_err();
            assert!(error.to_string().contains("manually"));
            assert!(!super::automatic_install_supported());
        }
    }

    #[test]
    fn only_app_bundles_directly_in_applications_are_swappable() {
        assert!(is_safe_swap_target(Path::new(
            "/Applications/TerminalCanvas.app"
        )));
        // Nada fuera de /Applications.
        assert!(!is_safe_swap_target(Path::new("/tmp/TerminalCanvas.app")));
        assert!(!is_safe_swap_target(Path::new(
            "/Users/alguien/Applications/TerminalCanvas.app"
        )));
        assert!(!is_safe_swap_target(Path::new("/Applications/Otra.app")));
        // Ni directorios que no sean un bundle.
        assert!(!is_safe_swap_target(Path::new("/Applications")));
        assert!(!is_safe_swap_target(Path::new("/Applications/algo")));
        // Ni anidados (no queremos borrar /Applications/Foo.app/Contents).
        assert!(!is_safe_swap_target(Path::new(
            "/Applications/Foo.app/Contents/Bar.app"
        )));
    }

    #[test]
    fn the_installed_path_and_the_volume_path_line_up() {
        assert_eq!(
            installed_app_path(),
            PathBuf::from("/Applications").join(APP_BUNDLE_NAME)
        );
        assert_eq!(
            bundle_in_volume(Path::new("/Volumes/TerminalCanvas")),
            PathBuf::from("/Volumes/TerminalCanvas").join(APP_BUNDLE_NAME)
        );
    }

    #[test]
    fn the_swap_refuses_an_unsafe_target() {
        let dir = std::env::temp_dir().join(format!("swap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
        let error = swap_bundle(&dir, &dir.join("otro.app")).expect_err("tiene que rechazar");
        assert!(error.to_string().contains("inseguro"), "got {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_swap_refuses_a_bundle_without_an_executable_dir() {
        // Un "bundle" que no tiene Contents/MacOS no es una app: no se
        // instala aunque el destino sea válido.
        let dir = std::env::temp_dir().join(format!("swap-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let error =
            swap_bundle(&dir, &super::installed_app_path()).expect_err("tiene que rechazar");
        assert!(error.to_string().contains("Contents/MacOS"), "got {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
