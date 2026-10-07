//! Modelo de fuente original y fragmentos del visor, preparado fuera de la UI.
//! Sin I/O ni layouts: un Arc<str>, rangos lógicos y filas UTF-8 acotadas.

use std::ops::Range;
use std::sync::Arc;

use unicode_segmentation::UnicodeSegmentation;

/// The maximum UTF-8 byte length handed to a visual row's text layout.
pub const FRAGMENT_BYTE_CAP: usize = 1024;

/// A real source line; its terminator never becomes part of a visual row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalLine {
    /// Original bytes, excluding LF or CRLF. A bare CR at EOF is content.
    pub content: Range<usize>,
    /// Original LF/CRLF bytes, or an empty range for an unterminated line.
    pub ending: Range<usize>,
    /// Indices into `SourceDocument::fragments`, including one empty row when
    /// this is an empty logical line.
    pub fragments: Range<usize>,
}

/// A display row referring directly to the immutable original source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualFragment {
    pub logical_line: usize,
    pub source: Range<usize>,
    /// Only true when a single extended grapheme exceeds the hard byte cap.
    pub starts_inside_grapheme: bool,
    pub ends_inside_grapheme: bool,
}

/// Store these original-source offsets across scrolling and visual rows.
/// Reversing a drag does not change the text copied from the normalized range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSelection {
    pub anchor: usize,
    pub caret: usize,
}

