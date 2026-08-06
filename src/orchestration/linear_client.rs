//! Cliente de Linear vía su API GraphQL (P3.17, T1).
//!
//! Mismo patrón que [`super::gh_client`]: worker propio, timeout duro, cache y
//! un estado `Unavailable(motivo)` explícito cuando falta el token o la API
//! rechaza. El token sale de `config.toml [integrations] linear_token`.

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use serde::Deserialize;

pub const LINEAR_API: &str = "https://api.linear.app/graphql";
pub const LINEAR_TIMEOUT: Duration = Duration::from_secs(20);
pub const LINEAR_CACHE_TTL: Duration = Duration::from_secs(60);

/// Query de issues abiertos asignados al usuario del token.
pub const ISSUES_QUERY: &str = r#"{"query":"{ viewer { assignedIssues(filter: {state: {type: {neq: \"completed\"}}}, first: 50) { nodes { identifier title description state { name } } } } }"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearAvailability {
    Ready,
    /// Sin token configurado, token inválido, o la API no respondió.
    Unavailable(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinearIssue {
    /// Identificador legible (`ENG-123`), que es lo que el humano reconoce.
    pub identifier: String,
    pub title: String,
    pub description: String,
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinearSnapshot {
    pub issues: Vec<LinearIssue>,
}

#[derive(Debug, Clone)]
pub struct LinearResult {
    pub availability: LinearAvailability,
    pub snapshot: LinearSnapshot,
}

// --- Parseo de la respuesta GraphQL (puro y testeable) ---

#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: Option<ResponseData>,
    #[serde(default)]
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct ResponseData {
    #[serde(default)]
    viewer: Option<Viewer>,
}

#[derive(Debug, Deserialize)]
struct Viewer {
    #[serde(default, rename = "assignedIssues")]
    assigned_issues: Option<IssueConnection>,
}

#[derive(Debug, Deserialize)]
struct IssueConnection {
    #[serde(default)]
    nodes: Vec<IssueNode>,
}

#[derive(Debug, Deserialize)]
struct IssueNode {
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    state: Option<IssueState>,
}

#[derive(Debug, Deserialize)]
struct IssueState {
    #[serde(default)]
    name: String,
}

/// Parsea la respuesta de Linear. Los `errors` de GraphQL ganan sobre `data`:
/// una respuesta 200 con errores no es una lista vacía, es un fallo.
pub fn parse_issues_response(body: &str) -> Result<LinearSnapshot, String> {
    let parsed: GraphQlResponse =
        serde_json::from_str(body).map_err(|err| format!("respuesta ilegible: {err}"))?;
    if let Some(errors) = parsed.errors.filter(|errors| !errors.is_empty()) {
        let message = errors
            .into_iter()
            .map(|error| error.message)
            .find(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "Linear rechazó la consulta".to_owned());
        return Err(message);
    }
    let nodes = parsed
        .data
        .and_then(|data| data.viewer)
        .and_then(|viewer| viewer.assigned_issues)
        .map(|connection| connection.nodes)
        .unwrap_or_default();
    Ok(LinearSnapshot {
        issues: nodes
            .into_iter()
            .map(|node| LinearIssue {
                identifier: node.identifier,
                title: node.title,
                description: node.description.unwrap_or_default(),
                state: node.state.map(|state| state.name).unwrap_or_default(),
            })
            .collect(),
    })
}

/// Nombre de branch/worktree para un issue de Linear: `ENG-123` ya es un
/// identificador seguro, pero igual se normaliza (nunca separadores de path).
pub fn linear_branch_name(identifier: &str, title: &str) -> String {
    let prefix: String = identifier
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect::<String>()
        .to_lowercase();
    let prefix = if prefix.is_empty() {
        "linear".to_owned()
    } else {
        prefix
    };
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        prefix
    } else {
        format!("{prefix}-{slug}")
    }
}

/// Prompt inicial para el agente que arranca a trabajar un issue de Linear.
pub fn linear_prompt(identifier: &str, title: &str, description: &str) -> String {
    let description = description.trim();
    if description.is_empty() {
        format!("Trabajá en el issue {identifier}: {title}")
    } else {
        format!("Trabajá en el issue {identifier}: {title}\n{description}")
    }
}

/// Consulta bloqueante (corre en el worker).
pub fn fetch_snapshot(token: Option<String>) -> LinearResult {
    let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
        return LinearResult {
            availability: LinearAvailability::Unavailable(
                "sin linear_token en config.toml [integrations]".to_owned(),
            ),
            snapshot: LinearSnapshot::default(),
        };
    };
    let response = ureq_post(&token);
    match response {
        Ok(body) => match parse_issues_response(&body) {
            Ok(snapshot) => LinearResult {
                availability: LinearAvailability::Ready,
                snapshot,
            },
            Err(reason) => LinearResult {
                availability: LinearAvailability::Unavailable(reason),
                snapshot: LinearSnapshot::default(),
            },
        },
        Err(reason) => LinearResult {
            availability: LinearAvailability::Unavailable(reason),
            snapshot: LinearSnapshot::default(),
        },
    }
}

