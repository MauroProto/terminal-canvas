//! Secret redaction changes only matched spans, preserving surrounding data.

const INVISIBLE: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{00AD}', '\u{202A}', '\u{202B}',
    '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

pub fn prepare_content(text: &str) -> String {
    redact_secrets(&strip_invisible(text))
}

pub fn strip_invisible(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            (!INVISIBLE.contains(ch) && !ch.is_control()) || matches!(ch, '\n' | '\r' | '\t')
        })
        .collect()
}

struct Redaction {
    start: usize,
    end: usize,
    replacement: &'static str,
}

pub fn redact_secrets(text: &str) -> String {
    let mut spans = private_key_spans(text);
    let mut offset = 0;
    while offset < text.len() {
        if let Some((label, label_end)) = label_at(text, offset) {
            let separator = skip_whitespace(text, label_end);
            let bare_bearer = label.eq_ignore_ascii_case("bearer") && separator > label_end;
            let assignment = is_secret_label(label)
                && matches!(text.as_bytes().get(separator), Some(b'=' | b':'));
            if assignment || bare_bearer {
                let mut value_start = if assignment { separator + 1 } else { separator };
                value_start = skip_whitespace(text, value_start);
                if assignment && label.to_ascii_lowercase().ends_with("authorization") {
                    if let Some((scheme, end)) = label_at(text, value_start) {
                        if matches!(
                            scheme.to_ascii_lowercase().as_str(),
                            "bearer" | "basic" | "token"
                        ) && skip_whitespace(text, end) > end
                        {
                            value_start = skip_whitespace(text, end);
                        }
                    }
                }
                if let Some((start, end)) = secret_value_span(text, value_start) {
                    spans.push(Redaction {
                        start,
                        end,
                        replacement: "[redacted]",
                    });
                    offset = end;
                    continue;
                }
            }
            offset = label_end;
        } else {
            offset += text[offset..].chars().next().unwrap().len_utf8();
        }
    }

    // Provider tokens and credential-bearing URLs do not always have labels.
    // Lexing punctuation separately preserves JSON keys and shell assignments.
    let mut offset = 0;
    while offset < text.len() {
        let end = token_end(text, offset);
        if end > offset {
            if looks_like_secret(&text[offset..end]) {
                spans.push(Redaction {
                    start: offset,
                    end,
                    replacement: "[redacted]",
                });
            }
            offset = end;
        } else {
            offset += text[offset..].chars().next().unwrap().len_utf8();
        }
    }

    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut merged: Vec<Redaction> = Vec::new();
    for span in spans {
        if let Some(last) = merged.last_mut() {
            if span.start < last.end {
                last.end = last.end.max(span.end);
                continue;
            }
        }
        merged.push(span);
    }
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    for span in merged {
        output.push_str(&text[copied..span.start]);
        output.push_str(span.replacement);
        copied = span.end;
    }
    output.push_str(&text[copied..]);
    output
}

fn skip_whitespace(text: &str, start: usize) -> usize {
    let mut end = start;
    for ch in text[start..].chars() {
        if !ch.is_whitespace() {
            break;
        }
        end += ch.len_utf8();
    }
    end
}

fn label_at(text: &str, start: usize) -> Option<(&str, usize)> {
    let quoted = matches!(text.as_bytes().get(start), Some(b'"' | b'\'' | b'\x60'));
    let label_start = start + usize::from(quoted);
    let mut end = label_start;
    while text
        .as_bytes()
        .get(end)
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == b'_')
    {
        end += 1;
    }
    if end == label_start {
        return None;
    }
    if quoted {
        if text.as_bytes().get(end) != text.as_bytes().get(start) {
            return None;
        }
        Some((&text[label_start..end], end + 1))
    } else {
        Some((&text[label_start..end], end))
    }
}

fn secret_value_span(text: &str, start: usize) -> Option<(usize, usize)> {
    let first = *text.as_bytes().get(start)?;
    if text[start..].starts_with("[redacted]") {
        return Some((start, start + "[redacted]".len()));
    }
    if matches!(first, b'"' | b'\'' | b'\x60') {
        let mut escaped = false;
        for (offset, ch) in text[start + 1..].char_indices() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch as u32 == u32::from(first) {
                return Some((start + 1, start + 1 + offset));
            }
        }
        // An unterminated quoted secret remains secret through the end.
        return Some((start + 1, text.len()));
    }
    let end = text[start..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace() || matches!(ch, ',' | ';' | '}' | ']' | ')'))
        .map(|(offset, _)| start + offset)
        .unwrap_or(text.len());
    (end > start).then_some((start, end))
}

fn token_end(text: &str, start: usize) -> usize {
    let mut url = false;
    for (offset, ch) in text[start..].char_indices() {
        let at = start + offset;
        if ch == ':' && text[at..].starts_with("://") {
            url = true;
        }
        if ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | '\u{60}' | ',' | ';' | '{' | '}' | '[' | ']' | '(' | ')' | '='
            )
            || (ch == ':' && !url)
        {
            return at;
        }
    }
    text.len()
}

