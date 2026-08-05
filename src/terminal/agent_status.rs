//! Canal de estado de agentes in-band (protocolo OSC 9999, compatible con
//! orca): los agentes —o wrappers inyectados por ellos— emiten
//! `\x1b]9999;<json>\x07` con su estado (`working`/`blocked`/`waiting`/
//! `done`, herramienta en curso, prompt). El lector del PTY recorta estas
//! secuencias del stream antes de que lleguen al parser VT y las convierte
//! en reports autoritativos que la orquestación prioriza sobre las
//! heurísticas de texto visible.

use serde::Deserialize;

/// Código OSC de estado de agente (protocolo orca) y de cwd del shell.
const OSC_CODE_AGENT_STATUS: u32 = 9999;
const OSC_CODE_CWD: u32 = 7;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_TOOL_CHARS: usize = 160;
const MAX_PROMPT_CHARS: usize = 16_384;
/// Tope de dígitos para el código OSC: más que eso no es un código válido y
/// evita bufferizar sin límite un stream malformado.
const MAX_CODE_DIGITS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusState {
    Working,
    Blocked,
    Waiting,
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStatusReport {
    pub state: AgentStatusState,
    pub tool: Option<String>,
    pub prompt: Option<String>,
    pub received_at_ms: i64,
}

#[derive(Deserialize)]
struct AgentStatusPayload {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

/// Resultado de interceptar un OSC: estado de agente o cwd del shell.
#[derive(Debug, Clone, PartialEq)]
pub enum OscCapture {
    AgentStatus(AgentStatusReport),
    Cwd(String),
}

/// Quita secuencias OSC 9999 (estado de agente) y OSC 7 (cwd) de un stream
/// de bytes del PTY, manejando secuencias partidas entre chunks. Inspirado en
/// `createAgentStatusOscProcessor` de orca.
#[derive(Default)]
pub struct AgentStatusStream {
    /// Bytes de una posible secuencia OSC aún sin clasificar (frontera de
    /// chunk o dígitos del código).
    seq: Vec<u8>,
    /// Código OSC confirmado (9999 o 7) cuyo payload estamos capturando.
    osc_code: Option<u32>,
    /// Se vio un ESC dentro del payload: el próximo byte decide si era ST.
    expect_st: bool,
    payload: Vec<u8>,
    truncated: bool,
}

impl AgentStatusStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Procesa un chunk del PTY. Devuelve los bytes limpios (sin los OSC
    /// interceptados), los reports de agente y los cwds. `now_ms` sella cada
    /// report para el chequeo de frescura posterior.
    pub fn process(
        &mut self,
        chunk: &[u8],
        now_ms: i64,
    ) -> (Vec<u8>, Vec<AgentStatusReport>, Vec<String>) {
        // Fast path: sin estado arrastrado y sin ESC no hay nada que recortar.
        if self.seq.is_empty()
            && self.osc_code.is_none()
            && !self.expect_st
            && !chunk.contains(&0x1b)
        {
            return (chunk.to_vec(), Vec::new(), Vec::new());
        }

        let mut clean: Vec<u8> = Vec::with_capacity(chunk.len());
        let mut reports = Vec::new();
        let mut cwds = Vec::new();

        for &byte in chunk {
            if self.expect_st {
                self.expect_st = false;
                if byte == b'\\' {
                    // Era el ST: se consume (ESC + \\) y no se emite nada.
                    continue;
                }
                // No era ST: el ESC abortó el OSC y arranca otra secuencia;
                // re-procesalo junto con este byte.
                self.seq.push(0x1b);
                self.seq.push(byte);
                self.classify_seq(&mut clean);
                continue;
            }
            if let Some(code) = self.osc_code {
                self.payload_byte(byte, code, now_ms, &mut reports, &mut cwds);
                continue;
            }
            if !self.seq.is_empty() {
                self.seq.push(byte);
                self.classify_seq(&mut clean);
                continue;
            }
            if byte == 0x1b {
                self.seq.push(byte);
            } else {
                clean.push(byte);
            }
        }

        (clean, reports, cwds)
    }

    /// Un byte dentro del payload de un OSC confirmado.
    fn payload_byte(
        &mut self,
        byte: u8,
        code: u32,
        now_ms: i64,
        reports: &mut Vec<AgentStatusReport>,
        cwds: &mut Vec<String>,
    ) {
        match byte {
            0x07 => self.finish_payload(code, now_ms, reports, cwds),
            0x1b => {
                // ESC dentro del payload: termina el OSC. El próximo byte dice
                // si era ST (\\) o un ESC que arranca otra secuencia.
                self.finish_payload(code, now_ms, reports, cwds);
                self.expect_st = true;
            }
            other => {
                if self.payload.len() < MAX_PAYLOAD_BYTES {
                    self.payload.push(other);
                } else {
                    self.truncated = true;
                }
            }
        }
    }

    /// Clasifica `seq` (bytes de una posible secuencia OSC) tras cada byte.
    fn classify_seq(&mut self, clean: &mut Vec<u8>) {
        // seq siempre arranca con 0x1b.
        if self.seq.len() == 1 {
            // Solo el ESC: esperar el próximo byte.
            return;
        }
        if self.seq[1] != b']' {
            // No es OSC: descargar tal cual y volver al estado normal. El
            // último byte podría ser a su vez un ESC nuevo.
            let last = *self.seq.last().unwrap();
            let mut bytes = std::mem::take(&mut self.seq);
            if last == 0x1b {
                // Re-procesá el ESC final como inicio de secuencia.
                let esc = bytes.pop();
                clean.extend_from_slice(&bytes);
                if let Some(esc) = esc {
                    self.seq.push(esc);
                }
            } else {
                clean.extend_from_slice(&bytes);
            }
            return;
        }
        // seq = \x1b] <algo>. El resto debe ser dígitos y eventualmente ';'.
        let body = &self.seq[2..];
        if body.is_empty() {
            return;
        }
        for &b in body {
            if b == b';' {
                // Fin del código: decidir si nos interesa.
                let digits = &body[..body.len() - 1];
                let code = parse_code(digits);
                let seq_bytes = std::mem::take(&mut self.seq);
                match code {
                    Some(OSC_CODE_AGENT_STATUS) | Some(OSC_CODE_CWD) => {
                        self.osc_code = code;
                        self.payload.clear();
                        self.truncated = false;
                        let _ = seq_bytes; // descartados (stripped)
                    }
                    _ => {
                        clean.extend_from_slice(&seq_bytes);
                    }
                }
                return;
            }
            if !b.is_ascii_digit() {
                // Código no numérico: no es un OSC que interceptemos.
                let seq_bytes = std::mem::take(&mut self.seq);
                clean.extend_from_slice(&seq_bytes);
                return;
            }
        }
        if body.len() > MAX_CODE_DIGITS {
            // Demasiados dígitos sin ';': no es un código válido.
            let seq_bytes = std::mem::take(&mut self.seq);
            clean.extend_from_slice(&seq_bytes);
        }
    }

    fn finish_payload(
        &mut self,
        code: u32,
        now_ms: i64,
        reports: &mut Vec<AgentStatusReport>,
        cwds: &mut Vec<String>,
    ) {
        self.osc_code = None;
        let truncated = self.truncated;
        self.truncated = false;
        if !truncated && !self.payload.is_empty() {
            match code {
                OSC_CODE_AGENT_STATUS => {
                    if let Some(report) = parse_agent_status_payload(&self.payload, now_ms) {
                        reports.push(report);
                    }
                }
                OSC_CODE_CWD => {
                    if let Some(cwd) = parse_osc7_cwd(&self.payload) {
                        cwds.push(cwd);
                    }
                }
                _ => {}
            }
        }
        self.payload.clear();
    }
}

fn parse_code(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Extrae el path de un payload OSC 7 (`file://host/path`). Acepta también un
/// path absoluto sin scheme.
fn parse_osc7_cwd(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("file://") {
        // file://host/path → quedarnos con el path (host puede ser vacío).
        let path_start = rest.find('/')?;
        let path = &rest[path_start..];
        if path.is_empty() {
            return None;
        }
        return Some(path.to_owned());
    }
    if text.starts_with('/') {
        return Some(text);
    }
    None
}

fn parse_agent_status_payload(bytes: &[u8], now_ms: i64) -> Option<AgentStatusReport> {
    let payload: AgentStatusPayload = serde_json::from_slice(bytes).ok()?;
    let state = match payload
        .state
        .as_deref()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "working" | "running" | "busy" => AgentStatusState::Working,
        "blocked" | "approval" | "permission" => AgentStatusState::Blocked,
        "waiting" | "input" | "needs_input" => AgentStatusState::Waiting,
        "done" | "idle" | "complete" | "finished" => AgentStatusState::Done,
        _ => return None,
    };
    Some(AgentStatusReport {
        state,
        tool: clamp_field(payload.tool, MAX_TOOL_CHARS),
        prompt: clamp_field(payload.prompt, MAX_PROMPT_CHARS),
        received_at_ms: now_ms,
    })
}

fn clamp_field(value: Option<String>, max_chars: usize) -> Option<String> {
    let value = value?.trim().to_owned();
    if value.is_empty() {
        return None;
    }
    let count = value.chars().count();
    if count <= max_chars {
        Some(value)
    } else {
        Some(value.chars().take(max_chars).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentStatusState, AgentStatusStream};

    fn process_all(
        stream: &mut AgentStatusStream,
        chunks: &[&[u8]],
    ) -> (Vec<u8>, Vec<super::AgentStatusReport>) {
        let mut clean = Vec::new();
        let mut reports = Vec::new();
        for (offset, chunk) in chunks.iter().enumerate() {
            let (chunk_clean, chunk_reports, _cwds) = stream.process(chunk, 1_000 + offset as i64);
            clean.extend_from_slice(&chunk_clean);
            reports.extend(chunk_reports);
        }
        (clean, reports)
    }

    #[test]
    fn strips_and_parses_bel_terminated_status() {
        let mut stream = AgentStatusStream::new();
        let input = b"before \x1b]9999;{\"state\":\"working\",\"tool\":\"Edit\"}\x07after";

        let (clean, reports, cwds) = stream.process(input, 42);

        assert_eq!(clean, b"before after");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, AgentStatusState::Working);
        assert_eq!(reports[0].tool.as_deref(), Some("Edit"));
        assert_eq!(reports[0].received_at_ms, 42);
        assert!(cwds.is_empty());
    }

    #[test]
    fn strips_st_terminated_status() {
        let mut stream = AgentStatusStream::new();
        let input = b"\x1b]9999;{\"state\":\"done\"}\x1b\\tail";

        let (clean, reports, _cwds) = stream.process(input, 7);

        assert_eq!(clean, b"tail");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, AgentStatusState::Done);
    }

    #[test]
    fn sequence_split_across_chunks_is_reassembled() {
        let mut stream = AgentStatusStream::new();
        let (clean, reports) = process_all(
            &mut stream,
            &[
                b"pre\x1b]99",
                b"99;{\"state\":\"blocked\"",
                b",\"prompt\":\"allow?\"}\x07post",
            ],
        );

        assert_eq!(clean, b"prepost");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, AgentStatusState::Blocked);
        assert_eq!(reports[0].prompt.as_deref(), Some("allow?"));
    }

    #[test]
    fn unrelated_escapes_pass_through_untouched() {
        let mut stream = AgentStatusStream::new();
        let input = b"\x1b]0;title\x07\x1b[31mred\x1b]999;other\x07";

        let (clean, reports, _cwds) = stream.process(input, 1);

        assert_eq!(clean, input);
        assert!(reports.is_empty());
    }

    #[test]
    fn chunk_without_escapes_passes_through() {
        let mut stream = AgentStatusStream::new();
        let input = b"plain output";

        let (clean, reports, cwds) = stream.process(input, 1);

        assert_eq!(clean, input);
        assert!(reports.is_empty());
        assert!(cwds.is_empty());
    }

    #[test]
    fn invalid_json_is_stripped_without_report() {
        let mut stream = AgentStatusStream::new();
        let input = b"a\x1b]9999;not-json\x07b";

        let (clean, reports, _cwds) = stream.process(input, 1);

        assert_eq!(clean, b"ab");
        assert!(reports.is_empty());
    }

    #[test]
    fn unknown_state_is_stripped_without_report() {
        let mut stream = AgentStatusStream::new();
        let input = b"a\x1b]9999;{\"state\":\"levitating\"}\x07b";

        let (clean, reports, _cwds) = stream.process(input, 1);

        assert_eq!(clean, b"ab");
        assert!(reports.is_empty());
    }

    #[test]
    fn payload_overflow_is_discarded_not_leaked() {
        let mut stream = AgentStatusStream::new();
        let mut input = Vec::from(b"\x1b]9999;".as_slice());
        input.extend(std::iter::repeat_n(b'x', super::MAX_PAYLOAD_BYTES + 16));
        input.extend_from_slice(b"\x07ok");

        let (clean, reports, _cwds) = stream.process(&input, 1);

        assert_eq!(clean, b"ok");
        assert!(reports.is_empty());
    }

    #[test]
    fn esc_inside_payload_aborts_and_next_sequence_still_parses() {
        let mut stream = AgentStatusStream::new();
        let input = b"\x1b]9999;{\"state\":\"working\"\x1b]9999;{\"state\":\"done\"}\x07";

        let (clean, reports, _cwds) = stream.process(input, 1);

        assert_eq!(clean, b"");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, AgentStatusState::Done);
    }

    #[test]
    fn state_aliases_map_to_canonical_states() {
        let mut stream = AgentStatusStream::new();
        for (alias, expected) in [
            ("running", AgentStatusState::Working),
            ("approval", AgentStatusState::Blocked),
            ("needs_input", AgentStatusState::Waiting),
            ("idle", AgentStatusState::Done),
        ] {
            let input = format!("\x1b]9999;{{\"state\":\"{alias}\"}}\x07");
            let (clean, reports, _cwds) = stream.process(input.as_bytes(), 1);
            assert!(clean.is_empty());
            assert_eq!(reports.len(), 1, "alias {alias}");
            assert_eq!(reports[0].state, expected, "alias {alias}");
        }
    }

    #[test]
    fn osc7_cwd_is_stripped_and_captured() {
        let mut stream = AgentStatusStream::new();
        let input = b"x\x1b]7;file://host/Users/me/proj\x07y";

        let (clean, reports, cwds) = stream.process(input, 1);

        assert_eq!(clean, b"xy");
        assert!(reports.is_empty());
        assert_eq!(cwds, vec!["/Users/me/proj".to_owned()]);
    }

    #[test]
    fn osc7_cwd_split_across_chunks() {
        let mut stream = AgentStatusStream::new();
        let mut clean = Vec::new();
        let mut cwds = Vec::new();
        for chunk in [b"a\x1b]7;file://h/tmp".as_slice(), b"/dir\x07b".as_slice()] {
            let (c, _r, w) = stream.process(chunk, 1);
            clean.extend_from_slice(&c);
            cwds.extend(w);
        }
        assert_eq!(clean, b"ab");
        assert_eq!(cwds, vec!["/tmp/dir".to_owned()]);
    }

    #[test]
    fn osc7_and_agent_status_coexist() {
        let mut stream = AgentStatusStream::new();
        let input = b"\x1b]7;file://h/tmp\x07\x1b]9999;{\"state\":\"working\"}\x07";

        let (clean, reports, cwds) = stream.process(input, 1);

        assert_eq!(clean, b"");
        assert_eq!(reports.len(), 1);
        assert_eq!(cwds, vec!["/tmp".to_owned()]);
    }

    #[test]
    fn parse_osc7_cwd_variants() {
        use super::parse_osc7_cwd;
        assert_eq!(
            parse_osc7_cwd(b"file://localhost/Users/x"),
            Some("/Users/x".to_owned())
        );
        assert_eq!(
            parse_osc7_cwd(b"file:///Users/x"),
            Some("/Users/x".to_owned())
        );
        assert_eq!(parse_osc7_cwd(b"/Users/x"), Some("/Users/x".to_owned()));
        assert_eq!(parse_osc7_cwd(b"relative/path"), None);
        assert_eq!(parse_osc7_cwd(b""), None);
        assert_eq!(parse_osc7_cwd(b"file://host"), None);
    }
}