/// POST a la API con timeout. Se usa `curl` por la misma razón que `gh` en
/// P2.13: evita sumar un stack HTTP bloqueante al binario para dos requests.
fn ureq_post(token: &str) -> Result<String, String> {
    let output = std::process::Command::new("curl")
        .args(["-sS", "-m"])
        .arg(LINEAR_TIMEOUT.as_secs().to_string())
        .args([
            "--connect-timeout",
            "5",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-H",
        ])
        .arg(format!("Authorization: {token}"))
        .args(["-d", ISSUES_QUERY, LINEAR_API])
        .output()
        .map_err(|err| format!("no se pudo invocar curl: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("la API de Linear no respondió");
        return Err(first.to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Worker con cache, gemelo del de `gh_client`.
#[derive(Debug, Default)]
pub struct LinearClient {
    worker: Option<LinearWorker>,
    last_fetch_at: Option<Instant>,
    in_flight: bool,
}

#[derive(Debug)]
struct LinearWorker {
    request_tx: Sender<Option<String>>,
    result_rx: Receiver<LinearResult>,
}

impl LinearClient {
    pub fn request(&mut self, token: Option<String>, force: bool) {
        if self.in_flight {
            return;
        }
        if !force && !self.cache_expired(Instant::now()) {
            return;
        }
        if self.worker.is_none() {
            self.worker = spawn_linear_worker();
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        if worker.request_tx.send(token).is_err() {
            self.worker = None;
            return;
        }
        self.in_flight = true;
    }

    fn cache_expired(&self, now: Instant) -> bool {
        match self.last_fetch_at {
            Some(at) => now.duration_since(at) >= LINEAR_CACHE_TTL,
            None => true,
        }
    }

    pub fn poll(&mut self) -> Vec<LinearResult> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(result) = worker.result_rx.try_recv() {
            out.push(result);
        }
        if !out.is_empty() {
            self.in_flight = false;
            self.last_fetch_at = Some(Instant::now());
        }
        out
    }
}

fn spawn_linear_worker() -> Option<LinearWorker> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<Option<String>>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<LinearResult>();
    std::thread::Builder::new()
        .name("linear-client".to_owned())
        .spawn(move || {
            while let Ok(token) = request_rx.recv() {
                if result_tx.send(fetch_snapshot(token)).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some(LinearWorker {
        request_tx,
        result_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        fetch_snapshot, linear_branch_name, linear_prompt, parse_issues_response,
        LinearAvailability, LinearClient, LINEAR_CACHE_TTL,
    };
    use std::time::Instant;

    const OK_FIXTURE: &str = r#"{
        "data": { "viewer": { "assignedIssues": { "nodes": [
            {"identifier":"ENG-123","title":"Arreglar el login","description":"Falla con SSO","state":{"name":"In Progress"}},
            {"identifier":"ENG-124","title":"Sin descripción","description":null,"state":{"name":"Todo"}}
        ] } } }
    }"#;

    #[test]
    fn parses_the_issue_list_fixture() {
        let snapshot = parse_issues_response(OK_FIXTURE).expect("parsea");
        assert_eq!(snapshot.issues.len(), 2);
        assert_eq!(snapshot.issues[0].identifier, "ENG-123");
        assert_eq!(snapshot.issues[0].state, "In Progress");
        assert!(snapshot.issues[0].description.contains("SSO"));
        // description null no rompe: queda vacío.
        assert_eq!(snapshot.issues[1].description, "");
    }

    #[test]
    fn graphql_errors_win_over_data() {
        let body = r#"{"data":{"viewer":null},"errors":[{"message":"Authentication required"}]}"#;
        let error = parse_issues_response(body).expect_err("tiene que fallar");
        assert_eq!(error, "Authentication required");
    }

    #[test]
    fn an_empty_error_list_is_not_a_failure() {
        let body = r#"{"data":{"viewer":{"assignedIssues":{"nodes":[]}}},"errors":[]}"#;
        let snapshot = parse_issues_response(body).expect("parsea");
        assert!(snapshot.issues.is_empty());
    }

    #[test]
    fn an_unreadable_body_reports_the_reason() {
        let error = parse_issues_response("no soy json").expect_err("falla");
        assert!(error.contains("ilegible"), "got {error}");
    }

    #[test]
    fn a_missing_viewer_yields_an_empty_list() {
        let snapshot = parse_issues_response(r#"{"data":{}}"#).expect("parsea");
        assert!(snapshot.issues.is_empty());
    }

    #[test]
    fn without_a_token_the_integration_reports_why() {
        let result = fetch_snapshot(None);
        match result.availability {
            LinearAvailability::Unavailable(reason) => {
                assert!(reason.contains("linear_token"), "got {reason}");
            }
            other => panic!("esperaba Unavailable, got {other:?}"),
        }
        // Un token en blanco cuenta como ausente.
        assert!(matches!(
            fetch_snapshot(Some("   ".to_owned())).availability,
            LinearAvailability::Unavailable(_)
        ));
    }

    #[test]
    fn branch_names_are_slugified_and_safe() {
        assert_eq!(
            linear_branch_name("ENG-123", "Arreglar el login"),
            "eng-123-arreglar-el-login"
        );
        let name = linear_branch_name("ENG-1", "../../etc/passwd");
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains(".."), "got {name}");
        assert_eq!(linear_branch_name("ENG-9", "   "), "eng-9");
    }

    #[test]
    fn the_prompt_includes_identifier_title_and_description() {
        let prompt = linear_prompt("ENG-123", "Arreglar el login", "Falla con SSO");
        assert!(prompt.starts_with("Trabajá en el issue ENG-123: Arreglar el login"));
        assert!(prompt.contains("Falla con SSO"));
        assert_eq!(
            linear_prompt("ENG-1", "T", "  ").lines().count(),
            1,
            "sin descripción es una sola línea"
        );
    }

    #[test]
    fn the_cache_blocks_a_second_request_within_the_ttl() {
        let mut client = LinearClient::default();
        assert!(client.cache_expired(Instant::now()));
        client.last_fetch_at = Some(Instant::now());
        assert!(!client.cache_expired(Instant::now()));
        assert!(client.cache_expired(Instant::now() + LINEAR_CACHE_TTL));
    }
}
