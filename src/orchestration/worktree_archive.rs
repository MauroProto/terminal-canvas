//! Recoverable worktree moves. Neither archival nor restore deletes source files.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const ARCHIVE_DIR: &str = ".terminalcanvas-archive";

#[derive(Debug, Serialize, Deserialize)]
struct ArchiveRecord {
    original: PathBuf,
    archived: PathBuf,
    had_changes: bool,
}

fn record_path(archived: &Path) -> PathBuf {
    archived.with_extension("restore.json")
}

fn git(root: &Path, args: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    let output = super::git::run_with_timeout(&mut command, Duration::from_secs(30))?;
    anyhow::ensure!(
        output.status.success(),
        "Git: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn require_inactive(path: &Path, live_paths: &[PathBuf]) -> anyhow::Result<()> {
    let path = std::fs::canonicalize(path)?;
    for live in live_paths {
        if let Ok(live) = std::fs::canonicalize(live) {
            anyhow::ensure!(!live.starts_with(&path), "Hay una terminal activa en este worktree. Cerrala antes de archivarlo o restaurarlo.");
        }
    }
    Ok(())
}

fn archive_root(root: &Path) -> anyhow::Result<PathBuf> {
    let archive = root.join(ARCHIVE_DIR);
    if let Ok(metadata) = std::fs::symlink_metadata(&archive) {
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "El directorio de archivos no puede ser un symlink"
        );
    }
    std::fs::create_dir_all(&archive)?;
    let canonical = std::fs::canonicalize(&archive)?;
    anyhow::ensure!(
        canonical.parent() == Some(std::fs::canonicalize(root)?.as_path()),
        "Directorio de archivos fuera del repositorio"
    );
    Ok(archive)
}

pub(super) fn archive(root: &Path, path: &Path, live_paths: &[PathBuf]) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        super::code_diff::is_managed_worktree(root, path),
        "El worktree no pertenece al directorio administrado"
    );
    let registered: Vec<_> = super::code_diff::list_git_worktrees(root)
        .into_iter()
        .map(|wt| wt.path)
        .collect();
    super::worktree_removal_safety::check_recursive_delete(path, root, &registered)
        .map_err(|guard| anyhow::anyhow!(guard.0))?;
    require_inactive(path, live_paths)?;
    // A failed status aborts. Dirty files are preserved by git worktree move,
    // including ignored/untracked files; no force removal or prune is used.
    let status = git(
        path,
        &["status".as_ref(), "--porcelain=v1".as_ref(), "-z".as_ref()],
    )?;
    let archived = archive_root(root)?.join(format!("wt-{}", uuid::Uuid::new_v4()));
    let record = ArchiveRecord {
        original: std::fs::canonicalize(path)?,
        archived: archived.clone(),
        had_changes: !status.is_empty(),
    };
    crate::state::durable_write::write_durable(
        &record_path(&archived),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    git(
        root,
        &[
            "worktree".as_ref(),
            "move".as_ref(),
            path.as_os_str(),
            archived.as_os_str(),
        ],
    )?;
    Ok(archived)
}

pub(super) fn original_for(root: &Path, archived: &Path) -> Option<PathBuf> {
    let parent = std::fs::canonicalize(archived.parent()?).ok()?;
    if parent != std::fs::canonicalize(root.join(ARCHIVE_DIR)).ok()? {
        return None;
    }
    let record: ArchiveRecord =
        serde_json::from_slice(&std::fs::read(record_path(archived)).ok()?).ok()?;
    if std::fs::canonicalize(&record.archived).ok()? != std::fs::canonicalize(archived).ok()? {
        return None;
    }
    validated_restore_parent(root, &record.original)?;
    Some(record.original)
}

/// The archive records a canonical original path. An absent immediate
/// worktree directory is recoverable only while its own repo-local parent
/// still exists; listing never creates it.
fn validated_restore_parent(root: &Path, original: &Path) -> Option<PathBuf> {
    original.file_name()?;
    let parent = original.parent()?;
    let lifecycle = std::fs::canonicalize(root).ok()?.join(".terminalcanvas");
    if !std::fs::symlink_metadata(&lifecycle).ok()?.is_dir()
        || std::fs::canonicalize(&lifecycle).ok()? != lifecycle
    {
        return None;
    }
    let managed = lifecycle.join("worktrees");
    if !parent.starts_with(&managed) {
        return None;
    }
    match std::fs::symlink_metadata(&managed) {
        Ok(metadata) if metadata.is_dir() => {
            let canonical = std::fs::canonicalize(parent).ok()?;
            (canonical == parent && canonical.is_dir()).then_some(canonical)
        }
        Err(error) if error.kind() == ErrorKind::NotFound && parent == managed => Some(managed),
        _ => None,
    }
}

