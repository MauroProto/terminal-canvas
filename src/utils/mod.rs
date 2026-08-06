pub mod diagnostics;
pub mod platform;

pub fn ascii_icontains(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len())
        .any(|window| window.eq_ignore_ascii_case(n))
}

/// Como [`ascii_icontains`] pero exigiendo borde de palabra: el carácter
/// anterior y posterior al match (si existen) no puede ser alfanumérico ni
/// `_`. Indispensable para nombres cortos de agente: sin borde, "amp"
/// matchearía dentro de "example" o "sample" y pintaría mal la taskbar.
pub fn ascii_icontains_word(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if h.len() < n.len() {
        return false;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    h.windows(n.len()).enumerate().any(|(start, window)| {
        if !window.eq_ignore_ascii_case(n) {
            return false;
        }
        let before_ok = start == 0 || !is_word_byte(h[start - 1]);
        let end = start + n.len();
        let after_ok = end == h.len() || !is_word_byte(h[end]);
        before_ok && after_ok
    })
}

#[cfg(test)]
mod tests {
    use super::{ascii_icontains, ascii_icontains_word};

    #[test]
    fn substring_match_is_case_insensitive() {
        assert!(ascii_icontains("Claude Code", "claude"));
        assert!(ascii_icontains("ejemplo", "EMP"));
    }

    #[test]
    fn word_match_accepts_isolated_tokens() {
        assert!(ascii_icontains_word("amp", "amp"));
        assert!(ascii_icontains_word("amp --model x", "amp"));
        assert!(ascii_icontains_word("run amp now", "amp"));
        assert!(ascii_icontains_word("AMP here", "amp"));
        assert!(ascii_icontains_word("-amp-", "amp"));
    }

    #[test]
    fn word_match_rejects_embedded_substrings() {
        // Regresión: sin borde de palabra, "amp" matchea dentro de estas.
        assert!(!ascii_icontains_word("example", "amp"));
        assert!(!ascii_icontains_word("sample of text", "amp"));
        assert!(!ascii_icontains_word("lamp", "amp"));
        assert!(!ascii_icontains_word("amplifier", "amp"));
        assert!(!ascii_icontains_word("my_amp", "amp"));
    }

    #[test]
    fn word_match_is_case_insensitive_but_still_bounded() {
        assert!(ascii_icontains_word("ExAmPLe", "example"));
        assert!(!ascii_icontains_word("ExAmPLe", "amp"));
    }
}
