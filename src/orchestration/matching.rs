#![allow(dead_code)]

use super::manager::{
    AgentProvider, AgentSessionMeta, AgentStatus, InboxEvent, Orchestrator, TaskCard,
};

pub(super) fn session_matches_query(
    orchestrator: &Orchestrator,
    session: &AgentSessionMeta,
    query: &str,
) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    let mut haystacks = vec![
        session.label.to_ascii_lowercase(),
        session.provider.label().to_ascii_lowercase(),
        session.status.label().to_ascii_lowercase(),
    ];
    if let Some(branch) = &session.branch {
        haystacks.push(branch.to_ascii_lowercase());
    }
    if let Some(task) = session
        .task_id
        .and_then(|task_id| orchestrator.tasks().iter().find(|task| task.id == task_id))
    {
        haystacks.push(task.title.to_ascii_lowercase());
        haystacks.push(task.state.label().to_ascii_lowercase());
    }
    if let Some(error) = &session.review_summary.last_error {
        haystacks.push(error.to_ascii_lowercase());
    }
    if let Some(success) = &session.review_summary.last_success {
        haystacks.push(success.to_ascii_lowercase());
    }
    if let Some(summary) = &session.command_summary {
        haystacks.push(summary.title.to_ascii_lowercase());
        haystacks.push(summary.excerpt.to_ascii_lowercase());
    }
    haystacks
        .into_iter()
        .any(|haystack| haystack.contains(&query))
}

pub(super) fn task_matches_query(
    orchestrator: &Orchestrator,
    task: &TaskCard,
    query: &str,
) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    if task.title.to_ascii_lowercase().contains(&query)
        || task.brief.to_ascii_lowercase().contains(&query)
        || task.state.label().to_ascii_lowercase().contains(&query)
    {
        return true;
    }
    task.session_ids.iter().any(|session_id| {
        orchestrator
            .sessions()
            .iter()
            .find(|session| session.session_id == *session_id)
            .map(|session| session_matches_query(orchestrator, session, query.as_str()))
            .unwrap_or(false)
    })
}

pub(super) fn task_matches_filters(
    orchestrator: &Orchestrator,
    task: &TaskCard,
    provider_filter: Option<AgentProvider>,
    status_filter: Option<AgentStatus>,
) -> bool {
    let provider_matches = provider_filter.map(|provider| {
        task.provider_hint == Some(provider)
            || task.session_ids.iter().any(|session_id| {
                orchestrator
                    .sessions()
                    .iter()
                    .find(|session| session.session_id == *session_id)
                    .map(|session| session.provider == provider)
                    .unwrap_or(false)
            })
    });

    let status_matches = status_filter.map(|status| {
        task.session_ids.iter().any(|session_id| {
            orchestrator
                .sessions()
                .iter()
                .find(|session| session.session_id == *session_id)
                .map(|session| session.status == status)
                .unwrap_or(false)
        })
    });

    provider_matches.unwrap_or(true) && status_matches.unwrap_or(true)
}

pub(super) fn inbox_matches_query(event: &InboxEvent, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    event.title.to_ascii_lowercase().contains(&query)
        || event.summary.to_ascii_lowercase().contains(&query)
}

pub(super) fn event_matches_filters(
    orchestrator: &Orchestrator,
    event: &InboxEvent,
    provider_filter: Option<AgentProvider>,
    status_filter: Option<AgentStatus>,
) -> bool {
    let Some(session_id) = event.session_id else {
        return provider_filter.is_none() && status_filter.is_none();
    };
    let Some(session) = orchestrator
        .sessions()
        .iter()
        .find(|session| session.session_id == session_id)
    else {
        return false;
    };
    provider_filter
        .map(|provider| session.provider == provider)
        .unwrap_or(true)
        && status_filter
            .map(|status| session.status == status)
            .unwrap_or(true)
}
