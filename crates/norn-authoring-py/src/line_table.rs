//! Byte-offset to `(1-based line, 0-based column)` mapping for Python sources.

use ruff_text_size::TextSize;

use crate::error::SourceSpan;

/// Precomputed line-start byte offsets for deterministic span mapping.
#[derive(Clone, Debug)]
pub(crate) struct LineTable<'a> {
    filename: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LineTable<'a> {
    /// Builds a [`LineTable`] in one linear pass over `source`.
    pub(crate) fn new(filename: &'a str, source: &str) -> Self {
        let mut line_starts = vec![0usize];
        let bytes = source.as_bytes();
        let mut idx = 0usize;
        while let Some(&byte) = bytes.get(idx) {
            if byte == b'\n' {
                if let Some(next) = idx.checked_add(1) {
                    line_starts.push(next);
                }
            } else if byte == b'\r' {
                let next_is_lf = idx
                    .checked_add(1)
                    .and_then(|next| bytes.get(next))
                    .is_some_and(|&next_byte| next_byte == b'\n');
                if !next_is_lf && let Some(next) = idx.checked_add(1) {
                    line_starts.push(next);
                }
            }
            idx = idx.saturating_add(1);
        }
        Self {
            filename,
            line_starts,
        }
    }

    /// Converts a UTF-8 byte offset into a [`SourceSpan`] with 1-based line and 0-based column.
    pub(crate) fn span_at(&self, offset: TextSize) -> SourceSpan {
        let byte_offset = offset.to_usize();
        let line_idx = match self.line_starts.binary_search(&byte_offset) {
            Ok(exact) => exact,
            Err(insert_pos) => insert_pos.saturating_sub(1),
        };
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0);
        let line = line_idx.saturating_add(1);
        let column = byte_offset.saturating_sub(line_start);
        SourceSpan::new(self.filename, line, column)
    }

    /// Returns a span anchored at the start of the file (`line 1, column 0`).
    pub(crate) fn file_start(&self) -> SourceSpan {
        SourceSpan::new(self.filename, 1, 0)
    }
}
