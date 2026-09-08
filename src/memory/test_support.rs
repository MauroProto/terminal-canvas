//! Fixtures de git y directorios temporales para los tests del Memory Hub.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn temp_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "tc-memory-{}-{}-{}",
        Uuid::new_v4().as_simple(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

pub fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, contents).expect("write");
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", os_devnull())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
    if !output.status.success() {
        panic!(
            "git {args:?} failed in {}: {}\n{}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn os_devnull() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

pub fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    write_file(&dir.join("README.md"), "seed\n");
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-m", "init"]);
    git(
        dir,
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/acme/app.git",
        ],
    );
}

pub fn make_worktree(repo: &Path, dest: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            dest.to_str().expect("utf8 path"),
        ],
    );
    git(dest, &["checkout", "-b", branch]);
}

pub fn clone_repo(origin: &Path, dest: &Path) {
    let status = Command::new("git")
        .args([
            "clone",
            "--quiet",
            origin.to_str().expect("utf8"),
            dest.to_str().expect("utf8"),
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", os_devnull())
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .expect("clone");
    assert!(status.success(), "git clone failed");
    git(dest, &["config", "user.name", "Test"]);
    git(dest, &["config", "user.email", "test@example.com"]);
    let _ = git(
        dest,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.com/acme/app.git",
        ],
    );
}
