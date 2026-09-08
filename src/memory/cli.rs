//! CLI `tc-memory`: misma semántica que la app, contra el mismo archivo SQLite.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use super::context::{build_context_pack, format_context_pack};
use super::model::{Actor, HandoffRequest, MemoryKind, RememberRequest, ScopeKind};
use super::store::MemoryStore;

pub fn run(args: &[String]) -> Result<String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return Ok(usage());
    }
    let parsed = parse_args(args)?;
    let store = MemoryStore::open(&parsed.db)?;
    match parsed.command.as_str() {
        "health" => Ok(serde_json::json!({
            "ok": true,
            "db": store.path().display().to_string(),
        })
        .to_string()
            + "\n"),
        "remember" => {
            let cwd = required_cwd(&parsed)?;
            let key = required(&parsed, "key")?;
            let content = required(&parsed, "content")?;
            let scope = parsed.scope.unwrap_or(ScopeKind::Project);
            let request = RememberRequest {
                client_id: "tc-memory-cli".to_owned(),
                request_id: parsed
                    .request_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                cwd,
                scope,
                kind: parsed.kind.unwrap_or(MemoryKind::Decision),
                key,
                content,
                actor: Actor::human(),
                expected_revision: parsed.expected_revision,
                orchestrator_task_id: parsed.task_id,
                workspace_id: None,
            };
            Ok(serde_json::to_string_pretty(&store.remember(request)?)? + "\n")
        }
        "propose" => {
            let cwd = required_cwd(&parsed)?;
            let request = RememberRequest {
                client_id: "tc-memory-cli".to_owned(),
                request_id: Uuid::new_v4().to_string(),
                cwd,
                scope: parsed.scope.unwrap_or(ScopeKind::Project),
                kind: parsed.kind.unwrap_or(MemoryKind::Fact),
                key: required(&parsed, "key")?,
                content: required(&parsed, "content")?,
                actor: Actor::agent("cli"),
                expected_revision: parsed.expected_revision,
                orchestrator_task_id: parsed.task_id,
                workspace_id: None,
            };
            Ok(serde_json::to_string_pretty(&store.propose(request)?)? + "\n")
        }
        "approve" => {
            let id = required(&parsed, "id")?;
            Ok(serde_json::to_string_pretty(&store.approve(&id, &Actor::human())?)? + "\n")
        }
        "reject" => {
            let id = required(&parsed, "id")?;
            Ok(serde_json::to_string_pretty(&store.reject(&id, &Actor::human())?)? + "\n")
        }
        "forget" => {
            let id = required(&parsed, "id")?;
            Ok(serde_json::to_string_pretty(&store.forget(&id, &Actor::human())?)? + "\n")
        }
        "context" => {
            let cwd = required_cwd(&parsed)?;
            let pack = build_context_pack(&store, &cwd, parsed.query.as_deref(), parsed.task_id)?;
            if parsed.text_output {
                Ok(format_context_pack(&pack))
            } else {
                Ok(serde_json::to_string_pretty(&pack)? + "\n")
            }
        }
        "search" => {
            let cwd = required_cwd(&parsed)?;
            let query = parsed
                .query
                .clone()
                .ok_or_else(|| anyhow!("search requiere una consulta"))?;
            Ok(serde_json::to_string_pretty(&store.search(&cwd, &query)?)? + "\n")
        }
        "pending" => {
            let cwd = required_cwd(&parsed)?;
            Ok(serde_json::to_string_pretty(
                &store.list_visible(&cwd, crate::memory::MemoryStatus::Candidate)?,
            )? + "\n")
        }
        "handoff" => {
            let cwd = required_cwd(&parsed)?;
            match parsed.subcommand.as_deref() {
                Some("get") => {
                    return Ok(serde_json::to_string_pretty(
                        &store.latest_handoff(&cwd, parsed.task_id)?,
                    )? + "\n");
                }
                Some("create") | None => {}
                Some(other) => anyhow::bail!("subcomando handoff desconocido: {other}"),
            }
            let record = store.create_handoff(HandoffRequest {
                cwd,
                summary: required(&parsed, "summary")?,
                provider: parsed.provider.clone(),
                session_id: None,
                orchestrator_task_id: parsed.task_id,
                ttl_secs: Some(72 * 3600),
            })?;
            Ok(serde_json::to_string_pretty(&record)? + "\n")
        }
        "export" => {
            let cwd = required_cwd(&parsed)?;
            store.export_markdown(&cwd)
        }
        "link" => {
            let a = parsed
                .cwd
                .clone()
                .ok_or_else(|| anyhow!("link requiere --cwd"))?;
            let b = parsed
                .other_cwd
                .clone()
                .ok_or_else(|| anyhow!("link requiere --other-cwd"))?;
            Ok(serde_json::to_string_pretty(&store.link_projects(&a, &b)?)? + "\n")
        }
        other => Err(anyhow!("comando desconocido: {other}")),
    }
}