fn require_unused_destination(original: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(original) {
        Ok(_) => anyhow::bail!("La ubicación original ya existe; no se reemplazó ningún archivo"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn prepare_restore_parent(root: &Path, original: &Path) -> anyhow::Result<()> {
    let parent = validated_restore_parent(root, original).ok_or_else(|| {
        anyhow::anyhow!("La carpeta de destino no está administrada por el repositorio")
    })?;
    if !parent.is_dir() {
        // Only the immediate managed directory may be missing, never an
        // arbitrary chain. A concurrent creation is reported rather than used.
        std::fs::create_dir(&parent)?;
    }
    anyhow::ensure!(
        std::fs::canonicalize(&parent)? == parent && std::fs::symlink_metadata(&parent)?.is_dir(),
        "La carpeta de destino cambió; no se movió el worktree"
    );
    require_unused_destination(original)
}

pub(super) fn restore(root: &Path, archived: &Path, live_paths: &[PathBuf]) -> anyhow::Result<()> {
    require_inactive(archived, live_paths)?;
    let original = original_for(root, archived).ok_or_else(|| {
        anyhow::anyhow!("No hay un registro de recuperación válido para este worktree")
    })?;
    require_unused_destination(&original)?;
    let registered: Vec<_> = super::code_diff::list_git_worktrees(root)
        .into_iter()
        .map(|wt| wt.path)
        .collect();
    super::worktree_removal_safety::check_recursive_delete(archived, root, &registered)
        .map_err(|guard| anyhow::anyhow!(guard.0))?;
    prepare_restore_parent(root, &original)?;
    git(
        root,
        &[
            "worktree".as_ref(),
            "move".as_ref(),
            archived.as_os_str(),
            original.as_os_str(),
        ],
    )?;
    let _ = std::fs::remove_file(record_path(archived));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_checkout_stays_visible_without_creating_its_missing_managed_parent() {
        let temporary =
            std::env::temp_dir().join(format!("archive-missing-parent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temporary).unwrap();
        let root = temporary.canonicalize().unwrap();
        let archive_dir = root.join(ARCHIVE_DIR);
        let archived = archive_dir.join("wt-retained");
        let managed = root.join(".terminalcanvas/worktrees");
        let original = managed.join("agent checkout");
        std::fs::create_dir_all(&archived).unwrap();
        std::fs::create_dir(managed.parent().unwrap()).unwrap();
        let record = ArchiveRecord {
            original: original.clone(),
            archived: archived.clone(),
            had_changes: true,
        };
        std::fs::write(record_path(&archived), serde_json::to_vec(&record).unwrap()).unwrap();

        assert_eq!(
            original_for(&root, &archived).as_deref(),
            Some(original.as_path())
        );
        assert!(!managed.exists(), "listing must not change the repository");
        let error = restore(&root, &archived, std::slice::from_ref(&archived)).unwrap_err();
        assert!(error.to_string().contains("terminal activa"));
        assert!(
            !managed.exists(),
            "a blocked restore must not create the parent"
        );
        // Test explicitly preparing the validated destination without moving
        // a worktree or consuming its archive record.
        prepare_restore_parent(&root, &original).unwrap();
        assert!(managed.is_dir());
        assert!(!original.exists());
        assert!(record_path(&archived).is_file());
        assert!(archived.is_dir());
        let sibling = managed.join("occupied checkout");
        std::fs::write(&sibling, "existing data").unwrap();
        assert!(prepare_restore_parent(&root, &sibling).is_err());
        assert_eq!(std::fs::read_to_string(&sibling).unwrap(), "existing data");
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn dirty_worktree_round_trips_without_losing_files_and_rejects_live_terminals() {
        let root = std::env::temp_dir().join(format!("archive-roundtrip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            let result = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        run(&["init", "-q"]);
        std::fs::write(root.join("base.txt"), "base").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        let wt = root.join(".terminalcanvas/worktrees/agent");
        run(&["worktree", "add", "-b", "agent", wt.to_str().unwrap()]);
        std::fs::write(wt.join("new.txt"), "uncommitted").unwrap();
        assert!(archive(&root, &wt, std::slice::from_ref(&wt)).is_err());
        let archived = archive(&root, &wt, &[]).unwrap();
        assert!(!wt.exists());
        assert_eq!(
            std::fs::read_to_string(archived.join("new.txt")).unwrap(),
            "uncommitted"
        );
        assert!(restore(&root, &archived, std::slice::from_ref(&archived)).is_err());
        restore(&root, &archived, &[]).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("new.txt")).unwrap(),
            "uncommitted"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
