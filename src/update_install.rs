//! Instalación de una actualización descargada en macOS (Ship-it 7.1, T3).
//!
//! El flujo es: montar el `.dmg`, **verificar la firma** del `.app` que trae,
//! y recién ahí reemplazar el instalado en `/Applications`. La verificación no
//! es opcional: sin ella, cualquier redirección del download sería ejecución
//! de código arbitrario con los permisos del usuario.
//!
//! El swap se hace al salir de la app (no se puede reemplazar el bundle que se
//! está ejecutando).

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
    (!path.is_empty()).then(|| PathBuf::from(path))
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
    let output = Command::new("hdiutil")
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
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(mount_point)
        .output();
}

/// Verifica la firma del bundle. Sin firma válida, no se instala.
pub fn verify_bundle_signature(app: &Path) -> bool {
    let mut codesign = Command::new("codesign");
    codesign
        .args(["--verify", "--deep", "--strict", "-R"])
        .arg(format!(
            "=identifier \"{APP_BUNDLE_ID}\" and anchor apple generic and certificate leaf[subject.OU] exists"
        ))
        .arg(app);
    let mut gatekeeper = Command::new("spctl");
    gatekeeper.args(["--assess", "--type", "execute"]).arg(app);
    command_succeeds(&mut codesign) && command_succeeds(&mut gatekeeper)
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
    let canonical_new = std::fs::canonicalize(new_bundle)?;
    let canonical_target = std::fs::canonicalize(APPLICATIONS_DIR)?.join(APP_BUNDLE_NAME);
    if canonical_new == canonical_target || canonical_new.starts_with(&canonical_target) {
        anyhow::bail!("el bundle nuevo no puede ser el destino instalado");
    }
    let backup = target.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&backup);
    let had_previous = target.exists();
    if had_previous {
        std::fs::rename(target, &backup)?;
    }
    // `ditto` preserva permisos, symlinks y metadata del bundle; un copy
    // recursivo común rompe la firma.
    let copied = Command::new("ditto")
        .arg(new_bundle)
        .arg(target)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !copied || !verify_bundle_signature(target) {
        let _ = std::fs::remove_dir_all(target);
        if had_previous {
            std::fs::rename(&backup, target)?;
        }
        anyhow::bail!("no se pudo copiar o verificar el bundle nuevo");
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

/// Instala un dmg ya descargado: montar → verificar firma → swap → desmontar.
pub fn install_dmg(dmg: &Path) -> anyhow::Result<()> {
    let mount_point = mount_dmg(dmg).ok_or_else(|| anyhow::anyhow!("no se pudo montar el dmg"))?;
    let result = (|| {
        let bundle = bundle_in_volume(&mount_point);
        if !bundle.exists() {
            anyhow::bail!("el dmg no contiene {APP_BUNDLE_NAME}");
        }
        if !verify_bundle_signature(&bundle) {
            anyhow::bail!("la firma del bundle descargado no verifica");
        }
        swap_bundle(&bundle, &installed_app_path())
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
