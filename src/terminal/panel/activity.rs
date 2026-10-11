//! Heurísticas de actividad del panel: inferir qué está haciendo la terminal
//! a partir del texto visible (comandos, keywords) para labels de preview y
//! taskbar.

use super::*;

pub(super) fn infer_activity_label_from_term(
    display_title: &str,
    shell_title: &str,
    term: &Term<crate::terminal::pty::EventProxy>,
) -> Option<String> {
    let visible_text = visible_text_snapshot(term, 10, 120);
    infer_activity_label(display_title, shell_title, &visible_text)
}

pub(super) fn visible_text_snapshot(
    term: &Term<crate::terminal::pty::EventProxy>,
    max_lines: usize,
    max_cols: usize,
) -> String {
    let content = term.renderable_content();
    let display_offset = content.display_offset;
    let mut last_row = None;
    let mut current_line = String::new();
    let mut lines = Vec::new();

    for indexed in content.display_iter {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        if last_row != Some(point.line) {
            if !current_line.trim().is_empty() {
                lines.push(current_line.trim_end().to_owned());
            }
            current_line.clear();
            last_row = Some(point.line);
        }

        if current_line.chars().count() >= max_cols {
            continue;
        }

        let ch = indexed.cell.c;
        if ch == '\0' || indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        current_line.push(if ch.is_control() { ' ' } else { ch });
    }

    if !current_line.trim().is_empty() {
        lines.push(current_line.trim_end().to_owned());
    }

    let mut tail = lines
        .into_iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .collect::<Vec<_>>();
    tail.reverse();
    tail.join("\n")
}

pub(super) fn infer_activity_label(
    display_title: &str,
    shell_title: &str,
    visible_text: &str,
) -> Option<String> {
    if let Some(command) = extract_prompt_command(visible_text) {
        if let Some(label) = map_command_to_activity(&command) {
            return Some(label.to_owned());
        }
    }

    for source in [visible_text, shell_title, display_title] {
        if let Some(label) = detect_activity_keyword(source) {
            return Some(label.to_owned());
        }
    }

    None
}

pub(super) fn preview_label_text(activity_label: Option<&str>, fallback_title: &str) -> String {
    if let Some(activity_label) = activity_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        activity_label.to_owned()
    } else {
        sanitize_preview_title(fallback_title).unwrap_or_else(|| "Terminal".to_owned())
    }
}

pub(super) fn sanitize_preview_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub(super) fn detect_activity_keyword(source: &str) -> Option<&'static str> {
    use crate::utils::ascii_icontains;
    let source = source.trim();

    [
        ("openclaude", "OpenClaude"),
        ("claude code", "Claude Code"),
        ("claude-code", "Claude Code"),
        (" codex", "Codex"),
        ("codex ", "Codex"),
        ("aider", "Aider"),
        ("cursor", "Cursor"),
        ("gemini", "Gemini"),
        ("chatgpt", "ChatGPT"),
        ("claude", "Claude Code"),
    ]
    .into_iter()
    .find_map(|(needle, label)| ascii_icontains(source, needle).then_some(label))
}