fn private_key_spans(text: &str) -> Vec<Redaction> {
    let upper = text.to_ascii_uppercase();
    let mut spans = Vec::new();
    let mut offset = 0;
    while let Some(begin) = upper[offset..].find("-----BEGIN") {
        let start = offset + begin;
        let line_end = upper[start..]
            .find('\n')
            .map(|n| start + n)
            .unwrap_or(text.len());
        if !upper[start..line_end].contains("PRIVATE KEY-----") {
            offset = line_end;
            continue;
        }
        let end = upper[line_end..]
            .find("-----END")
            .and_then(|end_marker| {
                let marker = line_end + end_marker;
                let end_line = upper[marker..]
                    .find('\n')
                    .map(|n| marker + n)
                    .unwrap_or(text.len());
                upper[marker..end_line]
                    .find("PRIVATE KEY-----")
                    .map(|suffix| marker + suffix + "PRIVATE KEY-----".len())
            })
            .unwrap_or(text.len());
        spans.push(Redaction {
            start,
            end,
            replacement: "[redacted private key]",
        });
        offset = end;
    }
    spans
}

fn looks_like_secret(token: &str) -> bool {
    if token.len() < 12 {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_live_",
        "sk_test_",
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "akia",
        "aiza",
        "eyj",
    ];
    PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) || contains_url_credentials(&lower)
}

fn is_secret_label(label: &str) -> bool {
    let normalized = label.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "access_key",
        "secret",
        "secret_key",
        "token",
        "auth_token",
        "authorization",
        "password",
        "passwd",
        "pwd",
        "private_key",
    ]
    .iter()
    .any(|needle| normalized == *needle || normalized.ends_with(&format!("_{needle}")))
}

fn contains_url_credentials(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .split_once('@')
        .is_some_and(|(credentials, _)| credentials.contains(':'))
}

#[cfg(test)]
mod tests {
    use super::{prepare_content, redact_secrets, strip_invisible};

    #[test]
    fn normal_content_preserves_spaces_indentation_and_line_endings() {
        let raw = "  let value = \"a  b\";\r\n\tif value {\n    print(value);\n}\n\n";
        assert_eq!(prepare_content(raw), raw);
        assert_eq!(strip_invisible("a\u{200B}b"), "ab");
    }

    #[test]
    fn redacting_prepared_content_is_idempotent() {
        for raw in [
            "OPENAI_API_KEY=sk-secretsecretsecret",
            "password=\"correct horse battery staple\"",
            "Authorization: Bearer opaque-value",
            "DATABASE_URL=postgres://user:password@example.com/db",
        ] {
            let prepared = prepare_content(raw);
            assert_eq!(prepare_content(&prepared), prepared);
        }
    }

    #[test]
    fn quoted_secrets_are_redacted_without_changing_surrounding_syntax() {
        for (raw, expected) in [
            (
                "password=\"correct horse battery staple\"",
                "password=\"[redacted]\"",
            ),
            (
                "password = 'correct horse battery staple'  # keep",
                "password = '[redacted]'  # keep",
            ),
            (
                r#"{"password":"correct horse battery staple","safe":"a  b"}"#,
                r#"{"password":"[redacted]","safe":"a  b"}"#,
            ),
            ("token: \"a\\\"b c\"\nkeep", "token: \"[redacted]\"\nkeep"),
            (
                "password: \"first\nsecond\"\nkeep",
                "password: \"[redacted]\"\nkeep",
            ),
            (
                "password: \"unterminated secret\nrest",
                "password: \"[redacted]",
            ),
        ] {
            assert_eq!(prepare_content(raw), expected, "input: {raw}");
        }
    }

    #[test]
    fn assignments_urls_bearer_tokens_and_private_keys_are_redacted() {
        let raw = "OPENAI_API_KEY=sk-secretsecretsecret\n\
                   Authorization: Bearer opaque-token-value\n\
                   Bearer opaque-standalone-token\n\
                   DATABASE_URL=postgres://user:password@example.com/db\n\
                   password = spaced-secret-value\n\
                   -----BEGIN OPENSSH PRIVATE KEY-----\n\
                   abcdefghijklmnopqrstuvwxyz\n\
                   -----END OPENSSH PRIVATE KEY-----\n\
                   keep this";
        let redacted = redact_secrets(raw);
        for leaked in [
            "sk-secret",
            "opaque-token-value",
            "opaque-standalone-token",
            "user:password",
            "spaced-secret-value",
            "abcdefghijklmnopqrstuvwxyz",
        ] {
            assert!(!redacted.contains(leaked), "secret leaked: {redacted}");
        }
        assert!(redacted.contains("OPENAI_API_KEY=[redacted]"));
        assert!(redacted.contains("[redacted private key]"));
        assert!(redacted.contains("keep this"));
        assert_eq!(
            prepare_content("token ghp_abcdefghijklmnopqrst keep\u{200B}me"),
            "token [redacted] keepme"
        );
    }
}
