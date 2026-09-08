//! Identidad de proyecto a partir de un cwd.
//!
//! Dos worktrees del mismo repo comparten `git --git-common-dir`. Un clone
//! distinto del mismo remote **no** se fusiona: el fingerprint del remote es
//! sólo una señal, nunca la clave.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use super::model::{IdentityKind, ResolvedLocation};

/// Resuelve la identidad durable de un directorio de trabajo.
///
/// No acepta un `project_id` aportado por un agente: el daemon calcula el
/// scope desde el cwd real.
pub fn resolve_location(cwd: &Path) -> ResolvedLocation {
    let cwd = canonicalize_best_effort(cwd);
    if let Some(git) = inspect_git(&cwd) {
        return ResolvedLocation {
            cwd,
            identity_kind: IdentityKind::GitCommonDir,
            identity_value: git.common_dir,
            worktree_root: Some(git.toplevel),
            remote_fingerprint: git.remote_fingerprint,
        };
    }
    ResolvedLocation {
        identity_value: cwd.to_string_lossy().into_owned(),
        cwd,
        identity_kind: IdentityKind::WorkspaceRoot,
        worktree_root: None,
        remote_fingerprint: None,
    }
}

struct GitIdentity {
    toplevel: PathBuf,
    common_dir: String,
    remote_fingerprint: Option<String>,
}

fn inspect_git(cwd: &Path) -> Option<GitIdentity> {
    let toplevel = git_stdout(cwd, &["rev-parse", "--show-toplevel"])?;
    let toplevel = canonicalize_best_effort(Path::new(&toplevel));
    let common_raw = git_stdout(&toplevel, &["rev-parse", "--git-common-dir"])?;
    let common_path = {
        let raw = PathBuf::from(&common_raw);
        if raw.is_absolute() {
            canonicalize_best_effort(&raw)
        } else {
            canonicalize_best_effort(&toplevel.join(raw))
        }
    };
    let remote_fingerprint = git_stdout(&toplevel, &["remote", "get-url", "origin"])
        .or_else(|| first_remote_url(&toplevel))
        .map(|url| fingerprint_remote(&url));
    Some(GitIdentity {
        toplevel,
        common_dir: common_path.to_string_lossy().into_owned(),
        remote_fingerprint,
    })
}

fn first_remote_url(repo: &Path) -> Option<String> {
    let remotes = git_stdout(repo, &["remote"])?;
    let first = remotes.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    git_stdout(repo, &["remote", "get-url", first])
}

/// Huella del remote sin credenciales. Nunca se usa como identidad.
pub fn fingerprint_remote(url: &str) -> String {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("ssh://"))
        .or_else(|| trimmed.strip_prefix("git://"))
        .unwrap_or(trimmed);
    let without_user = without_scheme
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(without_scheme);
    // git@host:path y host/path
    let normalized = without_user
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .replace(':', "/");
    normalized.to_ascii_lowercase()
}

pub fn canonicalize_best_effort(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_lexically(path))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{fingerprint_remote, resolve_location, IdentityKind};
    use crate::memory::test_support::{
        clone_repo, init_git_repo, make_worktree, temp_dir, write_file,
    };

    #[test]
    fn two_worktrees_share_the_git_common_dir() {
        let root = temp_dir();
        let repo = root.join("repo");
        init_git_repo(&repo);
        let worktree = root.join("wt-a");
        make_worktree(&repo, &worktree, "agent-a");

        let main = resolve_location(&repo);
        let other = resolve_location(&worktree);
        assert_eq!(main.identity_kind, IdentityKind::GitCommonDir);
        assert_eq!(main.identity_value, other.identity_value);
        assert_ne!(
            main.worktree_root, other.worktree_root,
            "cada worktree conserva su toplevel"
        );
        assert_ne!(main.task_identity(), other.task_identity());
    }

    #[test]
    fn a_second_repo_does_not_share_identity() {
        let root = temp_dir();
        let one = root.join("one");
        let two = root.join("two");
        init_git_repo(&one);
        init_git_repo(&two);
        let left = resolve_location(&one);
        let right = resolve_location(&two);
        assert_ne!(left.identity_value, right.identity_value);
    }

    #[test]
    fn an_unlinked_clone_has_its_own_common_dir() {
        let root = temp_dir();
        let origin = root.join("origin");
        init_git_repo(&origin);
        let clone = root.join("clone");
        clone_repo(&origin, &clone);
        crate::memory::test_support::git(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.com/acme/app.git",
            ],
        );
        let origin_id = resolve_location(&origin);
        let clone_id = resolve_location(&clone);
        assert_eq!(origin_id.identity_kind, IdentityKind::GitCommonDir);
        assert_eq!(clone_id.identity_kind, IdentityKind::GitCommonDir);
        assert_ne!(origin_id.identity_value, clone_id.identity_value);
        assert_eq!(origin_id.remote_fingerprint, clone_id.remote_fingerprint);
    }

    #[test]
    fn a_folder_without_git_uses_the_canonical_path() {
        let root = temp_dir();
        write_file(&root.join("notes.txt"), "hi");
        let resolved = resolve_location(&root);
        assert_eq!(resolved.identity_kind, IdentityKind::WorkspaceRoot);
        assert!(resolved.cwd.exists());
        assert_eq!(
            resolved.identity_value,
            resolved.cwd.to_string_lossy().as_ref()
        );
        assert_eq!(resolved.worktree_root, None);
    }

    #[test]
    fn remote_fingerprints_drop_credentials_and_git_suffix() {
        assert_eq!(
            fingerprint_remote("https://user:token@github.com/Acme/App.git"),
            "github.com/acme/app"
        );
        assert_eq!(
            fingerprint_remote("git@github.com:Acme/App.git"),
            "github.com/acme/app"
        );
    }
}
