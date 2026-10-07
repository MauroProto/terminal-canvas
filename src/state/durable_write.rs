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
    validate_write_target(path)?;
    // No-op por contenido: reescribir el mismo estado solo gasta disco y le
    // miente al ring de backups (copias idénticas).
    if content_matches(path, bytes) {
        return Ok(false);
    }
    // A no-op never touches the ring. Changed writes validate every slot
    // before rotation can move a special file or copy through a backup link.
    validate_backup_ring(path)?;
    rotate_backup_ring(path, SystemTime::now(), BACKUP_MIN_SPACING);
    write_atomic_changed(path, bytes)?;
    Ok(true)
}

/// Escritura atómica y durable sin ring de backups. Es la variante apropiada
/// para checkpoints frecuentes y regenerables (scrollback): mantiene el
/// patrón tmp/fsync/rename y el no-op por contenido, sin multiplicar archivos.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    validate_write_target(path)?;
    if content_matches(path, bytes) {
        return Ok(false);
    }
    write_atomic_changed(path, bytes)?;
    Ok(true)
}

// Check types before opening contents, rotating backups or replacing entries.
// This is a preflight for stable paths, not protection against concurrent
// filesystem replacement. Generic writers keep following regular-file links.
fn validate_write_target(path: &Path) -> std::io::Result<()> {
    let metadata = match retry_interrupted(|| std::fs::symlink_metadata(path)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let metadata = if metadata.file_type().is_symlink() {
        // A dangling link is present: failure here must not become permission
        // to create a new file over that link.
        retry_interrupted(|| std::fs::metadata(path))?
    } else {
        metadata
    };
    if metadata.is_file() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "state destination must be a regular file: {}",
                path.display()
            ),
        ))
    }
}

fn validate_backup_ring(path: &Path) -> std::io::Result<()> {
    for slot in 0..BACKUP_SLOTS {
        let backup = backup_path(path, slot);
        match retry_interrupted(|| std::fs::symlink_metadata(&backup)) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "state backup must be a regular file without a link: {}",
                        backup.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn write_atomic_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A predictable .tmp may be a link and may retain broad old permissions.
    // Create a fresh private inode exclusively, before writing any contents.
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

/// Protect configuration and every retained copy, including on a no-op save.
/// Unix uses owner-only modes. On Windows the per-user config directory and
/// its inherited ACL remain the OS security boundary.
pub fn protect_private_state(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private state needs a directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent)?;
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        for candidate in candidate_paths(path) {
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&candidate)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "private state must be a regular file with a single link",
                ));
            }
            // Descriptor-based chmod never follows a replacement symlink.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    {
        for candidate in candidate_paths(path) {
            match std::fs::symlink_metadata(&candidate) {
                Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "private state must be a regular file",
                    ));
                }
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn write_private_durable(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    protect_private_state(path)?;
    let changed = write_durable(path, bytes)?;
    protect_private_state(path)?;
    Ok(changed)
}

fn content_matches(path: &Path, bytes: &[u8]) -> bool {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Follow regular-file symlinks as before, but never wait for a FIFO
        // writer before descriptor metadata can reject the non-regular file.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let Ok(mut file) = retry_interrupted(|| options.open(path)) else {
        return false;
    };
    let Ok(metadata) = retry_interrupted(|| file.metadata()) else {
        return false;
    };
    metadata.is_file() && content_matches_reader(&mut file, metadata.len(), bytes)
}

const CONTENT_COMPARE_CHUNK_BYTES: usize = 8 * 1024;

fn content_matches_reader(reader: &mut impl std::io::Read, len: u64, bytes: &[u8]) -> bool {
    if u64::try_from(bytes.len()).ok() != Some(len) {
        return false;
    }
    let mut buffer = [0_u8; CONTENT_COMPARE_CHUNK_BYTES];
    let mut offset = 0;
    while offset < bytes.len() {
        let requested = (bytes.len() - offset).min(buffer.len());
        let Ok(read) = retry_interrupted(|| reader.read(&mut buffer[..requested])) else {
            return false;
        };
        if read == 0 || buffer[..read] != bytes[offset..offset + read] {
            return false;
        }
        offset += read;
    }
    // A matching metadata length is only a hint: reject growth after stat.
    matches!(retry_interrupted(|| reader.read(&mut buffer[..1])), Ok(0))
}

