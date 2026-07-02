#![allow(dead_code)]

use super::manager::{
    AgentProvider, AgentSessionMeta, AgentStatus, InboxEvent, Orchestrator, TaskCard,
};

pub(super) fn session_matches_query(
    orchestrator: &Orchestrator,
    session: &AgentSessionMeta,
    query: &str,
) -> bool {
    use crate::utils::ascii_icontains;
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    if ascii_icontains(&session.label, query)
        || ascii_icontains(session.provider.label(), query)
        || ascii_icontains(session.status.label(), query)
    {
        return true;
    }
    if let Some(branch) = &session.branch {
        if ascii_icontains(branch, query) {
            return true;
        }
    }
    if let Some(task) = session
        .task_id
        .and_then(|task_id| orchestrator.tasks().iter().find(|task| task.id == task_id))
    {
        if ascii_icontains(&task.title, query) || ascii_icontains(task.state.label(), query) {
            return true;
        }
    }
    if let Some(error) = &session.review_summary.last_error {
        if ascii_icontains(error, query) {
            return true;
        }
    }
    if let Some(success) = &session.review_summary.last_success {
        if ascii_icontains(success, query) {
            return true;
        }
    }
    if let Some(summary) = &session.command_summary {
        if ascii_icontains(&summary.title, query) || ascii_icontains(&summary.excerpt, query) {
            return true;
        }
    }
    false
}

pub(super) fn task_matches_query(
    orchestrator: &Orchestrator,
    task: &TaskCard,
    query: &str,
) -> bool {
    use crate::utils::ascii_icontains;
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    if ascii_icontains(&task.title, query)
        || ascii_icontains(&task.brief, query)
        || ascii_icontains(task.state.label(), query)
    {
        return true;
    }
    task.session_ids.iter().any(|session_id| {
        orchestrator
            .sessions()
            .iter()
            .find(|session| session.session_id == *session_id)
            .map(|session| session_matches_query(orchestrator, session, query))
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
    use crate::utils::ascii_icontains;
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    ascii_icontains(&event.title, query) || ascii_icontains(&event.summary, query)
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
