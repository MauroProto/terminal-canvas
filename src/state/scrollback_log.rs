//! Log incremental del scrollback por panel: en vez de reescribir el
//! checkpoint completo cada autosave (2 s), se appendean frames pequeños
//! (output/resize/clear). El checkpoint completo solo se reescribe cuando el
//! log supera 1 MB, en un cierre limpio, o al cerrar un panel.
//!
//! Formato binario:
//!   header: `MTLG` + u8 versión + u32 generation (LE)
//!   frame:  u64 seq + u8 kind (1=output, 2=resize, 3=clear) +
//!           u32 len (LE) + payload
//!
//! `read_frames` tolera una cola truncada (crash a mitad de frame): devuelve
//! solo los frames completos y reporta la `generation` del header.

use std::path::Path;

pub const MAGIC: &[u8; 4] = b"MTLG";
pub const VERSION: u8 = 2;

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
    pub seq: u64,
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

/// Frame: seq + kind + len + payload.
pub fn encode_frame(seq: u64, kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + payload.len());
    out.extend_from_slice(&seq.to_le_bytes());
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

/// Lee el prefijo válido del log y devuelve también su longitud exacta.
///
/// Una cola truncada (crash a mitad de frame) se ignora al restaurar, pero el
/// writer necesita esta longitud para truncarla físicamente antes de volver a
/// appendear. De otro modo, los frames nuevos quedarían detrás de bytes
/// corruptos y serían inalcanzables en el siguiente restore.
pub fn read_frames_with_valid_len(bytes: &[u8]) -> Option<(u32, Vec<Frame>, usize)> {
    if bytes.len() < 9 || &bytes[0..4] != MAGIC || bytes[4] != VERSION {
        return None;
    }
    let generation = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    let mut frames = Vec::new();
    let mut cursor = 9usize;
    let mut previous_seq: Option<u64> = None;
    while cursor + 13 <= bytes.len() {
        let seq = u64::from_le_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]);
        if previous_seq.is_some_and(|previous| previous.checked_add(1) != Some(seq)) {
            return None;
        }
        let kind = match FrameKind::from_u8(bytes[cursor + 8]) {
            Some(kind) => kind,
            None => break, // byte corrupto: cortamos acá
        };
        let len = u32::from_le_bytes([
            bytes[cursor + 9],
            bytes[cursor + 10],
            bytes[cursor + 11],
            bytes[cursor + 12],
        ]) as usize;
        let end = cursor + 13 + len;
        if end > bytes.len() {
            break; // frame truncado a mitad: descartamos la cola
        }
        frames.push(Frame {
            seq,
            kind,
            payload: bytes[cursor + 13..end].to_vec(),
        });
        previous_seq = Some(seq);
        cursor = end;
    }
    Some((generation, frames, cursor))
}

/// Lee el log completo. Devuelve `None` si el magic/versión no coinciden o si
/// hay un gap de secuencia. Una cola truncada se descarta en memoria.
pub fn read_frames(bytes: &[u8]) -> Option<(u32, Vec<Frame>)> {
    let (generation, frames, _) = read_frames_with_valid_len(bytes)?;
    Some((generation, frames))
}

/// Reasigna una secuencia durable a un lote producido en RAM. El contador del
/// PTY puede reiniciarse al abrir la app; la continuidad que detecta pérdidas
/// pertenece al archivo y se decide en su único writer.
pub fn renumber_frames(bytes: &[u8], first_seq: u64) -> Option<(Vec<u8>, u64)> {
    let mut framed = encode_header(0);
    framed.extend_from_slice(bytes);
    let (_, frames) = read_frames(&framed)?;
    let mut next_seq = first_seq;
    let mut out = Vec::with_capacity(bytes.len());
    for frame in frames {
        out.extend_from_slice(&encode_frame(next_seq, frame.kind, &frame.payload));
        next_seq = next_seq.checked_add(1)?;
    }
    Some((out, next_seq))
}

/// Crea un log nuevo (o lo resetea) con la generation dada.
pub fn reset_log(path: &Path, generation: u32) -> std::io::Result<()> {
    crate::state::durable_write::write_atomic(path, &encode_header(generation)).map(|_| ())
}

