//! Redacción de secretos y limpieza de Unicode invisible antes de persistir.

const INVISIBLE: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{00AD}', '\u{202A}', '\u{202B}',
    '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

pub fn prepare_content(text: &str) -> String {
    redact_secrets(&strip_invisible(text))
}

pub fn strip_invisible(text: &str) -> String {
    text.chars()
        .filter(|ch| (!INVISIBLE.contains(ch) && !ch.is_control()) || *ch == '\n' || *ch == '\t')
        .collect()
}

pub fn redact_secrets(text: &str) -> String {
    let mut output = Vec::new();
    let mut inside_private_key = false;
    for line in text.lines() {
        let upper = line.to_ascii_uppercase();
        if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
            inside_private_key = true;
            output.push("[redacted private key]".to_owned());
            continue;
        }
        if inside_private_key {
            if upper.contains("-----END") && upper.contains("PRIVATE KEY-----") {
                inside_private_key = false;
            }
            continue;
        }
        output.push(redact_line(line));
    }
    output.join("\n")
}

fn looks_like_secret(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | ';' | '.'));
    if trimmed.len() < 12 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
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
    if PREFIXES.iter().any(|prefix| lower.contains(prefix)) {
        return true;
    }
    if contains_url_credentials(&lower) {
        return true;
    }
    false
}

fn redact_line(line: &str) -> String {
    let leading = &line[..line.len() - line.trim_start().len()];
    let tokens: Vec<_> = line.split_whitespace().collect();
    let mut output = Vec::new();
    let mut secret_context = false;
    let mut index = 0;

    while index < tokens.len() {
        let token = tokens[index];
        let clean = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '`' | ',' | ';' | '.' | '(' | ')' | '[' | ']'
            )
        });
        let lower = clean.to_ascii_lowercase();

        if secret_context {
            if matches!(lower.as_str(), "bearer" | "basic" | "token") {
                output.push(token.to_owned());
                index += 1;
                continue;
            }
            output.push("[redacted]".to_owned());
            secret_context = false;
            index += 1;
            continue;
        }

        // Los logs y handoffs suelen incluir `Bearer <credencial>` sin la
        // etiqueta Authorization. Es una señal inequívoca para ocultar el
        // token siguiente antes de persistirlo.
        if lower == "bearer" {
            output.push(token.to_owned());
            secret_context = true;
            index += 1;
            continue;
        }

        let next_is_separator = tokens
            .get(index + 1)
            .is_some_and(|next| matches!(*next, "=" | ":"));
        if is_secret_label(clean) && next_is_separator {
            output.push(token.to_owned());
            output.push(tokens[index + 1].to_owned());
            secret_context = true;
            index += 2;
            continue;
        }

        if let Some((label, value)) = split_assignment(clean) {
            if is_secret_label(label) && !value.is_empty() {
                let separator = if clean.contains('=') { '=' } else { ':' };
                output.push(format!("{label}{separator}[redacted]"));
                index += 1;
                continue;
            }
        }
        if (clean.ends_with('=') || clean.ends_with(':'))
            && is_secret_label(clean.trim_end_matches(['=', ':']))
        {
            output.push(token.to_owned());
            secret_context = true;
            index += 1;
            continue;
        }
        if looks_like_secret(clean) {
            output.push("[redacted]".to_owned());
        } else {
            output.push(token.to_owned());
        }
        index += 1;
    }

    format!("{leading}{}", output.join(" "))
}

fn split_assignment(value: &str) -> Option<(&str, &str)> {
    value
        .split_once('=')
        .or_else(|| value.split_once(':').filter(|_| !value.contains("://")))
}

fn is_secret_label(label: &str) -> bool {
    let normalized = label
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .to_ascii_lowercase();
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
    let Some((authority, _)) = rest.split_once('@') else {
        return false;
    };
    authority.contains(':')
}

#[cfg(test)]
mod tests {
    use super::{prepare_content, redact_secrets, strip_invisible};

    #[test]
    fn secrets_are_redacted_and_invisible_unicode_is_stripped() {
        let raw = format!("token ghp_{} keep{}", "a".repeat(20), "\u{200B}me");
        let prepared = prepare_content(&raw);
        assert!(!prepared.contains("ghp_"), "{prepared}");
        assert!(prepared.contains("[redacted]"));
        assert!(prepared.contains("keepme"));
        assert!(!strip_invisible("a\u{200B}b").contains('\u{200B}'));
        assert_eq!(redact_secrets("hello world"), "hello world");
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
    }
}