impl SourceSelection {
    pub fn normalized(self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetError {
    UnknownFragment,
    InvalidScalarCursor,
    InvalidSourceOffset,
}

/// Prepare this entire value in the reader worker, then publish it in the
/// reader's existing generation-token result slot. The UI only borrows slices.
pub struct SourceDocument {
    source: Arc<str>,
    lines: Vec<LogicalLine>,
    fragments: Vec<VisualFragment>,
    has_long_lines: bool,
}

impl SourceDocument {
    #[cfg(test)]
    pub fn prepare(source: Arc<str>) -> Self {
        Self::prepare_cancellable(source, &|| false).expect("uncancellable preparation")
    }

    /// Cooperative cancellation, never a partially prepared published result.
    /// Input must already satisfy the reader's source byte/line limits.
    /// Cancellation is checked between lines, graphemes and scalar chunks.
    pub fn prepare_cancellable(source: Arc<str>, cancelled: &dyn Fn() -> bool) -> Option<Self> {
        if cancelled() {
            return None;
        }
        let mut lines = Vec::new();
        let mut fragments = Vec::new();
        let mut has_long_lines = false;
        let mut start = 0;
        for raw_line in source.split_inclusive('\n') {
            if cancelled() {
                return None;
            }
            let end = start + raw_line.len();
            let content_end = if raw_line.ends_with("\r\n") {
                end - 2
            } else if raw_line.ends_with('\n') {
                end - 1
            } else {
                end
            };
            let first_fragment = fragments.len();
            has_long_lines |= content_end - start > FRAGMENT_BYTE_CAP;
            fragment_line(
                &source,
                start..content_end,
                lines.len(),
                &mut fragments,
                cancelled,
            )?;
            lines.push(LogicalLine {
                content: start..content_end,
                ending: content_end..end,
                fragments: first_fragment..fragments.len(),
            });
            start = end;
        }
        if cancelled() {
            return None;
        }
        Some(Self {
            source,
            lines,
            fragments,
            has_long_lines,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn source_arc(&self) -> Arc<str> {
        Arc::clone(&self.source)
    }

    pub fn logical_lines(&self) -> &[LogicalLine] {
        &self.lines
    }

    /// Contenido de una línea lógica, sin LF/CRLF; no reconstruye filas.
    pub fn logical_line_text(&self, index: usize) -> Option<&str> {
        self.lines
            .get(index)
            .map(|line| &self.source[line.content.clone()])
    }

    /// Precalculado en el worker: no recorre las líneas en cada frame.
    pub fn has_long_lines(&self) -> bool {
        self.has_long_lines
    }

    pub fn fragments(&self) -> &[VisualFragment] {
        &self.fragments
    }

    /// This is the only text handed to a fragment's galley: at most 1024 bytes.
    pub fn fragment_text(&self, index: usize) -> Option<&str> {
        self.fragments
            .get(index)
            .map(|fragment| &self.source[fragment.source.clone()])
    }

    pub fn select_all(&self) -> SourceSelection {
        SourceSelection {
            anchor: 0,
            caret: self.source.len(),
        }
    }

    /// Copy from the original source once, never by joining display rows.
    /// This preserves actual CRLF/LF, EOF and pathological Unicode clusters.
    pub fn selection_text(&self, selection: SourceSelection) -> Result<&str, OffsetError> {
        self.valid_selection(selection)
            .map(|range| &self.source[range])
    }

    /// Highlight only a visible fragment's intersection with the selection.
    /// A selected newline may have no visible fragment bytes; it still remains
    /// in `selection_text`, including the final terminator in Ctrl+A.
    pub fn fragment_selection_text(
        &self,
        index: usize,
        selection: SourceSelection,
    ) -> Result<&str, OffsetError> {
        let range = self.valid_selection(selection)?;
        let fragment = self
            .fragments
            .get(index)
            .ok_or(OffsetError::UnknownFragment)?;
        let start = range.start.max(fragment.source.start);
        let end = range.end.min(fragment.source.end);
        if end <= start {
            Ok(&self.source[fragment.source.start..fragment.source.start])
        } else {
            Ok(&self.source[start..end])
        }
    }

    /// Map the fragment galley's Unicode scalar cursor to an original byte
    /// offset. egui's CharIndex counts scalars, not UTF-8 bytes or graphemes.
    pub fn fragment_cursor_to_source(
        &self,
        index: usize,
        scalar_cursor: usize,
    ) -> Result<usize, OffsetError> {
        let fragment = self
            .fragments
            .get(index)
            .ok_or(OffsetError::UnknownFragment)?;
        let text = &self.source[fragment.source.clone()];
        text.char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
            .nth(scalar_cursor)
            .map(|offset| fragment.source.start + offset)
            .ok_or(OffsetError::InvalidScalarCursor)
    }

    /// For repainting a stored source selection in a fragment's galley.
    /// This rejects a newline-only offset or a cursor outside this fragment.
    pub fn source_to_fragment_cursor(
        &self,
        index: usize,
        offset: usize,
    ) -> Result<usize, OffsetError> {
        let fragment = self
            .fragments
            .get(index)
            .ok_or(OffsetError::UnknownFragment)?;
        if offset < fragment.source.start
            || offset > fragment.source.end
            || !self.source.is_char_boundary(offset)
        {
            return Err(OffsetError::InvalidSourceOffset);
        }
        Ok(self.source[fragment.source.start..offset].chars().count())
    }

    fn valid_selection(&self, selection: SourceSelection) -> Result<Range<usize>, OffsetError> {
        let range = selection.normalized();
        if range.end > self.source.len()
            || !self.source.is_char_boundary(range.start)
            || !self.source.is_char_boundary(range.end)
        {
            Err(OffsetError::InvalidSourceOffset)
        } else {
            Ok(range)
        }
    }
}

fn fragment_line(
    source: &str,
    content: Range<usize>,
    logical_line: usize,
    fragments: &mut Vec<VisualFragment>,
    cancelled: &dyn Fn() -> bool,
) -> Option<()> {
    let mut fragment_start = content.start;
    for (relative_start, grapheme) in source[content.clone()].grapheme_indices(true) {
        if cancelled() {
            return None;
        }
        let start = content.start + relative_start;
        let end = start + grapheme.len();
        if grapheme.len() > FRAGMENT_BYTE_CAP {
            if fragment_start < start {
                push_fragment(fragments, logical_line, fragment_start..start, false, false);
            }
            let mut chunk_start = start;
            while chunk_start < end {
                if cancelled() {
                    return None;
                }
                let mut chunk_end = (chunk_start + FRAGMENT_BYTE_CAP).min(end);
                while !source.is_char_boundary(chunk_end) {
                    chunk_end -= 1;
                }
                // A scalar is at most four bytes and the cap is 1024.
                debug_assert!(chunk_end > chunk_start);
                push_fragment(
                    fragments,
                    logical_line,
                    chunk_start..chunk_end,
                    chunk_start != start,
                    chunk_end != end,
                );
                chunk_start = chunk_end;
            }
            fragment_start = end;
        } else if end - fragment_start > FRAGMENT_BYTE_CAP {
            push_fragment(fragments, logical_line, fragment_start..start, false, false);
            fragment_start = start;
        }
    }
    if fragment_start < content.end || content.is_empty() {
        push_fragment(
            fragments,
            logical_line,
            fragment_start..content.end,
            false,
            false,
        );
    }
    Some(())
}

fn push_fragment(
    fragments: &mut Vec<VisualFragment>,
    logical_line: usize,
    source: Range<usize>,
    starts_inside_grapheme: bool,
    ends_inside_grapheme: bool,
) {
    debug_assert!(source.len() <= FRAGMENT_BYTE_CAP);
    fragments.push(VisualFragment {
        logical_line,
        source,
        starts_inside_grapheme,
        ends_inside_grapheme,
    });
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::{OffsetError, SourceDocument, SourceSelection, FRAGMENT_BYTE_CAP};
    use unicode_segmentation::UnicodeSegmentation;

    fn prepare(text: &str) -> SourceDocument {
        SourceDocument::prepare(Arc::from(text))
    }

    fn assert_lossless(document: &SourceDocument) {
        let mut reconstructed = String::new();
        let mut source_end = 0;
        let mut fragment_end = 0;
        for (index, line) in document.logical_lines().iter().enumerate() {
            assert_eq!(line.content.start, source_end);
            assert_eq!(line.ending.start, line.content.end);
            assert_eq!(line.fragments.start, fragment_end);
            assert!(!line.fragments.is_empty());
            let mut content_end = line.content.start;
            for fragment in &document.fragments()[line.fragments.clone()] {
                assert_eq!(fragment.logical_line, index);
                assert_eq!(fragment.source.start, content_end);
                assert!(fragment.source.len() <= FRAGMENT_BYTE_CAP);
                assert!(document.source().is_char_boundary(fragment.source.start));
                assert!(document.source().is_char_boundary(fragment.source.end));
                assert!(line.content.is_empty() || !fragment.source.is_empty());
                reconstructed.push_str(&document.source()[fragment.source.clone()]);
                content_end = fragment.source.end;
            }
            assert_eq!(content_end, line.content.end);
            reconstructed.push_str(&document.source()[line.ending.clone()]);
            source_end = line.ending.end;
            fragment_end = line.fragments.end;
        }
        assert_eq!(source_end, document.source().len());
        assert_eq!(fragment_end, document.fragments().len());
        assert_eq!(reconstructed, document.source());
        assert_eq!(
            document.selection_text(document.select_all()).unwrap(),
            document.source()
        );
    }

    #[test]
    fn empty_source_has_no_phantom_line_or_fragment() {
        let document = prepare("");
        assert!(document.logical_lines().is_empty());
        assert!(document.fragments().is_empty());
        assert_eq!(document.selection_text(document.select_all()).unwrap(), "");
        assert_lossless(&document);
    }

    #[test]
    fn crlf_lf_empty_lines_and_eof_retain_exact_source_ranges() {
        let document = prepare("a\r\n\r\nb\nc\r");
        let lines = document.logical_lines();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].content, 0..1);
        assert_eq!(lines[0].ending, 1..3);
        assert_eq!(lines[1].content, 3..3);
        assert_eq!(lines[1].ending, 3..5);
        assert_eq!(lines[2].content, 5..6);
        assert_eq!(lines[2].ending, 6..7);
        assert_eq!(lines[3].content, 7..9);
        assert_eq!(lines[3].ending, 9..9);
        assert_eq!(document.fragment_text(1), Some(""));
        assert_eq!(document.fragment_text(3), Some("c\r"));
        assert_lossless(&document);
    }

    #[test]
    fn every_short_ascii_line_ending_sequence_matches_std_lines() {
        for length in 0..=6 {
            for mut number in 0..3_usize.pow(length) {
                let mut text = String::new();
                for _ in 0..length {
                    text.push(['a', '\r', '\n'][number % 3]);
                    number /= 3;
                }
                let document = prepare(&text);
                let actual = document
                    .logical_lines()
                    .iter()
                    .map(|line| &document.source()[line.content.clone()])
                    .collect::<Vec<_>>();
                assert_eq!(actual, text.lines().collect::<Vec<_>>(), "input {text:?}");
                assert_lossless(&document);
            }
        }
    }

    #[test]
    fn ascii_cap_edges_and_a_two_megabyte_single_line_are_bounded() {
        for length in [1023, 1024, 1025, 2048, 2 * 1024 * 1024] {
            let text = "a".repeat(length);
            let document = prepare(&text);
            assert_eq!(document.logical_lines().len(), 1);
            assert_eq!(document.logical_line_text(0), Some(text.as_str()));
            assert_eq!(document.has_long_lines(), length > FRAGMENT_BYTE_CAP);
            assert_eq!(
                document.fragments().len(),
                length.div_ceil(FRAGMENT_BYTE_CAP)
            );
            assert_lossless(&document);
        }
    }

    #[test]
    fn ordinary_combining_cluster_moves_whole_to_the_next_fragment() {
        let text = format!("{}e\u{301}fin", "a".repeat(1023));
        let document = prepare(&text);
        assert_eq!(document.fragment_text(0).unwrap().len(), 1023);
        assert_eq!(document.fragment_text(1), Some("e\u{301}fin"));
        assert!(document.fragments().iter().all(|fragment| {
            !fragment.starts_inside_grapheme && !fragment.ends_inside_grapheme
        }));
        assert_lossless(&document);
    }

    #[test]
    fn ordinary_emoji_zwj_and_regional_indicator_clusters_are_not_split() {
        for cluster in ["👩‍👩‍👧‍👦", "🇦🇷"] {
            assert_eq!(cluster.graphemes(true).count(), 1);
            let text = format!("{}{cluster}", "x".repeat(1023));
            let document = prepare(&text);
            assert_eq!(document.fragment_text(0).unwrap().len(), 1023);
            assert_eq!(document.fragment_text(1), Some(cluster));
            assert_lossless(&document);
        }
    }

    #[test]
    fn pathological_combining_cluster_respects_cap_and_remains_lossless() {
        let cluster = format!("a{}", "\u{301}".repeat(1300));
        assert_eq!(cluster.graphemes(true).count(), 1);
        let text = format!("XX{cluster}Z\r\n");
        let document = prepare(&text);
        assert_eq!(document.fragment_text(0), Some("XX"));
        assert!(document
            .fragments()
            .iter()
            .any(|fragment| fragment.ends_inside_grapheme));
        assert!(document
            .fragments()
            .iter()
            .any(|fragment| fragment.starts_inside_grapheme));
        assert_eq!(
            document.fragment_text(document.fragments().len() - 1),
            Some("Z")
        );
        assert_lossless(&document);
    }

    #[test]
    fn mixed_unicode_content_and_multibyte_cap_edges_are_utf8_safe() {
        let text = format!("{}\r\n{}", "中🙂é\t".repeat(600), "🦀".repeat(300));
        let document = prepare(&text);
        assert_eq!(document.logical_lines().len(), 2);
        assert_lossless(&document);
        for line in document.logical_lines() {
            let boundaries = document.source()[line.content.clone()]
                .grapheme_indices(true)
                .map(|(offset, _)| line.content.start + offset)
                .chain(std::iter::once(line.content.end))
                .collect::<Vec<_>>();
            for fragment in &document.fragments()[line.fragments.clone()] {
                assert!(boundaries.contains(&fragment.source.start));
                assert!(boundaries.contains(&fragment.source.end));
            }
        }
    }

    #[test]
    fn fragment_cursors_map_back_to_the_original_utf8_offsets() {
        let document = prepare(&format!("{}\r\nZ", "aé🙂e\u{301}".repeat(300)));
        for index in 0..document.fragments().len() {
            let fragment = &document.fragments()[index];
            let text = document.fragment_text(index).unwrap();
            for (scalar, byte) in text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .enumerate()
            {
                let expected = fragment.source.start + byte;
                assert_eq!(
                    document.fragment_cursor_to_source(index, scalar),
                    Ok(expected)
                );
                assert_eq!(
                    document.source_to_fragment_cursor(index, expected),
                    Ok(scalar)
                );
            }
            assert_eq!(
                document.fragment_cursor_to_source(index, text.chars().count() + 1),
                Err(OffsetError::InvalidScalarCursor)
            );
        }
    }

    #[test]
    fn a_selection_crossing_display_fragments_does_not_insert_newlines() {
        let text = format!("{}é🙂end", "x".repeat(1023));
        let document = prepare(&text);
        let selection = SourceSelection {
            anchor: document.fragment_cursor_to_source(0, 1022).unwrap(),
            caret: document.fragment_cursor_to_source(1, 2).unwrap(),
        };
        assert_eq!(document.selection_text(selection), Ok("xé🙂"));
        assert_eq!(document.fragment_selection_text(0, selection), Ok("x"));
        assert_eq!(document.fragment_selection_text(1, selection), Ok("é🙂"));
    }

    #[test]
    fn reverse_drag_across_logical_lines_preserves_original_crlf() {
        let document = prepare("aé\r\n🙂Z\n");
        let selection = SourceSelection {
            anchor: document.fragment_cursor_to_source(1, 1).unwrap(),
            caret: document.fragment_cursor_to_source(0, 1).unwrap(),
        };
        assert_eq!(document.selection_text(selection), Ok("é\r\n🙂"));
        assert_eq!(
            document.selection_text(document.select_all()),
            Ok("aé\r\n🙂Z\n")
        );
    }

    #[test]
    fn copying_only_line_endings_keeps_them_without_visible_fragment_text() {
        let document = prepare("a\r\nb\n");
        let selection = SourceSelection {
            anchor: 1,
            caret: 3,
        };
        assert_eq!(document.selection_text(selection), Ok("\r\n"));
        assert_eq!(document.fragment_selection_text(0, selection), Ok(""));
        assert_eq!(document.fragment_selection_text(1, selection), Ok(""));
        assert_eq!(
            document.source_to_fragment_cursor(0, 2),
            Err(OffsetError::InvalidSourceOffset)
        );
        assert_lossless(&document);
    }

    #[test]
    fn invalid_source_offsets_and_fragment_indices_are_rejected() {
        let document = prepare("é🙂");
        for selection in [
            SourceSelection {
                anchor: 1,
                caret: 2,
            },
            SourceSelection {
                anchor: 0,
                caret: 3,
            },
            SourceSelection {
                anchor: 0,
                caret: 7,
            },
        ] {
            assert_eq!(
                document.selection_text(selection),
                Err(OffsetError::InvalidSourceOffset)
            );
        }
        assert_eq!(
            document.fragment_cursor_to_source(1, 0),
            Err(OffsetError::UnknownFragment)
        );
        assert_eq!(
            document.source_to_fragment_cursor(0, 1),
            Err(OffsetError::InvalidSourceOffset)
        );
    }

    #[test]
    fn an_empty_visual_line_has_a_valid_zero_cursor_and_source_position() {
        let document = prepare("\r\n");
        assert_eq!(document.fragment_cursor_to_source(0, 0), Ok(0));
        assert_eq!(document.source_to_fragment_cursor(0, 0), Ok(0));
        assert_eq!(
            document.fragment_cursor_to_source(0, 1),
            Err(OffsetError::InvalidScalarCursor)
        );
        assert_eq!(document.selection_text(document.select_all()), Ok("\r\n"));
    }

    #[test]
    fn source_storage_is_shared_without_per_fragment_string_copies() {
        let source: Arc<str> = Arc::from("shared text\n");
        let document = SourceDocument::prepare(Arc::clone(&source));
        assert!(Arc::ptr_eq(&source, &document.source_arc()));
        assert_lossless(&document);
    }

    #[test]
    fn cancellation_never_returns_a_partial_document() {
        assert!(SourceDocument::prepare_cancellable(Arc::from(""), &|| true).is_none());
        let calls = Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() >= 12
        };
        assert!(
            SourceDocument::prepare_cancellable(Arc::from("x".repeat(10_000)), &cancelled)
                .is_none()
        );
        assert_eq!(calls.get(), 12);
        let document = prepare("next request\r\n");
        assert_lossless(&document);
    }
}
