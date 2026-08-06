//! Quoteo POSIX de paths para insertarlos en la línea de comandos de un
//! terminal.
//!
//! Es el mismo esquema que usa Terminal.app al arrastrar archivos: comillas
//! simples envolviendo el path, con cada `'` interna escapada como `'\''`.
//! Dentro de comillas simples POSIX no hay ningún otro carácter especial, así
//! que espacios, `$`, `~`, unicode y guiones iniciales quedan neutralizados.

/// Devuelve `path` como un token shell seguro para pegar en una línea de
/// comandos. Ejemplos:
/// - `/tmp/a b.txt` → `'/tmp/a b.txt'`
/// - `/tmp/it's.txt` → `'/tmp/it'\''s.txt'`
pub fn quote_path(path: &str) -> String {
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('\'');
    for ch in path.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::quote_path;

    /// Desarma el quoteo POSIX de comillas simples para verificar el
    /// round-trip: si `posix_unquote(quote_path(p)) == p` para un path
    /// arbitrario, el token es exactamente ese path y nada más.
    fn posix_unquote(token: &str) -> Option<String> {
        let mut out = String::new();
        let mut chars = token.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\'' => {
                    // Dentro de comillas simples hasta el cierre.
                    loop {
                        match chars.next()? {
                            '\'' => break,
                            other => out.push(other),
                        }
                    }
                }
                '\\' => {
                    // Escape fuera de comillas: el próximo char es literal.
                    out.push(chars.next()?);
                }
                _ => return None, // Un token seguro nunca tiene chars sueltos.
            }
        }
        Some(out)
    }

    #[test]
    fn a_plain_path_is_wrapped_in_single_quotes() {
        assert_eq!(quote_path("/tmp/archivo.txt"), "'/tmp/archivo.txt'");
    }

    #[test]
    fn single_quotes_are_escaped_posix_style() {
        // ' → '\'' : cierra la comilla, apóstrofo literal escapado, reabre.
        assert_eq!(quote_path("/tmp/it's.txt"), "'/tmp/it'\\''s.txt'");
    }

    #[test]
    fn nasty_paths_round_trip_exactly() {
        let cases = [
            "/tmp/a b c.txt",
            "/tmp/$(rm -rf ~); `x` $HOME *.txt",
            "-rf /",
            "/tmp/café/ñoño — 中文.txt",
            "''",
            "'",
            "\\backslash\\",
            "tabs\tand\nnewlines",
        ];
        for path in cases {
            let quoted = quote_path(path);
            assert_eq!(
                posix_unquote(&quoted).as_deref(),
                Some(path),
                "round-trip falló para {path:?} → {quoted:?}"
            );
        }
    }

    #[test]
    fn the_token_never_starts_with_a_dash() {
        // Arranca siempre con comilla: un path con guion no se vuelve flag.
        assert!(quote_path("-rf /").starts_with('\''));
        assert!(quote_path("--help").starts_with('\''));
    }
}
