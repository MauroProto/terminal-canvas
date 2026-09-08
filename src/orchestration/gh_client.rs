//! Cliente de GitHub vía la CLI `gh` (P2.13, T1).
//!
//! Corre `gh pr list` / `gh issue list` en un worker propio (mismo patrón que
//! `DiffLoader`) para no bloquear el frame, con timeout duro de 20 s y cache de
//! 60 s. Si `gh` no está instalado o no hay sesión iniciada, el estado queda en
//! [`GhAvailability::Unavailable`] con el motivo, en vez de fallar en silencio.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Timeout duro de cada invocación de `gh`.
pub const GH_TIMEOUT: Duration = Duration::from_secs(20);
/// Cuánto vale un resultado antes de volver a preguntar.
pub const GH_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhAvailability {
    Ready,
    /// `gh` no está, no hay auth, o el repo no tiene remoto de GitHub.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Issue {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhSnapshot {
    pub pull_requests: Vec<PullRequest>,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone)]
pub struct GhResult {
    pub repo_root: PathBuf,
    pub availability: GhAvailability,
    pub snapshot: GhSnapshot,
}

/// Parsea la salida JSON de `gh pr list --json ...`. Una salida vacía o
/// inválida devuelve lista vacía en vez de romper la UI.
pub fn parse_pull_requests(json: &str) -> Vec<PullRequest> {
    serde_json::from_str(json.trim()).unwrap_or_default()
}

/// Parsea la salida JSON de `gh issue list --json ...`.
pub fn parse_issues(json: &str) -> Vec<Issue> {
    serde_json::from_str(json.trim()).unwrap_or_default()
}

/// Motivo legible cuando `gh auth status` falla. La CLI escribe el detalle en
/// stderr; nos quedamos con la primera línea útil.
pub fn auth_failure_reason(stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh no está autenticado");
    // Las líneas de gh vienen con glifos de estado; los sacamos.
    first
        .trim_start_matches(['✓', '✗', 'X', '-', '•'])
        .trim()
        .to_owned()
}

/// Slug seguro para el nombre de branch/worktree de un issue.
/// `issue-<n>-<slug>`: minúsculas, solo alfanumérico y guiones, tope 40 chars.
pub fn issue_branch_name(number: u64, title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("issue-{number}")
    } else {
        format!("issue-{number}-{slug}")
    }
}

/// Prompt inicial para el agente que arranca a trabajar un issue.
pub fn issue_prompt(number: u64, title: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("Trabajá en el issue #{number}: {title}")
    } else {
        format!("Trabajá en el issue #{number}: {title}\n{body}")
    }
}