fn retry_interrupted<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    loop {
        match operation() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
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
    if let Err(err) = std::fs::copy(path, &newest) {
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

#[cfg(all(test, unix))]
mod private_security_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn security_private_state_and_backups_are_repaired_even_on_noop_save() {
        let root = std::env::temp_dir().join(format!("tc-private-{}", uuid::Uuid::new_v4()));
        let directory = root.join("config");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        let backup = backup_path(&path, 0);
        std::fs::write(&path, b"dummy-current").unwrap();
        std::fs::write(&backup, b"dummy-backup").unwrap();
        for file in [&path, &backup] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(!write_private_durable(&path, b"dummy-current").unwrap());
        for file in [&path, &backup] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        write_private_durable(&path, b"dummy-new").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn security_private_state_rejects_links_and_atomic_write_ignores_legacy_tmp_link() {
        let root = std::env::temp_dir().join(format!("tc-link-{}", uuid::Uuid::new_v4()));
        let directory = root.join("config");
        std::fs::create_dir_all(&directory).unwrap();
        let outside = root.join("untouched");
        std::fs::write(&outside, b"outside-fixture").unwrap();
        let path = directory.join("config.toml");
        symlink(&outside, &path).unwrap();
        assert!(write_private_durable(&path, b"must-not-write").is_err());
        std::fs::remove_file(&path).unwrap();
        symlink(&outside, backup_path(&path, 0)).unwrap();
        assert!(write_private_durable(&path, b"must-not-write").is_err());
        std::fs::remove_file(backup_path(&path, 0)).unwrap();
        symlink(&outside, tmp_path(&path)).unwrap();
        write_private_durable(&path, b"private-fixture").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside-fixture");
        assert_eq!(std::fs::read(&path).unwrap(), b"private-fixture");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod streaming_content_tests {
    use std::io::{Error, ErrorKind, Read};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{
        backup_path, content_matches, content_matches_reader, write_atomic, write_durable,
        BACKUP_SLOTS, CONTENT_COMPARE_CHUNK_BYTES,
    };

    struct ControlledReader<'a> {
        remaining: &'a [u8],
        calls: usize,
        bytes_read: usize,
        largest_request: usize,
        max_chunk: usize,
        interrupt_calls: &'a [usize],
        fail_on_call: Option<usize>,
    }

    impl<'a> ControlledReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                remaining: data,
                calls: 0,
                bytes_read: 0,
                largest_request: 0,
                max_chunk: usize::MAX,
                interrupt_calls: &[],
                fail_on_call: None,
            }
        }
    }

    impl Read for ControlledReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.largest_request = self.largest_request.max(buffer.len());
            if self.interrupt_calls.contains(&self.calls) {
                return Err(ErrorKind::Interrupted.into());
            }
            if self.fail_on_call == Some(self.calls) {
                return Err(Error::from(ErrorKind::PermissionDenied));
            }
            let read = self.remaining.len().min(buffer.len()).min(self.max_chunk);
            buffer[..read].copy_from_slice(&self.remaining[..read]);
            self.remaining = &self.remaining[read..];
            self.bytes_read += read;
            Ok(read)
        }
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tc-streaming-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fingerprint(path: &Path) -> (Vec<u8>, SystemTime) {
        (
            std::fs::read(path).unwrap(),
            std::fs::metadata(path).unwrap().modified().unwrap(),
        )
    }

    #[test]
    fn streaming_content_matches_equal_bytes_across_fixed_chunks() {
        let bytes = vec![b'x'; CONTENT_COMPARE_CHUNK_BYTES * 2 + 9];
        let mut reader = ControlledReader::new(&bytes);
        assert!(content_matches_reader(
            &mut reader,
            bytes.len() as u64,
            &bytes
        ));
        assert_eq!(reader.largest_request, CONTENT_COMPARE_CHUNK_BYTES);
        assert_eq!(reader.bytes_read, bytes.len());
        assert_eq!(reader.calls, 4);
    }

    #[test]
    fn streaming_content_matches_empty_bytes_only_after_eof() {
        let mut reader = ControlledReader::new(b"");
        assert!(content_matches_reader(&mut reader, 0, b""));
        assert_eq!(reader.calls, 1);
        assert_eq!(reader.largest_request, 1);
    }

    #[test]
    fn streaming_content_rejects_a_difference_at_the_first_byte() {
        let mut reader = ControlledReader::new(b"xbc");
        assert!(!content_matches_reader(&mut reader, 3, b"abc"));
        assert_eq!(reader.calls, 1);
    }

    #[test]
    fn streaming_content_rejects_a_difference_at_the_last_byte() {
        let bytes = vec![b'x'; CONTENT_COMPARE_CHUNK_BYTES + 9];
        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() = b'y';
        let mut reader = ControlledReader::new(&changed);
        assert!(!content_matches_reader(
            &mut reader,
            bytes.len() as u64,
            &bytes
        ));
        assert_eq!(reader.calls, 2);
    }

    #[test]
    fn streaming_content_rejects_a_gibibyte_length_without_reading() {
        let mut reader = ControlledReader::new(b"unused");
        assert!(!content_matches_reader(&mut reader, 1_u64 << 30, b"x"));
        assert_eq!(reader.calls, 0);
        assert_eq!(reader.bytes_read, 0);
        assert_eq!(reader.largest_request, 0);
    }

    #[test]
    fn streaming_content_rejects_truncation_after_metadata() {
        let mut reader = ControlledReader::new(b"ab");
        assert!(!content_matches_reader(&mut reader, 3, b"abc"));
        assert_eq!(reader.calls, 2);
        assert_eq!(reader.bytes_read, 2);
    }

    #[test]
    fn streaming_content_rejects_growth_with_only_one_sentinel_byte() {
        for (data, expected) in [
            (b"abc-trailer".as_slice(), b"abc".as_slice()),
            (b"x".as_slice(), b"".as_slice()),
        ] {
            let mut reader = ControlledReader::new(data);
            assert!(!content_matches_reader(
                &mut reader,
                expected.len() as u64,
                expected
            ));
            assert_eq!(reader.bytes_read, expected.len() + 1);
            assert_eq!(reader.remaining.len(), data.len() - expected.len() - 1);
        }
    }

    #[test]
    fn streaming_content_rejects_read_errors_before_or_during_comparison() {
        for failing_call in [1, 2] {
            let mut reader = ControlledReader::new(b"abc");
            reader.max_chunk = 1;
            reader.fail_on_call = Some(failing_call);
            assert!(!content_matches_reader(&mut reader, 3, b"abc"));
        }
    }

    #[test]
    fn streaming_content_rejects_an_error_while_checking_eof() {
        let mut reader = ControlledReader::new(b"abc");
        reader.fail_on_call = Some(2);
        assert!(!content_matches_reader(&mut reader, 3, b"abc"));
        assert_eq!(reader.bytes_read, 3);
    }

    #[test]
    fn streaming_content_retries_interrupted_data_reads_and_eof() {
        for max_chunk in [usize::MAX, 1] {
            let mut reader = ControlledReader::new(b"abc");
            reader.max_chunk = max_chunk;
            reader.interrupt_calls = &[1, 3, 5, 7];
            assert!(content_matches_reader(&mut reader, 3, b"abc"));
            assert_eq!(reader.bytes_read, 3);
            assert_eq!(reader.calls, if max_chunk == 1 { 8 } else { 4 });
        }
    }

    #[test]
    fn streaming_content_accepts_short_reads_without_growing_the_buffer() {
        let bytes = vec![b'x'; CONTENT_COMPARE_CHUNK_BYTES * 2 + 9];
        let mut reader = ControlledReader::new(&bytes);
        reader.max_chunk = 7;
        assert!(content_matches_reader(
            &mut reader,
            bytes.len() as u64,
            &bytes
        ));
        assert_eq!(reader.largest_request, CONTENT_COMPARE_CHUNK_BYTES);
        assert_eq!(reader.bytes_read, bytes.len());
        assert!(reader.calls > 4);
    }

    #[test]
    fn streaming_content_noop_preserves_main_timestamp_and_entire_backup_ring() {
        let directory = TempDirectory::new();
        let path = directory.path().join("layout.json");
        std::fs::write(&path, b"unchanged").unwrap();
        for slot in 0..BACKUP_SLOTS {
            std::fs::write(backup_path(&path, slot), format!("backup-{slot}")).unwrap();
        }
        let paths: Vec<_> = std::iter::once(path.clone())
            .chain((0..BACKUP_SLOTS).map(|slot| backup_path(&path, slot)))
            .collect();
        let before: Vec<_> = paths.iter().map(|path| fingerprint(path)).collect();
        assert!(!write_durable(&path, b"unchanged").unwrap());
        assert!(!write_atomic(&path, b"unchanged").unwrap());
        let after: Vec<_> = paths.iter().map(|path| fingerprint(path)).collect();
        assert_eq!(before, after);
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            BACKUP_SLOTS + 1
        );
    }

    #[test]
    fn streaming_content_rejects_a_missing_file_and_directory() {
        let directory = TempDirectory::new();
        assert!(!content_matches(&directory.path().join("missing"), b""));
        assert!(!content_matches(directory.path(), b""));
    }

    #[cfg(unix)]
    #[test]
    fn streaming_content_follows_a_symlink_to_a_regular_file() {
        let directory = TempDirectory::new();
        let target = directory.path().join("target");
        let link = directory.path().join("link");
        std::fs::write(&target, b"same").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(content_matches(&link, b"same"));
        assert!(!content_matches(&link, b"different"));
        assert_eq!(std::fs::read(&target).unwrap(), b"same");
    }

    #[cfg(unix)]
    #[test]
    fn streaming_content_rejects_a_fifo_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = TempDirectory::new();
        let fifo = directory.path().join("fifo");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c owns a NUL-terminated path valid for this call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sender.send(content_matches(&fifo, b""));
        });
        // No scoped join on failure: a regression to blocking open must fail
        // this test promptly rather than keep CI waiting for a FIFO writer.
        assert!(!receiver.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod special_file_write_tests {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{backup_path, write_atomic, write_durable, BACKUP_SLOTS};

    type Writer = fn(&Path, &[u8]) -> io::Result<bool>;

    fn writers() -> [Writer; 2] {
        [write_durable, write_atomic]
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tc-special-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Entry {
        kind: &'static str,
        bytes: Option<Vec<u8>>,
        link: Option<PathBuf>,
        modified: SystemTime,
        readonly: bool,
        #[cfg(unix)]
        mode: u32,
        #[cfg(unix)]
        inode: u64,
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Entry)> {
        fn visit(root: &Path, path: &Path, entries: &mut Vec<(PathBuf, Entry)>) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            let is_link = metadata.file_type().is_symlink();
            let kind = if is_link {
                "link"
            } else if metadata.is_file() {
                "file"
            } else if metadata.is_dir() {
                "directory"
            } else {
                "special"
            };
            #[cfg(unix)]
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            entries.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                Entry {
                    kind,
                    // Never open a FIFO or follow a link while fingerprinting.
                    bytes: (kind == "file").then(|| std::fs::read(path).unwrap()),
                    link: is_link.then(|| std::fs::read_link(path).unwrap()),
                    modified: metadata.modified().unwrap(),
                    readonly: metadata.permissions().readonly(),
                    #[cfg(unix)]
                    mode: metadata.permissions().mode(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                },
            ));
            if kind == "directory" {
                for child in std::fs::read_dir(path).unwrap() {
                    visit(root, &child.unwrap().path(), entries);
                }
            }
        }
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    fn seed_ring(path: &Path) {
        for slot in 0..BACKUP_SLOTS {
            std::fs::write(backup_path(path, slot), format!("backup-{slot}")).unwrap();
        }
    }

    #[test]
    fn write_rejects_directory_destinations_before_any_mutation() {
        for writer in writers() {
            let directory = TempDirectory::new();
            let path = directory.path().join("state");
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("child"), b"keep").unwrap();
            seed_ring(&path);
            let before = snapshot(directory.path());
            let error = writer(&path, b"new").unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("state destination"));
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[test]
    fn changed_durable_write_rejects_every_directory_backup_before_mutation() {
        for main_exists in [false, true] {
            for slot in 0..BACKUP_SLOTS {
                let directory = TempDirectory::new();
                let path = directory.path().join("state");
                if main_exists {
                    std::fs::write(&path, b"old").unwrap();
                }
                seed_ring(&path);
                let invalid = backup_path(&path, slot);
                std::fs::remove_file(&invalid).unwrap();
                std::fs::create_dir(&invalid).unwrap();
                std::fs::write(invalid.join("child"), b"keep").unwrap();
                let before = snapshot(directory.path());
                let error = write_durable(&path, b"new").unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert!(error.to_string().contains("state backup"));
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[test]
    fn generic_noop_preserves_anomalous_backup_slots() {
        for slot in 0..BACKUP_SLOTS {
            let directory = TempDirectory::new();
            let path = directory.path().join("state");
            std::fs::write(&path, b"same").unwrap();
            seed_ring(&path);
            let invalid = backup_path(&path, slot);
            std::fs::remove_file(&invalid).unwrap();
            std::fs::create_dir(&invalid).unwrap();
            let before = snapshot(directory.path());
            for writer in writers() {
                assert!(!writer(&path, b"same").unwrap());
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[test]
    fn atomic_write_does_not_validate_or_mutate_an_unrelated_backup() {
        for main_exists in [false, true] {
            for slot in 0..BACKUP_SLOTS {
                let directory = TempDirectory::new();
                let path = directory.path().join("state");
                if main_exists {
                    std::fs::write(&path, b"old").unwrap();
                }
                let backup = backup_path(&path, slot);
                std::fs::create_dir(&backup).unwrap();
                std::fs::write(backup.join("child"), b"keep").unwrap();
                let before = snapshot(&backup);
                assert!(write_atomic(&path, b"new").unwrap());
                assert_eq!(std::fs::read(&path).unwrap(), b"new");
                assert_eq!(snapshot(&backup), before);
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
            }
        }
    }

    #[test]
    fn valid_creation_rotation_spacing_and_noop_remain_compatible() {
        let directory = TempDirectory::new();
        let path = directory.path().join("nested").join("state");
        assert!(write_durable(&path, b"first").unwrap());
        assert!(!backup_path(&path, 0).exists());
        assert!(write_durable(&path, b"second").unwrap());
        let newest = backup_path(&path, 0);
        assert_eq!(std::fs::read(&newest).unwrap(), b"first");
        let before = snapshot(directory.path());
        assert!(!write_durable(&path, b"second").unwrap());
        assert_eq!(snapshot(directory.path()), before);
        let backup_before = snapshot(&newest);
        assert!(write_durable(&path, b"third").unwrap());
        assert_eq!(snapshot(&newest), backup_before);
        assert!(!backup_path(&path, 1).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"third");
    }

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path_c = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path_c owns a NUL-terminated path valid for this call.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    }

    #[cfg(unix)]
    fn bounded_write(writer: Writer, path: &Path, bytes: &'static [u8]) -> io::Result<bool> {
        use std::sync::mpsc;
        use std::time::Duration;
        let path = path.to_path_buf();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sender.send(writer(&path, bytes));
        });
        // On timeout, dropping this JoinHandle detaches instead of waiting for
        // a blocked FIFO open. Fixtures never supply a reader/writer peer.
        let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        result
    }

    #[cfg(unix)]
    #[test]
    fn write_rejects_fifo_destinations_without_opening_or_replacing_them() {
        for has_backups in [false, true] {
            for writer in writers() {
                let directory = TempDirectory::new();
                let path = directory.path().join("state");
                make_fifo(&path);
                if has_backups {
                    seed_ring(&path);
                }
                let before = snapshot(directory.path());
                let error = bounded_write(writer, &path, b"new").unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn changed_durable_write_rejects_fifo_backups_but_noop_preserves_them() {
        for slot in 0..BACKUP_SLOTS {
            let directory = TempDirectory::new();
            let path = directory.path().join("state");
            std::fs::write(&path, b"same").unwrap();
            seed_ring(&path);
            let fifo = backup_path(&path, slot);
            std::fs::remove_file(&fifo).unwrap();
            make_fifo(&fifo);
            let before = snapshot(directory.path());
            assert!(!bounded_write(write_durable, &path, b"same").unwrap());
            assert_eq!(snapshot(directory.path()), before);
            let error = bounded_write(write_durable, &path, b"new").unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(snapshot(directory.path()), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn changed_durable_write_rejects_regular_and_dangling_backup_links() {
        use std::os::unix::fs::symlink;
        for target_exists in [false, true] {
            for slot in 0..BACKUP_SLOTS {
                let directory = TempDirectory::new();
                let path = directory.path().join("state");
                std::fs::write(&path, b"same").unwrap();
                seed_ring(&path);
                let target = directory.path().join("outside");
                if target_exists {
                    std::fs::write(&target, b"outside").unwrap();
                }
                let link = backup_path(&path, slot);
                std::fs::remove_file(&link).unwrap();
                symlink(&target, &link).unwrap();
                let before = snapshot(directory.path());
                assert!(!write_durable(&path, b"same").unwrap());
                assert_eq!(snapshot(directory.path()), before);
                let error = write_durable(&path, b"new").unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn generic_writes_keep_regular_main_link_noop_and_changed_write_behavior() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        for (writer, has_ring) in [
            (write_durable as Writer, true),
            (write_atomic as Writer, false),
        ] {
            let directory = TempDirectory::new();
            let target = directory.path().join("outside");
            let path = directory.path().join("state");
            std::fs::write(&target, b"same").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
            symlink(&target, &path).unwrap();
            let before = snapshot(directory.path());
            assert!(!writer(&path, b"same").unwrap());
            assert_eq!(snapshot(directory.path()), before);
            let target_before = snapshot(&target);
            assert!(writer(&path, b"new").unwrap());
            assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
            assert_eq!(snapshot(&target), target_before);
            // The durable writer retains the old target bytes; atomic never
            // creates a ring. Both replace only the main link entry.
            let backup = backup_path(&path, 0);
            if has_ring {
                assert_eq!(std::fs::read(&backup).unwrap(), b"same");
            } else {
                assert!(!backup.exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_rejects_dangling_and_non_regular_main_links_before_mutation() {
        use std::os::unix::fs::symlink;
        for kind in ["missing", "directory", "fifo"] {
            for writer in writers() {
                let directory = TempDirectory::new();
                let target = directory.path().join("outside");
                match kind {
                    "missing" => {}
                    "directory" => std::fs::create_dir(&target).unwrap(),
                    "fifo" => make_fifo(&target),
                    _ => unreachable!(),
                }
                let path = directory.path().join("state");
                symlink(&target, &path).unwrap();
                seed_ring(&path);
                let before = snapshot(directory.path());
                let error = bounded_write(writer, &path, b"new").unwrap_err();
                assert_eq!(
                    error.kind(),
                    if kind == "missing" {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::InvalidInput
                    }
                );
                assert_eq!(snapshot(directory.path()), before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_noop_keeps_its_stricter_backup_link_protection() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = TempDirectory::new();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("state");
        let target = directory.path().join("outside");
        for file in [&path, &target] {
            std::fs::write(file, b"same").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        symlink(&target, backup_path(&path, 0)).unwrap();
        let before = snapshot(directory.path());
        assert!(!write_durable(&path, b"same").unwrap());
        assert_eq!(snapshot(directory.path()), before);
        assert!(super::write_private_durable(&path, b"same").is_err());
        assert_eq!(snapshot(directory.path()), before);
    }
}
