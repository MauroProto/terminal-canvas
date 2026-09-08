//! Recoverable worktree moves. Neither archival nor restore deletes source files.
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
    let managed = std::fs::canonicalize(root.join(".terminalcanvas/worktrees")).ok()?;
    let original_parent = std::fs::canonicalize(record.original.parent()?).ok()?;
    original_parent
        .starts_with(managed)
        .then_some(record.original)
}

pub(super) fn restore(root: &Path, archived: &Path, live_paths: &[PathBuf]) -> anyhow::Result<()> {
    require_inactive(archived, live_paths)?;
    let original = original_for(root, archived).ok_or_else(|| {
        anyhow::anyhow!("No hay un registro de recuperación válido para este worktree")
    })?;
    anyhow::ensure!(
        !original.exists(),
        "La ubicación original ya existe; no se reemplazó ningún archivo"
    );
    let registered: Vec<_> = super::code_diff::list_git_worktrees(root)
        .into_iter()
        .map(|wt| wt.path)
        .collect();
    super::worktree_removal_safety::check_recursive_delete(archived, root, &registered)
        .map_err(|guard| anyhow::anyhow!(guard.0))?;
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
        assert!(archive(&root, &wt, &[wt.clone()]).is_err());
        let archived = archive(&root, &wt, &[]).unwrap();
        assert!(!wt.exists());
        assert_eq!(
            std::fs::read_to_string(archived.join("new.txt")).unwrap(),
            "uncommitted"
        );
        assert!(restore(&root, &archived, &[archived.clone()]).is_err());
        restore(&root, &archived, &[]).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("new.txt")).unwrap(),
            "uncommitted"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