struct Parsed {
    db: PathBuf,
    command: String,
    subcommand: Option<String>,
    cwd: Option<PathBuf>,
    other_cwd: Option<PathBuf>,
    key: Option<String>,
    content: Option<String>,
    id: Option<String>,
    summary: Option<String>,
    query: Option<String>,
    scope: Option<ScopeKind>,
    kind: Option<MemoryKind>,
    expected_revision: Option<u32>,
    task_id: Option<Uuid>,
    request_id: Option<String>,
    provider: Option<String>,
    text_output: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed> {
    let mut db = super::store::default_db_path().unwrap_or_else(|| PathBuf::from("memory.db"));
    let mut subcommand = None;
    let mut cwd = None;
    let mut other_cwd = None;
    let mut key = None;
    let mut content = None;
    let mut id = None;
    let mut summary = None;
    let mut query = None;
    let mut scope = None;
    let mut kind = None;
    let mut expected_revision = None;
    let mut task_id = None;
    let mut request_id = None;
    let mut provider = None;
    let mut text_output = false;
    let mut rest: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--db" => {
                db = PathBuf::from(next(args, &mut i, "--db")?);
            }
            "--cwd" => cwd = Some(PathBuf::from(next(args, &mut i, "--cwd")?)),
            "--other-cwd" => other_cwd = Some(PathBuf::from(next(args, &mut i, "--other-cwd")?)),
            "--key" => key = Some(next(args, &mut i, "--key")?),
            "--content" => content = Some(next(args, &mut i, "--content")?),
            "--id" => id = Some(next(args, &mut i, "--id")?),
            "--summary" => summary = Some(next(args, &mut i, "--summary")?),
            "--scope" => {
                let raw = next(args, &mut i, "--scope")?;
                scope = Some(ScopeKind::parse(&raw).ok_or_else(|| anyhow!("scope inválido"))?);
            }
            "--kind" => {
                let raw = next(args, &mut i, "--kind")?;
                kind = Some(MemoryKind::parse(&raw).ok_or_else(|| anyhow!("kind inválido"))?);
            }
            "--expected-revision" => {
                expected_revision = Some(
                    next(args, &mut i, "--expected-revision")?
                        .parse()
                        .map_err(|_| anyhow!("expected-revision inválido"))?,
                );
            }
            "--task" => {
                task_id = Some(
                    Uuid::parse_str(&next(args, &mut i, "--task")?)
                        .map_err(|_| anyhow!("task uuid inválido"))?,
                );
            }
            "--request-id" => request_id = Some(next(args, &mut i, "--request-id")?),
            "--provider" => provider = Some(next(args, &mut i, "--provider")?),
            "--text" => text_output = true,
            other if other.starts_with('-') => {
                return Err(anyhow!("flag desconocida: {other}"));
            }
            other => rest.push(other.to_owned()),
        }
        i += 1;
    }

    if rest.is_empty() {
        return Err(anyhow!(usage()));
    }
    let command = rest[0].clone();
    if rest.len() > 1 {
        if rest[0] == "handoff" {
            subcommand = Some(rest[1].clone());
            if rest.len() > 2 && query.is_none() && rest[1] != "create" {
                query = Some(rest[2..].join(" "));
            }
        } else if rest[0] == "search" || rest[0] == "context" {
            query = Some(rest[1..].join(" "));
        }
    }
    Ok(Parsed {
        db,
        command,
        subcommand,
        cwd,
        other_cwd,
        key,
        content,
        id,
        summary,
        query,
        scope,
        kind,
        expected_revision,
        task_id,
        request_id,
        provider,
        text_output,
    })
}

