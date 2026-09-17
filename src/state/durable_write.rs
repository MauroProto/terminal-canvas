//! Escritura durable de archivos de estado (patrón `durable-file-write.ts` de
//! Orca).
//!
//! El problema: un `write` directo puede dejar el archivo en cero bytes si el
//! SO cae entre el rename y el flush de datos, y un crash a mitad de escritura
//! corrompe el único ejemplar. Las tres defensas:
//!
//! 1. **tmp → fsync → rename → fsync del directorio**: el rename aterriza el
//!    archivo ya completo; el fsync previo evita que el rename exponga un
//!    archivo de longitud 0, y el del directorio asegura la entrada de
//!    directorio en disco.
//! 2. **Ring de backups** `.bak.0..4`: snapshots en momentos distintos
//!    (espaciado mínimo entre rotaciones), no 5 copias casi iguales. Si el
//!    principal no parsea, se prueba slot por slot.
//! 3. **No-op por contenido**: si los bytes no cambiaron, no se reescribe.
//!    El autosave de 2 s no puede estar tocando el disco sin motivo.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Cantidad de slots del ring de backups (`.bak.0` es el más nuevo).
pub const BACKUP_SLOTS: usize = 5;
/// Espaciado mínimo entre rotaciones del ring: los backups quedan repartidos
/// en el tiempo (snapshots de momentos distintos) en vez de ser 5 copias casi
/// idénticas de la misma hora.
pub const BACKUP_MIN_SPACING: Duration = Duration::from_secs(60 * 60);

/// Escribe `bytes` en `path` de forma durable. Devuelve `true` si escribió;
/// `false` si el archivo ya tenía exactamente ese contenido (no-op).
pub fn write_durable(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    // No-op por contenido: reescribir el mismo estado solo gasta disco y le
    // miente al ring de backups (copias idénticas).
    if content_matches(path, bytes) {
        return Ok(false);
    }
    rotate_backup_ring(path, SystemTime::now(), BACKUP_MIN_SPACING);
    write_atomic_changed(path, bytes)?;
    Ok(true)
}

/// Escritura atómica y durable sin ring de backups. Es la variante apropiada
/// para checkpoints frecuentes y regenerables (scrollback): mantiene el
/// patrón tmp/fsync/rename y el no-op por contenido, sin multiplicar archivos.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if content_matches(path, bytes) {
        return Ok(false);
    }
    write_atomic_changed(path, bytes)?;
    Ok(true)
}