pub(super) fn extract_prompt_command(visible_text: &str) -> Option<String> {
    for line in visible_text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for marker in [" % ", " $ ", "> "] {
            if let Some(index) = line.rfind(marker) {
                let tail = line[index + marker.len()..].trim();
                if tail.is_empty() {
                    continue;
                }
                let command = tail
                    .split_whitespace()
                    .next()
                    .map(|part| part.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`')))
                    .filter(|part| !part.is_empty())?;
                return Some(command.to_owned());
            }
        }
    }

    None
}

pub(super) fn map_command_to_activity(command: &str) -> Option<&'static str> {
    let command = command.trim();
    for (needle, label) in [
        ("openclaude", "OpenClaude"),
        ("claude", "Claude Code"),
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("aider", "Aider"),
        ("cursor", "Cursor"),
        ("cursor-agent", "Cursor"),
        ("gemini", "Gemini"),
        ("chatgpt", "ChatGPT"),
    ] {
        if command.eq_ignore_ascii_case(needle) {
            return Some(label);
        }
    }
    None
}

pub(super) fn is_generic_terminal_name(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("terminal")
        || trimmed.eq_ignore_ascii_case("shell")
}
pub(super) fn shell_label() -> String {
    let shell = default_shell();
    let shell_name = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shell");
    format!("-{shell_name}")
}

pub(super) fn cwd_label(cwd: Option<&Path>) -> String {
    cwd.and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Terminal")
        .to_owned()
}

/// Busca una URL en la celda (fila de viewport `row`, columna `col`) del
/// grid visible. Extrae el token contiguo alrededor de la celda y lo valida
/// como URL (scheme http/https o `www.`). Devuelve `None` si no hay URL.
pub(super) fn url_at_cell(
    term: &Term<crate::terminal::pty::EventProxy>,
    row: usize,
    col: usize,
) -> Option<String> {
    let content = term.renderable_content();
    let display_offset = content.display_offset;

    // Juntá los caracteres de la fila objetivo, ordenados por columna.
    let mut cells: Vec<(usize, char)> = Vec::new();
    for indexed in content.display_iter {
        let Some(point) = point_to_viewport(display_offset, indexed.point) else {
            continue;
        };
        if point.line != row {
            continue;
        }
        let ch = indexed.cell.c;
        if ch == '\0' || indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        cells.push((point.column.0, if ch.is_control() { ' ' } else { ch }));
    }
    if cells.is_empty() {
        return None;
    }
    cells.sort_by_key(|(column, _)| *column);

    // Mapeá columnas a índices de la fila (una columna por glifo).
    let cols: Vec<usize> = cells.iter().map(|(column, _)| *column).collect();
    let chars: Vec<char> = cells.iter().map(|(_, ch)| *ch).collect();
    let idx = cols.iter().position(|column| *column == col)?;

    let is_url_char = |c: char| {
        !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | '`' | '|' | '(' | ')')
    };
    let mut start = idx;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx;
    while end + 1 < chars.len() && is_url_char(chars[end + 1]) {
        end += 1;
    }
    let token: String = chars[start..=end].iter().collect();
    let token = token.trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    looks_like_url(token).then(|| token.to_owned())
}

fn looks_like_url(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www."))
        && token.chars().count() >= 8
}

#[cfg(test)]
mod url_tests {
    use super::{looks_like_url, url_at_cell};

    #[test]
    fn url_validation_requires_scheme_or_www() {
        assert!(looks_like_url("https://example.com/path"));
        assert!(looks_like_url("http://localhost:8080"));
        assert!(looks_like_url("www.example.com"));
        assert!(!looks_like_url("example.com"));
        assert!(!looks_like_url("notaurl"));
        assert!(!looks_like_url("http://"));
    }

    fn term_with(text: &str) -> alacritty_terminal::term::Term<crate::terminal::pty::EventProxy> {
        use alacritty_terminal::term::test::TermSize;
        use alacritty_terminal::term::{Config as TermConfig, Term};
        use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize::new(60, 5),
            crate::terminal::pty::EventProxy::new(tx),
        );
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, text.as_bytes());
        term
    }

    #[test]
    fn url_at_cell_extracts_url_under_cursor() {
        let term = term_with("visit https://example.com/page now");
        // "visit " ocupa cols 0-5; la URL arranca en col 6.
        let url = url_at_cell(&term, 0, 10);
        assert_eq!(url.as_deref(), Some("https://example.com/page"));
    }

    #[test]
    fn url_at_cell_returns_none_for_plain_text() {
        let term = term_with("hello world no links here");
        assert_eq!(url_at_cell(&term, 0, 3), None);
    }

    #[test]
    fn url_at_cell_strips_trailing_punctuation() {
        let term = term_with("see https://example.com/a.");
        let url = url_at_cell(&term, 0, 8);
        assert_eq!(url.as_deref(), Some("https://example.com/a"));
    }
}

