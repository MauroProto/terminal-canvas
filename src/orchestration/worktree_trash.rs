//! Trash diferido para worktrees (P1.9).
//!
//! En vez de borrar un worktree en el acto (lento y destructivo), se renombra a
//! `<repo>/.terminalcanvas-trash/wt-<epoch>-<nonce8hex>` y el borrado recursivo
//! real lo hace después el worker, serializado. Si el rename falla (cross-volume)
//! se devuelve `None` y quien llama sigue con el borrado directo.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const TRASH_DIR: &str = ".terminalcanvas-trash";

/// Edad mínima de una entrada del trash antes de que el sweep la considere
/// stale y la borre.
pub const STALE_TRASH_AGE_SECS: u64 = 5 * 60;

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn nonce8() -> String {
    let n = uuid::Uuid::new_v4();
    n.simple().to_string()[..8].to_owned()
}

/// Directorio de trash de un repo.
pub fn trash_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(TRASH_DIR)
}

/// Renombra el worktree al trash. Devuelve el path dentro del trash si el
/// rename funcionó, o `None` si falló (cross-volume) para caer a borrado directo.
pub fn move_to_trash(repo_root: &Path, worktree: &Path) -> Option<PathBuf> {
    let trash = trash_dir(repo_root);
    std::fs::create_dir_all(&trash).ok()?;
    let name = format!("wt-{}-{}", epoch_now(), nonce8());
    let dest = trash.join(name);
    std::fs::rename(worktree, &dest).ok()?;
    Some(dest)
}

/// ¿El nombre matchea el patrón de entradas del trash (`wt-<epoch>-<hex8>`)?
pub fn is_trash_entry(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("wt-") else {
        return false;
    };
    let mut parts = rest.splitn(2, '-');
    let Some(epoch) = parts.next() else {
        return false;
    };
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        Some(hex) => hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        None => false,
    }
}

/// Época embebida en el nombre de una entrada del trash, si la hay.
fn epoch_of(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("wt-")?;
    let epoch = rest.split('-').next()?;
    epoch.parse().ok()
}

/// Borra entradas stale del trash (edad > 5 min) al abrir un workspace.
/// Nunca sigue symlinks: si una entrada es un symlink, solo se quita el link.
pub fn sweep_stale_trash(repo_root: &Path) -> usize {
    let trash = trash_dir(repo_root);
    let Ok(entries) = std::fs::read_dir(&trash) else {
        return 0;
    };
    let now = epoch_now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_trash_entry(&name) {
            continue;
        }
        let age = now.saturating_sub(epoch_of(&name).unwrap_or(0));
        if age <= STALE_TRASH_AGE_SECS {
            continue;
        }
        let path = entry.path();
        // symlink_metadata no sigue el link: si es symlink, quitamos solo el
        // link; si es dir real, borrado recursivo.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let ok = if meta.file_type().is_symlink() {
            std::fs::remove_file(&path).is_ok()
        } else {
            std::fs::remove_dir_all(&path).is_ok()
        };
        if ok {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::{is_trash_entry, move_to_trash, sweep_stale_trash, trash_dir};
    use std::fs;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wt-trash-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn trash_entry_pattern_recognizes_valid_names() {
        assert!(is_trash_entry("wt-1722940000-0123abcd"));
        assert!(!is_trash_entry("wt-1722940000-0123abc")); // 7 hex
        assert!(!is_trash_entry("wt-1722940000-0123abcde")); // 9 hex
        assert!(!is_trash_entry("wt-1722940000-zzzzzzzz")); // no hex
        assert!(!is_trash_entry("wt--0123abcd")); // sin epoch
        assert!(!is_trash_entry("otra-cosa"));
    }

    #[test]
    fn move_to_trash_renames_into_the_trash_dir() {
        let repo = temp("repo");
        let wt = repo.join("worktrees").join("feat");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join("archivo.txt"), "x").unwrap();

        let dest = move_to_trash(&repo, &wt).expect("rename dentro del mismo fs");
        assert!(!wt.exists(), "el worktree ya no está en su lugar");
        assert!(dest.exists(), "está en el trash");
        assert!(dest.starts_with(trash_dir(&repo)));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn sweep_removes_only_stale_entries() {
        let repo = temp("sweep");
        let trash = trash_dir(&repo);
        fs::create_dir_all(&trash).unwrap();

        // Una entrada vieja (epoch 0) y una fresca (ahora).
        let stale = trash.join("wt-0-00000000");
        fs::create_dir_all(&stale).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let fresh = trash.join(format!("wt-{now}-11111111"));
        fs::create_dir_all(&fresh).unwrap();

        let removed = sweep_stale_trash(&repo);
        assert_eq!(removed, 1, "solo la stale");
        assert!(!stale.exists());
        assert!(fresh.exists(), "la fresca se conserva");
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn sweep_does_not_follow_symlinks() {
        let repo = temp("symlink");
        let trash = trash_dir(&repo);
        fs::create_dir_all(&trash).unwrap();
        // Un dir real AFUERA del trash y un symlink stale que apunta a él.
        let outside = temp("outside-target");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("importante.txt"), "no borrar").unwrap();

        #[cfg(unix)]
        {
            let link = trash.join("wt-0-22222222");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let removed = sweep_stale_trash(&repo);
            assert_eq!(removed, 1, "se quitó el link");
            assert!(!link.exists(), "el link desaparece");
            assert!(
                outside.join("importante.txt").exists(),
                "el target NO se toca"
            );
        }
        let _ = fs::remove_dir_all(&outside);
        let _ = fs::remove_dir_all(&repo);
    }
}