fn next(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    let value = args
        .get(*i + 1)
        .cloned()
        .ok_or_else(|| anyhow!("{flag} requiere un valor"))?;
    *i += 1;
    Ok(value)
}

fn required(parsed: &Parsed, name: &str) -> Result<String> {
    match name {
        "key" => parsed.key.clone().ok_or_else(|| anyhow!("falta --key")),
        "content" => parsed
            .content
            .clone()
            .ok_or_else(|| anyhow!("falta --content")),
        "id" => parsed.id.clone().ok_or_else(|| anyhow!("falta --id")),
        "summary" => parsed
            .summary
            .clone()
            .ok_or_else(|| anyhow!("falta --summary")),
        _ => Err(anyhow!("campo desconocido")),
    }
}

fn required_cwd(parsed: &Parsed) -> Result<PathBuf> {
    parsed.cwd.clone().ok_or_else(|| anyhow!("falta --cwd"))
}

fn usage() -> String {
    "tc-memory [--db PATH] <command> [options]\n\
     commands: health remember propose approve reject forget context search pending handoff export link\n\
     context prints JSON by default; add --text for an injectable context block\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::memory::test_support::{init_git_repo, make_worktree, temp_dir};

    #[test]
    fn cli_remember_then_context_from_a_second_cwd() {
        let root = temp_dir();
        let repo = root.join("repo");
        init_git_repo(&repo);
        let worktree = root.join("wt");
        make_worktree(&repo, &worktree, "other");
        let db = root.join("memory.db");

        let remember = run(&[
            "tc-memory".into(),
            "--db".into(),
            db.display().to_string(),
            "remember".into(),
            "--cwd".into(),
            repo.display().to_string(),
            "--key".into(),
            "architecture/auth-strategy".into(),
            "--content".into(),
            "Usar cookies HttpOnly".into(),
        ])
        .expect("remember");
        assert!(remember.contains("Usar cookies HttpOnly"), "{remember}");
        assert!(
            remember.contains("\"active\"") || remember.contains("active"),
            "{remember}"
        );

        let context = run(&[
            "tc-memory".into(),
            "--db".into(),
            db.display().to_string(),
            "context".into(),
            "--cwd".into(),
            worktree.display().to_string(),
        ])
        .expect("context");
        let context_json: serde_json::Value =
            serde_json::from_str(&context).expect("context stdout is valid JSON");
        assert!(
            context_json.to_string().contains("Usar cookies HttpOnly"),
            "el pack del segundo cwd tiene que traer la memoria: {context}"
        );
    }

    #[test]
    fn help_is_a_successful_output() {
        let help = run(&["tc-memory".into(), "--help".into()]).expect("help");
        assert!(help.contains("commands:"));
        assert!(help.contains("--text"));
    }

    #[test]
    fn unknown_handoff_subcommand_is_rejected() {
        let root = temp_dir();
        let error = run(&[
            "tc-memory".into(),
            "--db".into(),
            root.join("memory.db").display().to_string(),
            "handoff".into(),
            "destroy".into(),
            "--cwd".into(),
            root.display().to_string(),
            "--summary".into(),
            "should not be written".into(),
        ])
        .expect_err("unknown subcommands cannot create a handoff");
        assert!(error.to_string().contains("desconocido"));
    }
}
