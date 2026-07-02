use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

use uuid::Uuid;

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

#[derive(Debug)]
pub(super) struct GitInspectResult {
    pub(super) session_id: Uuid,
    pub(super) observation: Option<GitObservation>,
}

/// Runs git inspections on a dedicated worker thread so the UI thread never
/// blocks on subprocess I/O; results are polled on later frames.
#[derive(Debug, Default)]
pub(super) struct GitInspector {
    worker: Option<GitInspectorWorker>,
    in_flight: HashSet<Uuid>,
}

#[derive(Debug)]
struct GitInspectorWorker {
    request_tx: Sender<(Uuid, PathBuf)>,
    result_rx: Receiver<GitInspectResult>,
}

impl GitInspector {
    pub(super) fn request(&mut self, session_id: Uuid, cwd: PathBuf) {
        if self.in_flight.contains(&session_id) {
            return;
        }
        if self.worker.is_none() {
            self.worker = spawn_inspector_worker();
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if worker.request_tx.send((session_id, cwd)).is_ok() {
            self.in_flight.insert(session_id);
        } else {
            self.worker = None;
            self.in_flight.clear();
        }
    }

    pub(super) fn poll(&mut self) -> Vec<GitInspectResult> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut results = Vec::new();
        while let Ok(result) = worker.result_rx.try_recv() {
            self.in_flight.remove(&result.session_id);
            results.push(result);
        }
        results
    }
}

fn spawn_inspector_worker() -> Option<GitInspectorWorker> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<(Uuid, PathBuf)>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("git-inspector".to_owned())
        .spawn(move || {
            while let Ok((session_id, cwd)) = request_rx.recv() {
                let observation = inspect_git_state(&cwd);
                let result = GitInspectResult {
                    session_id,
                    observation,
                };
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(GitInspectorWorker {
        request_tx,
        result_rx,
    })
}

#[derive(Debug)]
pub(super) struct WorktreeCreateJob {
    pub(super) session_id: Uuid,
    pub(super) repo_root: PathBuf,
    pub(super) worktree_path: PathBuf,
    pub(super) branch: String,
}

#[derive(Debug)]
pub(super) struct WorktreeCreateResult {
    pub(super) session_id: Uuid,
    pub(super) error: Option<String>,
}

/// Crea worktrees en un hilo de trabajo: `git worktree add` puede tardar
/// segundos en repos grandes y no puede bloquear el hilo de UI. El manager
/// encola el lanzamiento y spawnea el panel cuando llega el resultado.
#[derive(Debug, Default)]
pub(super) struct WorktreeCreator {
    worker: Option<WorktreeCreatorWorker>,
}

#[derive(Debug)]
struct WorktreeCreatorWorker {
    request_tx: Sender<WorktreeCreateJob>,
    result_rx: Receiver<WorktreeCreateResult>,
}

impl WorktreeCreator {
    /// Devuelve el trabajo si no se pudo encolar, para que el caller pueda
    /// caer a la creación síncrona en vez de dejar el lanzamiento colgado.
    pub(super) fn request(&mut self, job: WorktreeCreateJob) -> Result<(), WorktreeCreateJob> {
        if self.worker.is_none() {
            self.worker = spawn_worktree_worker();
        }
        let Some(worker) = self.worker.as_ref() else {
            return Err(job);
        };
        match worker.request_tx.send(job) {
            Ok(()) => Ok(()),
            Err(returned) => {
                self.worker = None;
                Err(returned.0)
            }
        }
    }

    pub(super) fn poll(&mut self) -> Vec<WorktreeCreateResult> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut results = Vec::new();
        while let Ok(result) = worker.result_rx.try_recv() {
            results.push(result);
        }
        results
    }
}

fn spawn_worktree_worker() -> Option<WorktreeCreatorWorker> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<WorktreeCreateJob>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("git-worktree".to_owned())
        .spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let error = create_git_worktree(&job.repo_root, &job.worktree_path, &job.branch)
                    .err()
                    .map(|err| err.to_string());
                let result = WorktreeCreateResult {
                    session_id: job.session_id,
                    error,
                };
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(WorktreeCreatorWorker {
        request_tx,
        result_rx,
    })
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::{GitInspector, WorktreeCreateJob, WorktreeCreator};

    #[test]
    fn worktree_creator_reports_failure_outside_a_repo() {
        let mut creator = WorktreeCreator::default();
        let session_id = Uuid::new_v4();
        let base = std::env::temp_dir().join(format!("worktree-create-test-{session_id}"));
        std::fs::create_dir_all(&base).expect("create temp dir");

        creator
            .request(WorktreeCreateJob {
                session_id,
                repo_root: base.clone(),
                worktree_path: base.join("wt"),
                branch: "test-branch".to_owned(),
            })
            .expect("enqueue worktree job");

        let mut results = Vec::new();
        for _ in 0..250 {
            results = creator.poll();
            if !results.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, session_id);
        assert!(results[0].error.is_some());
    }

    #[test]
    fn git_inspector_delivers_result_and_clears_in_flight() {
        let mut inspector = GitInspector::default();
        let session_id = Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("git-inspect-test-{session_id}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        inspector.request(session_id, dir.clone());

        let mut results = Vec::new();
        for _ in 0..250 {
            results = inspector.poll();
            if !results.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, session_id);
        assert!(results[0].observation.is_none());
        assert!(inspector.in_flight.is_empty());
    }
}
