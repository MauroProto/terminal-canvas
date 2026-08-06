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

/// Tope para `git worktree add`: un stall del filesystem (OneDrive/NFS) no
/// puede dejar el lanzamiento colgado para siempre. Vencido el plazo se mata
/// el child. Why: Orca (`WORKTREE_ADD_TIMEOUT_MS`) usa 180 s con la misma idea.
const WORKTREE_ADD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

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
    ensure_push_auto_setup_remote(repo_root);
    // `--no-track`: sin esto la rama nueva hereda el upstream de la base y
    // `git status` miente "behind by N" antes del primer push. El upstream
    // correcto lo crea `push.autoSetupRemote` recién al primer push.
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "--no-track", "-b", branch])
        .arg(worktree_path);
    let output = run_with_timeout(&mut command, WORKTREE_ADD_TIMEOUT)?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Configura `push.autoSetupRemote=true` solo si el usuario no lo configuró en
/// ningún scope (exit 1 de `git config --get`). Combinado con `--no-track`
/// evita heredar upstreams ajenos y hace que el primer push cree el upstream.
fn ensure_push_auto_setup_remote(repo_root: &Path) {
    let already = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "push.autoSetupRemote"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if already {
        return;
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "push.autoSetupRemote", "true"])
        .output();
}

/// Corre un comando con timeout, drenando stdout/stderr en hilos aparte para
/// que los pipes nunca bloqueen al hijo. Vencido el plazo mata al child y
/// devuelve un error `TimedOut`; un proceso colgado jamás deja la llamada
/// colgando.
fn run_with_timeout(
    command: &mut Command,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::process::{Output, Stdio};

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drenar en hilos: si el hijo escribe más que el buffer del pipe y nadie
    // lee, se bloquea en write() y el timeout nunca lo vería terminado.
    let stdout_handle = child.stdout.take().map(drain_pipe);
    let stderr_handle = child.stderr.take().map(drain_pipe);

    let started = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = stdout_handle.map(join_drain).unwrap_or_default();
                let stderr = stderr_handle.map(join_drain).unwrap_or_default();
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("git worktree add timed out after {timeout:?}");
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
}

fn drain_pipe<R: std::io::Read + Send + 'static>(
    mut reader: R,
) -> std::thread::JoinHandle<Vec<u8>> {
    // El unwrap es de spawn: si no se puede crear el hilo de drenaje el
    // fallback es leer nada, pero worktree add sigue funcionando.
    std::thread::Builder::new()
        .name("git-pipe-drain".to_owned())
        .spawn(move || {
            let mut buffer = Vec::new();
            let _ = reader.read_to_end(&mut buffer);
            buffer
        })
        .expect("spawn pipe drain thread")
}

fn join_drain(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
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
    use std::process::Command;
    use std::time::{Duration, Instant};

    use uuid::Uuid;

    use super::{
        create_git_worktree, run_with_timeout, GitInspector, WorktreeCreateJob, WorktreeCreator,
    };

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
    fn a_stalled_command_is_killed_by_the_timeout() {
        // Regresión: un `git worktree add` colgado en un filesystem lento
        // (OneDrive/NFS) no puede dejar el lanzamiento esperando para siempre.
        let mut command = Command::new("sleep");
        command.arg("30");

        let started = Instant::now();
        let result = run_with_timeout(&mut command, Duration::from_millis(200));
        let elapsed = started.elapsed();

        assert!(result.is_err(), "el timeout debe abortar el comando");
        assert!(
            elapsed < Duration::from_secs(5),
            "tardó {elapsed:?}: el kill no funcionó"
        );
    }

    #[test]
    fn a_fast_command_returns_its_captured_output() {
        let mut command = Command::new("echo");
        command.arg("hola");
        let output = run_with_timeout(&mut command, Duration::from_secs(5)).expect("run echo");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hola");
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git disponible");
        assert!(
            output.status.success(),
            "git {args:?} falló: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn worktree_branches_are_created_without_upstream_tracking() {
        // End-to-end con git real: la rama base tiene upstream (como pasa con
        // un clone); la rama del worktree NO debe heredarlo (`--no-track`), y
        // push.autoSetupRemote queda habilitado para el primer push.
        let id = Uuid::new_v4();
        let base = std::env::temp_dir().join(format!("worktree-notrack-{id}"));
        let origin = base.join("origin");
        let clone = base.join("clone");
        std::fs::create_dir_all(&origin).unwrap();

        git(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("file.txt"), "base").unwrap();
        git(&origin, &["add", "."]);
        git(
            &origin,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        );
        git(
            &base,
            &[
                "clone",
                "-q",
                &origin.display().to_string(),
                &clone.display().to_string(),
            ],
        );
        git(&clone, &["push", "-q", "-u", "origin", "main"]);
        assert!(!git(&clone, &["config", "branch.main.merge"]).is_empty());

        let worktree_path = clone.join("..").join(format!("wt-{id}"));
        create_git_worktree(&clone, &worktree_path, "feature").expect("crear worktree");

        // Sin upstream: ni merge ni remote para la rama nueva.
        let probe = Command::new("git")
            .arg("-C")
            .arg(&worktree_path)
            .args(["config", "branch.feature.merge"])
            .output()
            .unwrap();
        assert!(!probe.status.success(), "la rama heredó upstream");
        assert_eq!(git(&clone, &["config", "push.autoSetupRemote"]), "true");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&worktree_path);
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