fn write_atomic_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = sibling_with_suffix(path, &format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_directory(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn content_matches(path: &Path, bytes: &[u8]) -> bool {
    std::fs::read(path).is_ok_and(|existing| existing == bytes)
}

/// Carga el primer contenido válido en el orden principal → `.bak.0..4`.
///
/// `parse` decide validez: un archivo que no parsea (crash a mitad de
/// escritura, disco lleno) se salta y se prueba el siguiente slot. Devuelve
/// `None` si ningún slot existe ni parsea.
pub fn load_first_valid<T>(path: &Path, mut parse: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
    for candidate in candidate_paths(path) {
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        if let Some(value) = parse(&bytes) {
            return Some(value);
        }
    }
    None
}

/// Caminos a probar al cargar: el principal primero, después el ring de más
/// nuevo a más viejo.
fn candidate_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    paths.extend((0..BACKUP_SLOTS).map(|slot| backup_path(path, slot)));
    paths
}

/// Path del backup en un slot del ring (`layout.json.bak.0` = más nuevo).
pub fn backup_path(path: &Path, slot: usize) -> PathBuf {
    sibling_with_suffix(path, &format!(".bak.{slot}"))
}

/// Path temporal de escritura (misma carpeta que el destino: el rename tiene
/// que ser dentro del mismo filesystem para ser atómico).
pub fn tmp_path(path: &Path) -> PathBuf {
    sibling_with_suffix(path, ".tmp")
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Rotación del ring: si el slot más nuevo tiene ≥ `min_spacing` de edad (o no
/// existe), desplaza `.bak.i → .bak.i+1` y copia el principal actual a
/// `.bak.0`. Si el slot más nuevo es reciente no se rota: el contenido actual
/// es casi seguro casi idéntico al ya respaldado.
fn rotate_backup_ring(path: &Path, now: SystemTime, min_spacing: Duration) {
    if !path.exists() {
        // Todavía no hay nada que respaldar.
        return;
    }
    let newest = backup_path(path, 0);
    if let Ok(meta) = std::fs::metadata(&newest) {
        if let Ok(mtime) = meta.modified() {
            if now
                .duration_since(mtime)
                .map_or(true, |age| age < min_spacing)
            {
                // mtime en el futuro (reloj corregido) también cuenta como
                // "reciente": no rotar dos veces seguidas.
                return;
            }
        }
    }
    // Desplazar de más viejo a más nuevo para no pisar nada.
    for slot in (1..BACKUP_SLOTS).rev() {
        let from = backup_path(path, slot - 1);
        let to = backup_path(path, slot);
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    if let Err(err) = std::fs::read(path).and_then(|bytes| write_atomic_changed(&newest, &bytes)) {
        log::warn!(
            "No se pudo refrescar el backup de {}: {err}",
            path.display()
        );
    }
}

/// fsync del directorio best-effort: asegura que la entrada de directorio del
/// rename quedó en disco. En Windows abrir un directorio como File falla; se
/// ignora porque el rename ya es lo bastante durable ahí.
fn sync_directory(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{
        backup_path, load_first_valid, rotate_backup_ring, tmp_path, write_atomic, write_durable,
        BACKUP_SLOTS,
    };

    fn unique_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("durable-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_writes_and_reads_back() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        assert!(write_durable(&path, b"hola").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"hola");
        // El temporal no queda tirado.
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn unchanged_content_is_a_noop() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        write_durable(&path, b"igual").unwrap();
        let first_write = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Mismo contenido: no reescribe.
        assert!(!write_durable(&path, b"igual").unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            first_write
        );
    }

    #[test]
    fn atomic_write_is_durable_without_creating_backup_artifacts() {
        let dir = unique_dir();
        let path = dir.join("checkpoint.txt");
        assert!(write_atomic(&path, b"v1").unwrap());
        assert!(write_atomic(&path, b"v2").unwrap());

        assert_eq!(std::fs::read(&path).unwrap(), b"v2");
        assert!(!backup_path(&path, 0).exists());
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn a_second_save_backs_up_the_previous_content() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        write_durable(&path, b"v1").unwrap();
        write_durable(&path, b"v2").unwrap();

        assert_eq!(std::fs::read(backup_path(&path, 0)).unwrap(), b"v1");
        assert_eq!(std::fs::read(&path).unwrap(), b"v2");
    }

    #[test]
    fn rotation_within_the_spacing_window_is_skipped() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        write_durable(&path, b"v1").unwrap();
        // Espaciado enorme: la segunda escritura no rota el ring.
        write_durable(&path, b"v2").unwrap();
        write_durable(&path, b"v3").unwrap();

        // El slot 0 sigue siendo v1 (la única rotación que pasó).
        assert_eq!(std::fs::read(backup_path(&path, 0)).unwrap(), b"v1");
    }

    #[test]
    fn the_ring_keeps_at_most_five_backups_oldest_first_shifted() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        // Espaciado 0: rota en cada escritura.
        for version in 0..8u32 {
            // `copy` puede dejar un mtime apenas posterior a un `now`
            // capturado justo antes. Un reloj futuro controlado hace que el
            // test verifique el ring, no la granularidad del filesystem.
            let now = SystemTime::now() + Duration::from_secs(60 + u64::from(version));
            rotate_backup_ring(&path, now, Duration::ZERO);
            std::fs::write(&path, format!("v{version}")).unwrap();
        }
        // 8 escrituras → el anillo retiene las últimas 5 copias desplazadas:
        // bak.0 es la versión 6 (la previa a la última escritura).
        assert_eq!(std::fs::read(&path).unwrap(), b"v7");
        assert_eq!(std::fs::read(backup_path(&path, 0)).unwrap(), b"v6");
        assert_eq!(std::fs::read(backup_path(&path, 4)).unwrap(), b"v2");
        assert!(!backup_path(&path, BACKUP_SLOTS).exists());
    }

    #[test]
    fn load_skips_a_corrupt_primary_and_uses_the_newest_valid_backup() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        std::fs::write(&path, b"v1").unwrap();
        // Rotación forzada: v1 queda respaldado; después v2 pasa al principal.
        rotate_backup_ring(&path, SystemTime::now(), Duration::ZERO);
        std::fs::write(&path, b"v2").unwrap();
        rotate_backup_ring(&path, SystemTime::now(), Duration::ZERO);
        // El principal se corrompe; bak.0 tiene la versión buena más nueva.
        std::fs::write(&path, b"{roto").unwrap();

        let loaded: Option<Vec<u8>> = load_first_valid(&path, |bytes| {
            (!bytes.starts_with(b"{")).then(|| bytes.to_vec())
        });
        assert_eq!(loaded.as_deref(), Some(b"v2".as_slice()));
    }

    #[test]
    fn load_walks_the_ring_until_something_parses() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        // bak.1 = v1 (válido), bak.0 = v2; después se corrompen principal y bak.0.
        std::fs::write(&path, b"v1").unwrap();
        rotate_backup_ring(&path, SystemTime::now(), Duration::ZERO);
        std::fs::write(&path, b"v2").unwrap();
        rotate_backup_ring(&path, SystemTime::now(), Duration::ZERO);
        std::fs::write(&path, b"{roto").unwrap();
        std::fs::write(backup_path(&path, 0), b"{tambien roto").unwrap();

        let loaded: Option<Vec<u8>> = load_first_valid(&path, |bytes| {
            (!bytes.starts_with(b"{")).then(|| bytes.to_vec())
        });
        assert_eq!(loaded.as_deref(), Some(b"v1".as_slice()));
    }

    #[test]
    fn load_returns_none_when_nothing_is_valid() {
        let dir = unique_dir();
        let path = dir.join("layout.json");
        let loaded: Option<Vec<u8>> = load_first_valid(&path, |_| None);
        assert!(loaded.is_none());
    }
}

