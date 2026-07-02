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
    format!("-{}", shell_name)
}

pub(super) fn cwd_label(cwd: Option<&Path>) -> String {
    cwd.and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Terminal")
        .to_owned()
}