/// Appendea bytes de frames al log. Si el archivo no existe, lo crea con
/// header de generation 0 primero.
pub fn append_frames(path: &Path, frames_bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    if !path.exists() {
        crate::state::durable_write::write_atomic(path, &encode_header(0))?;
    }
    let bytes = std::fs::read(path)?;
    let Some((_, _, valid_len)) = read_frames_with_valid_len(&bytes) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "log incremental inválido",
        ));
    };
    // Windows append-only access cannot truncate a torn frame. The single
    // durable writer repairs it with write access, then seeks to the new EOF.
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    if valid_len < bytes.len() {
        file.set_len(valid_len as u64)?;
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(frames_bytes)?;
    // El autosave afirma durabilidad frente a crash/power loss, no sólo que
    // los bytes llegaron al page cache del proceso.
    file.sync_data()
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
            bytes.extend_from_slice(&encode_frame(frame.seq, frame.kind, &frame.payload));
        }
        read_frames(&bytes).expect("valid log")
    }

    #[test]
    fn round_trips_output_and_resize_frames() {
        let frames = vec![
            Frame {
                seq: 10,
                kind: FrameKind::Output,
                payload: b"hola\r\n".to_vec(),
            },
            Frame {
                seq: 11,
                kind: FrameKind::Resize,
                payload: resize_payload(120, 40),
            },
            Frame {
                seq: 12,
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
        bytes.extend_from_slice(&encode_frame(1, FrameKind::Output, b"completo".as_slice()));
        // Frame truncado a mitad: anunciamos 100 bytes pero damos 3.
        bytes.extend_from_slice(&2u64.to_le_bytes());
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

        append_frames(
            &path,
            &encode_frame(1, FrameKind::Output, b"uno".as_slice()),
        )
        .unwrap();
        append_frames(
            &path,
            &encode_frame(2, FrameKind::Output, b"dos".as_slice()),
        )
        .unwrap();

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

    #[test]
    fn append_repairs_a_truncated_tail_before_writing_new_frames() {
        let dir = std::env::temp_dir().join(format!("mtlg-repair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("panel.mtlg");
        let mut bytes = super::encode_header(4);
        bytes.extend_from_slice(&encode_frame(1, FrameKind::Output, b"uno"));
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.push(FrameKind::Output as u8);
        bytes.extend_from_slice(&100u32.to_le_bytes());
        bytes.extend_from_slice(b"rota");
        std::fs::write(&path, bytes).unwrap();

        append_frames(&path, &encode_frame(2, FrameKind::Output, b"dos")).unwrap();

        let (generation, frames) = read_frames(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(generation, 4);
        assert_eq!(
            frames
                .iter()
                .map(|frame| (frame.seq, frame.payload.as_slice()))
                .collect::<Vec<_>>(),
            vec![(1, b"uno".as_slice()), (2, b"dos".as_slice())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sequence_gap_invalidates_the_log() {
        let mut bytes = super::encode_header(0);
        bytes.extend_from_slice(&encode_frame(10, FrameKind::Output, b"uno"));
        bytes.extend_from_slice(&encode_frame(12, FrameKind::Output, b"tres"));
        assert!(read_frames(&bytes).is_none());
    }

    #[test]
    fn a_sequence_after_u64_max_is_rejected_without_panicking() {
        let mut bytes = super::encode_header(0);
        bytes.extend_from_slice(&encode_frame(u64::MAX, FrameKind::Output, b"last"));
        bytes.extend_from_slice(&encode_frame(0, FrameKind::Output, b"invalid"));
        assert!(read_frames(&bytes).is_none());
    }

    #[test]
    fn renumbering_makes_a_new_process_continue_the_durable_sequence() {
        let mut fresh_process = encode_frame(1, FrameKind::Output, b"uno");
        fresh_process.extend_from_slice(&encode_frame(2, FrameKind::Output, b"dos"));
        let (renumbered, next) = super::renumber_frames(&fresh_process, 41).unwrap();
        let mut log = super::encode_header(0);
        log.extend_from_slice(&renumbered);
        let (_, frames) = read_frames(&log).unwrap();
        assert_eq!(
            frames.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
            vec![41, 42]
        );
        assert_eq!(next, 43);
    }
}
