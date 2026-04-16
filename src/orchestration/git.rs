use std::path::{Path, PathBuf};
use std::process::Command;

use super::manager::DiffStats;

#[derive(Debug, Clone)]
pub(super) struct GitObservation {
    pub(super) repo_root: PathBuf,
    pub(super) branch: String,
    pub(super) dirty: bool,
    pub(super) changed_files: Vec<PathBuf>,
    pub(super) diff_stats: DiffStats,
}

pub(super) fn inspect_git_state(path: &Path) -> Option<GitObservation> {
    let repo_root = git_repo_root(path)?;
    let branch = git_stdout(&repo_root, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "detached".to_owned());
    let status = git_stdout(&repo_root, &["status", "--porcelain"])?;
    let changed_files = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let dirty = !changed_files.is_empty();
    let diff_stats = parse_diff_stats(git_stdout(&repo_root, &["diff", "--shortstat", "HEAD"]));
    Some(GitObservation {
        repo_root,
        branch,
        dirty,
        changed_files,
        diff_stats,
    })
}

pub(super) fn git_repo_root(path: &Path) -> Option<PathBuf> {
    git_stdout(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

pub(super) fn parse_diff_stats(raw: Option<String>) -> DiffStats {
    let Some(raw) = raw else {
        return DiffStats::default();
    };
    let mut stats = DiffStats::default();
    for segment in raw.split(',') {
        let segment = segment.trim();
        if let Some(value) = segment.split_whitespace().next() {
            if segment.contains("file changed") || segment.contains("files changed") {
                stats.files_changed = value.parse().unwrap_or(0);
            } else if segment.contains("insertion") {
                stats.insertions = value.parse().unwrap_or(0);
            } else if segment.contains("deletion") {
                stats.deletions = value.parse().unwrap_or(0);
            }
        }
    }
    stats
}

pub(super) fn create_git_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
) -> anyhow::Result<()> {
    if worktree_path.exists() {
        return Ok(());
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "-b", branch])
        .arg(worktree_path)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}