/// Corre un comando con timeout duro; `None` si no arrancó o si se pasó del
/// tiempo (en ese caso se mata el hijo para no dejar zombies).
fn run_with_timeout(mut command: Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

fn gh_command(repo_root: &PathBuf, args: &[&str]) -> Command {
    let mut command = Command::new("gh");
    command.current_dir(repo_root).args(args);
    command
}

/// Consulta PRs e issues del repo. Bloqueante: corre en el worker.
pub fn fetch_snapshot(repo_root: &PathBuf) -> GhResult {
    // 1) ¿está `gh` y hay sesión?
    let Some(auth) = run_with_timeout(gh_command(repo_root, &["auth", "status"]), GH_TIMEOUT)
    else {
        return GhResult {
            repo_root: repo_root.clone(),
            availability: GhAvailability::Unavailable("gh no está instalado o no responde".into()),
            snapshot: GhSnapshot::default(),
        };
    };
    if !auth.status.success() {
        let reason = auth_failure_reason(&String::from_utf8_lossy(&auth.stderr));
        return GhResult {
            repo_root: repo_root.clone(),
            availability: GhAvailability::Unavailable(reason),
            snapshot: GhSnapshot::default(),
        };
    }

    let prs = run_with_timeout(
        gh_command(
            repo_root,
            &[
                "pr",
                "list",
                "--json",
                "number,title,headRefName,state,updatedAt",
            ],
        ),
        GH_TIMEOUT,
    );
    let issues = run_with_timeout(
        gh_command(
            repo_root,
            &[
                "issue",
                "list",
                "--json",
                "number,title,state,body,updatedAt",
            ],
        ),
        GH_TIMEOUT,
    );

    // Si el repo no tiene remoto de GitHub, `gh pr list` falla: lo reportamos
    // como no disponible en vez de mostrar listas vacías mintiendo.
    if let Some(output) = prs.as_ref() {
        if !output.status.success() {
            let reason = auth_failure_reason(&String::from_utf8_lossy(&output.stderr));
            return GhResult {
                repo_root: repo_root.clone(),
                availability: GhAvailability::Unavailable(reason),
                snapshot: GhSnapshot::default(),
            };
        }
    }

    GhResult {
        repo_root: repo_root.clone(),
        availability: GhAvailability::Ready,
        snapshot: GhSnapshot {
            pull_requests: prs
                .as_ref()
                .map(|output| parse_pull_requests(&String::from_utf8_lossy(&output.stdout)))
                .unwrap_or_default(),
            issues: issues
                .as_ref()
                .map(|output| parse_issues(&String::from_utf8_lossy(&output.stdout)))
                .unwrap_or_default(),
        },
    }
}

/// Worker con cache: pide a `gh` fuera del frame y cachea 60 s por repo.
#[derive(Debug, Default)]
pub struct GhClient {
    worker: Option<GhWorker>,
    last_fetch_at: Option<Instant>,
    last_repo_root: Option<PathBuf>,
    in_flight_repo: Option<PathBuf>,
}

#[derive(Debug)]
struct GhWorker {
    request_tx: Sender<PathBuf>,
    result_rx: Receiver<GhResult>,
}

impl GhClient {
    /// Pide un refresh. Respeta el cache salvo que `force` sea true.
    pub fn request(&mut self, repo_root: PathBuf, force: bool) {
        if self.in_flight_repo.is_some() {
            return;
        }
        if !force && !self.cache_expired(&repo_root, Instant::now()) {
            return;
        }
        if self.worker.is_none() {
            self.worker = spawn_gh_worker();
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if worker.request_tx.send(repo_root.clone()).is_err() {
            self.worker = None;
            return;
        }
        self.in_flight_repo = Some(repo_root);
    }

    fn cache_expired(&self, repo_root: &Path, now: Instant) -> bool {
        match (self.last_repo_root.as_deref(), self.last_fetch_at) {
            (Some(cached_root), Some(at)) if cached_root == repo_root => {
                now.duration_since(at) >= GH_CACHE_TTL
            }
            _ => true,
        }
    }

    /// Indica carga sólo para el repo visible. Una consulta vieja de otro
    /// workspace no debe dejar un spinner engañoso en el actual.
    pub fn is_loading_for(&self, repo_root: &Path) -> bool {
        self.in_flight_repo.as_deref() == Some(repo_root)
    }

    pub fn poll(&mut self) -> Vec<GhResult> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(result) = worker.result_rx.try_recv() {
            out.push(result);
        }
        if let Some(last) = out.last() {
            self.last_repo_root = Some(last.repo_root.clone());
            self.last_fetch_at = Some(Instant::now());
            self.in_flight_repo = None;
        }
        out
    }
}

fn spawn_gh_worker() -> Option<GhWorker> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<PathBuf>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<GhResult>();
    std::thread::Builder::new()
        .name("gh-client".to_owned())
        .spawn(move || {
            while let Ok(repo_root) = request_rx.recv() {
                if result_tx.send(fetch_snapshot(&repo_root)).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(GhWorker {
        request_tx,
        result_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        auth_failure_reason, issue_branch_name, issue_prompt, parse_issues, parse_pull_requests,
        GhClient, GH_CACHE_TTL,
    };
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    const PR_FIXTURE: &str = r#"[
        {"number":42,"title":"Fix the thing","headRefName":"fix/thing","state":"OPEN","updatedAt":"2026-08-05T12:00:00Z"},
        {"number":41,"title":"Old one","headRefName":"chore/old","state":"MERGED","updatedAt":"2026-08-01T09:30:00Z"}
    ]"#;

    const ISSUE_FIXTURE: &str = r#"[
        {"number":7,"title":"Botón roto","state":"OPEN","body":"Al clickear no pasa nada","updatedAt":"2026-08-04T10:00:00Z"}
    ]"#;

    #[test]
    fn parses_the_pr_list_fixture() {
        let prs = parse_pull_requests(PR_FIXTURE);
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].head_ref_name, "fix/thing");
        assert_eq!(prs[0].state, "OPEN");
        assert_eq!(prs[1].state, "MERGED");
    }

    #[test]
    fn parses_the_issue_list_fixture() {
        let issues = parse_issues(ISSUE_FIXTURE);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 7);
        assert_eq!(issues[0].title, "Botón roto");
        assert!(issues[0].body.contains("no pasa nada"));
    }

    #[test]
    fn a_broken_payload_yields_an_empty_list_instead_of_panicking() {
        assert!(parse_pull_requests("").is_empty());
        assert!(parse_pull_requests("no json").is_empty());
        assert!(parse_issues("{}").is_empty());
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let prs = parse_pull_requests(r#"[{"number":1}]"#);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].title, "");
        assert_eq!(prs[0].head_ref_name, "");
    }

    #[test]
    fn auth_failure_reason_takes_the_first_useful_line() {
        let stderr = "\n  ✗ You are not logged into any GitHub hosts\n  Run gh auth login\n";
        assert_eq!(
            auth_failure_reason(stderr),
            "You are not logged into any GitHub hosts"
        );
    }

    #[test]
    fn auth_failure_reason_has_a_fallback() {
        assert_eq!(auth_failure_reason("   \n\n"), "gh no está autenticado");
    }

    #[test]
    fn issue_branch_names_are_slugified_and_capped() {
        assert_eq!(issue_branch_name(7, "Botón roto"), "issue-7-bot-n-roto");
        assert_eq!(issue_branch_name(9, "  "), "issue-9");
        let long = issue_branch_name(1, &"palabra ".repeat(20));
        assert!(long.len() <= 8 + 40, "got {} chars: {long}", long.len());
        assert!(!long.ends_with('-'), "got {long}");
    }

    #[test]
    fn issue_branch_names_never_contain_path_separators() {
        let name = issue_branch_name(3, "../../etc/passwd");
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains(".."), "got {name}");
    }

    #[test]
    fn issue_prompt_includes_number_title_and_body() {
        let prompt = issue_prompt(7, "Botón roto", "Al clickear no pasa nada");
        assert!(prompt.starts_with("Trabajá en el issue #7: Botón roto"));
        assert!(prompt.contains("Al clickear no pasa nada"));
    }

    #[test]
    fn issue_prompt_without_body_is_a_single_line() {
        let prompt = issue_prompt(7, "Botón roto", "   ");
        assert_eq!(prompt.lines().count(), 1, "got {prompt:?}");
    }

    #[test]
    fn the_cache_blocks_a_second_request_within_the_ttl() {
        let mut client = GhClient::default();
        let repo = PathBuf::from("/tmp/example-repo");
        // Sin fetch previo, el cache está vencido.
        assert!(client.cache_expired(&repo, Instant::now()));
        client.last_repo_root = Some(repo.clone());
        client.last_fetch_at = Some(Instant::now());
        assert!(
            !client.cache_expired(&repo, Instant::now()),
            "recién consultado"
        );
        assert!(
            client.cache_expired(&repo, Instant::now() + GH_CACHE_TTL),
            "pasado el TTL vuelve a consultar"
        );
        assert!(
            client.cache_expired(Path::new("/tmp/otro-repo"), Instant::now()),
            "el cache de un repo no debe bloquear otro workspace"
        );
    }
}