impl TerminalPanel {
    pub fn orchestration_observation(&self, workspace_id: Uuid) -> PanelRuntimeObservation {
        let mut visible_text = String::new();
        let mut agent_status = None;
        let mut activity_revision = None;
        let mut text_snapshot_available = false;
        let attached = self.runtime_session_attached();
        let recent_output = self
            .with_pty(|pty| {
                if let Ok(term) = pty.term.try_lock() {
                    visible_text = visible_text_snapshot(&term, 16, 180);
                    text_snapshot_available = true;
                }
                agent_status = pty.agent_status_snapshot();
                activity_revision = Some(pty.activity_revision());
                pty.output_elapsed() <= Duration::from_secs(4)
            })
            .unwrap_or(false);

        PanelRuntimeObservation {
            panel_id: self.id,
            runtime_session_id: self.focused_runtime_session_id(),
            workspace_id,
            hook_claim: self.focused_hook_claim(),
            title: self.title.clone(),
            visible_text: if self.minimized || !attached {
                String::new()
            } else {
                visible_text
            },
            alive: self.leaf_is_alive(self.focused_leaf),
            recent_output: if self.minimized || !attached {
                false
            } else {
                recent_output
            },
            activity_revision,
            text_snapshot_available: text_snapshot_available && attached && !self.minimized,
            attached,
            minimized: self.minimized,
            agent_status: if self.minimized || !attached {
                None
            } else {
                agent_status
            },
        }
    }

    pub(crate) fn hook_claim_for_leaf(&self, leaf: Uuid) -> Option<Claim> {
        if !self.active_leaf_ids().contains(&leaf) {
            return None;
        }
        let session = self.leaf_session(leaf)?;
        let claim = session.hook_claim()?;
        (claim.key
            == LeafKey {
                workspace: self.workspace_id?,
                panel: self.id,
                leaf,
            }
            && Some(claim.runtime) == session.runtime_session_id()
            && claim.phase == ClaimPhase::Active
            && claim.owner.is_some())
        .then_some(claim)
    }

    pub(crate) fn focused_hook_claim(&self) -> Option<Claim> {
        self.hook_claim_for_leaf(self.focused_leaf)
    }

    pub(crate) fn all_hook_claims(&self) -> Vec<Claim> {
        self.active_leaf_ids()
            .into_iter()
            .filter_map(|leaf| self.hook_claim_for_leaf(leaf))
            .collect()
    }

    pub(crate) fn leaf_is_alive(&self, leaf: Uuid) -> bool {
        self.active_leaf_ids().contains(&leaf)
            && self
                .leaf_session(leaf)
                .is_some_and(SessionController::is_alive)
    }

    /// Shared actual publication boundary: an ended owned Stop can finish its
    /// own record, while all other kinds need the exact leaf to remain alive.
    pub(crate) fn hook_publication_liveness(
        current: Option<Claim>,
        claim: Claim,
        alive: bool,
        kind: crate::orchestration::HookKind,
    ) -> Option<bool> {
        if current != Some(claim)
            || claim.phase != ClaimPhase::Active
            || claim.owner.is_none()
            || (!alive && kind != crate::orchestration::HookKind::Stop)
        {
            return None;
        }
        Some(alive)
    }

    /// The envelope retains its durable lease through this exact target check
    /// and the subsequent cache/activity publication. Metadata needs a live leaf.
    pub(crate) fn publish_hook_session_id(
        &mut self,
        claim: Claim,
        session_id: Option<String>,
        kind: crate::orchestration::HookKind,
    ) -> Option<bool> {
        let alive = Self::hook_publication_liveness(
            self.hook_claim_for_leaf(claim.key.leaf),
            claim,
            self.leaf_is_alive(claim.key.leaf),
            kind,
        )?;
        if alive {
            if let Some(session_id) = session_id {
                self.set_agent_session_id_for_leaf(claim.key.leaf, Some(session_id));
            }
        }
        Some(alive)
    }
}
