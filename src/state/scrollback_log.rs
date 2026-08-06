//! Log incremental del scrollback por panel: en vez de reescribir el
//! checkpoint completo cada autosave (2 s), se appendean frames pequeños
//! (output/resize/clear). El checkpoint completo solo se reescribe cuando el
//! log supera 1 MB, en un cierre limpio, o al cerrar un panel.
//!
//! Formato binario:
//!   header: `MTLG` + u8 versión + u32 generation (LE)
//!   frame:  u8 kind (1=output, 2=resize, 3=clear) + u32 len (LE) + payload
//!
//! `read_frames` tolera una cola truncada (crash a mitad de frame): devuelve
//! solo los frames completos y reporta la `generation` del header.

use std::path::Path;

pub const MAGIC: &[u8; 4] = b"MTLG";
pub const VERSION: u8 = 1;

/// Tope del log antes de forzar un checkpoint completo (y reset con
/// generation+1).
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Output = 1,
    Resize = 2,
    Clear = 3,
}

impl FrameKind {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Output),
            2 => Some(Self::Resize),
            3 => Some(Self::Clear),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

/// Header del log: magic + versión + generation.
pub fn encode_header(generation: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&generation.to_le_bytes());
    out
}

/// Frame: kind + len + payload.
pub fn encode_frame(kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind as u8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Payload de un frame de resize: cols u16 + rows u16 (LE).
pub fn resize_payload(cols: u16, rows: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.extend_from_slice(&cols.to_le_bytes());
    out.extend_from_slice(&rows.to_le_bytes());
    out
}

/// Parsea un resize payload.
pub fn parse_resize(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() < 4 {
        return None;
    }
    let cols = u16::from_le_bytes([payload[0], payload[1]]);
    let rows = u16::from_le_bytes([payload[2], payload[3]]);
    Some((cols, rows))
}

/// Lee el log completo. Devuelve `None` si el magic/versión no coinciden
/// (generation mismatch lo decide el caller comparando generations).
/// Una cola truncada (crash a mitad de frame) se descarta: solo se devuelven
/// los frames completos.
pub fn read_frames(bytes: &[u8]) -> Option<(u32, Vec<Frame>)> {
    if bytes.len() < 9 || &bytes[0..4] != MAGIC || bytes[4] != VERSION {
        return None;
    }
    let generation = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    let mut frames = Vec::new();
    let mut cursor = 9usize;
    while cursor + 5 <= bytes.len() {
        let kind = match FrameKind::from_u8(bytes[cursor]) {
            Some(kind) => kind,
            None => break, // byte corrupto: cortamos acá
        };
        let len = u32::from_le_bytes([
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
            bytes[cursor + 4],
        ]) as usize;
        let end = cursor + 5 + len;
        if end > bytes.len() {
            break; // frame truncado a mitad: descartamos la cola
        }
        frames.push(Frame {
            kind,
            payload: bytes[cursor + 5..end].to_vec(),
        });
        cursor = end;
    }
    Some((generation, frames))
}

/// Crea un log nuevo (o lo resetea) con la generation dada.
pub fn reset_log(path: &Path, generation: u32) -> std::io::Result<()> {
    std::fs::write(path, encode_header(generation))
}

/// Appendea bytes de frames al log. Si el archivo no existe, lo crea con
/// header de generation 0 primero.
pub fn append_frames(path: &Path, frames_bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if !path.exists() {
        std::fs::write(path, encode_header(0))?;
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(frames_bytes)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        append_frames, encode_frame, parse_resize, read_frames, reset_log, resize_payload, Frame,
        FrameKind,
    };

    fn round_trip(frames: &[Frame]) -> (u32, Vec<Frame>) {
        let mut bytes = super::encode_header(0);
        for frame in frames {
            bytes.extend_from_slice(&encode_frame(frame.kind, &frame.payload));
        }
        read_frames(&bytes).expect("valid log")
    }

    #[test]
    fn round_trips_output_and_resize_frames() {
        let frames = vec![
            Frame {
                kind: FrameKind::Output,
                payload: b"hola\r\n".to_vec(),
            },
            Frame {
                kind: FrameKind::Resize,
                payload: resize_payload(120, 40),
            },
            Frame {
                kind: FrameKind::Output,
                payload: b"chau\r\n".to_vec(),
            },
        ];
        let (gen, parsed) = round_trip(&frames);
        assert_eq!(gen, 0);
        assert_eq!(parsed, frames);
        assert_eq!(parse_resize(&parsed[1].payload), Some((120, 40)));
    }

    #[test]
    fn truncated_tail_is_dropped_but_complete_frames_survive() {
        let mut bytes = super::encode_header(7);
        bytes.extend_from_slice(&encode_frame(FrameKind::Output, b"completo".as_slice()));
        // Frame truncado a mitad: anunciamos 100 bytes pero damos 3.
        bytes.push(FrameKind::Output as u8);
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");

        let (gen, parsed) = read_frames(&bytes).expect("header válido");
        assert_eq!(gen, 7);
        assert_eq!(parsed.len(), 1, "solo el frame completo");
        assert_eq!(parsed[0].payload, b"completo".to_vec());
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = super::encode_header(0);
        bytes[0] = b'X';
        assert!(read_frames(&bytes).is_none());
    }

    #[test]
    fn bad_version_is_rejected() {
        let mut bytes = super::encode_header(0);
        bytes[4] = 99;
        assert!(read_frames(&bytes).is_none());
    }

    #[test]
    fn empty_file_is_rejected() {
        assert!(read_frames(&[]).is_none());
    }

    #[test]
    fn append_creates_header_when_missing_and_appends_after() {
        let dir = std::env::temp_dir().join(format!("mtlg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("panel.mtlg");

        append_frames(&path, &encode_frame(FrameKind::Output, b"uno".as_slice())).unwrap();
        append_frames(&path, &encode_frame(FrameKind::Output, b"dos".as_slice())).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let (gen, parsed) = read_frames(&bytes).unwrap();
        assert_eq!(gen, 0);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].payload, b"dos".to_vec());

        // Reset sube la generation y vacía los frames.
        reset_log(&path, 1).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let (gen, parsed) = read_frames(&bytes).unwrap();
        assert_eq!(gen, 1);
        assert!(parsed.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