/// Migrate existing Unix configuration and backups even for content-equal saves.
/// Windows inherits the per-user profile ACL; mode bits are not an ACL substitute.
pub fn protect_private_files(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        // SAFETY: geteuid has no pointer arguments and only returns process identity.
        let uid = unsafe { libc::geteuid() };
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("Private file has no parent"))?;
        let parent_metadata = match std::fs::symlink_metadata(parent) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if !parent_metadata.is_dir()
            || parent_metadata.file_type().is_symlink()
            || parent_metadata.uid() != uid
        {
            return Err(std::io::Error::other(
                "Private directory must be owned by the current user and not be a symlink",
            ));
        }
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(parent)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || metadata.uid() != uid {
            return Err(std::io::Error::other("Invalid private directory owner"));
        }
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        let mut paths = candidate_paths(path);
        paths.push(tmp_path(path));
        for candidate in paths {
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&candidate)
            {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != uid {
                return Err(std::io::Error::other(
                    "Private config must be an owner-only regular file",
                ));
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
#[cfg(all(test, unix))]
mod private_file_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn config_migration_noop_replacement_and_backups_remain_private() {
        let root = std::env::temp_dir().join(format!("tc-private-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let config = crate::config::AppConfig {
            linear_token: Some("dummy-private-value".to_owned()),
            ..Default::default()
        };
        crate::config::save_to_path(&config, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(backup_path(&path, 0), "old dummy").unwrap();
        std::fs::set_permissions(
            backup_path(&path, 0),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        crate::config::save_to_path(&config, &path).unwrap();
        for candidate in [&path, &backup_path(&path, 0)] {
            assert_eq!(
                std::fs::metadata(candidate).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut changed = config.clone();
        changed.font_size += 1.0;
        crate::config::save_to_path(&changed, &path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            crate::config::load_from_path(&path).linear_token,
            config.linear_token
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn private_config_rejects_symlinks_and_hardlinks() {
        let root = std::env::temp_dir().join(format!("tc-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("other.txt");
        let path = root.join("config.toml");
        std::fs::write(&target, "untouched").unwrap();
        symlink(&target, &path).unwrap();
        assert!(protect_private_files(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(protect_private_files(&path).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
        std::fs::remove_dir_all(root).unwrap();
    }
}
