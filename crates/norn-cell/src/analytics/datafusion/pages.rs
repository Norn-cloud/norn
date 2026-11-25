//! NB1 (#1765): a hard byte bound on every decoded Parquet batch. Before a
//! row group is decoded, the page headers of its projected column chunks are
//! read from the file itself (the Thrift compact `PageHeader` before each
//! page; nothing else of the page is read or allocated), so the bound comes
//! from the pages the reader will decompress, not from the footer's
//! averages. A page (dictionary or data) whose stated size is past the bound
//! refuses before the reader would allocate it, and so does a row group
//! whose pages do not hold the rows its footer claims. The rows per batch
//! are then the most (up to [`super::files::CHUNK_ROWS`]) for which any run
//! of that many rows decodes within the bound: per column, its dictionary,
//! every data page the run's rows touch, and a view per value. A skewed row group (one giant row among tiny ones), a
//! huge dictionary page and a footer that understates its sizes all meet
//! the same bound.

use std::fs::File;
use std::io::{BufReader, Read, Seek as _, SeekFrom};

use parquet::arrow::arrow_reader::ArrowReaderMetadata;

use super::files::{CHUNK_ROWS, Meter};
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// The most pages one row group's projected column chunks may hold.
const MAX_GROUP_PAGES: usize = 1 << 20;
/// The deepest Thrift nesting a page header may use.
const MAX_DEPTH: u32 = 8;
/// The bytes a decoded value adds besides its page bytes (a string view).
const VALUE_OVERHEAD: u64 = 16;

/// A page header's sizes and row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageHeader {
    kind: i32,
    uncompressed: i64,
    compressed: i64,
    /// Data page rows (`num_values` of a v1 page, `num_rows` of a v2 page).
    rows: Option<i64>,
}

const DATA_PAGE: i32 = 0;
const INDEX_PAGE: i32 = 1;
const DICTIONARY_PAGE: i32 = 2;
const DATA_PAGE_V2: i32 = 3;

fn malformed() -> super::super::EngineRefusal {
    engine_error("a Parquet page header is malformed")
}

/// A reader of the Thrift compact protocol over a byte budget.
struct Thrift<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Thrift<R> {
    fn byte(&mut self) -> Outcome<u8> {
        self.left = self.left.checked_sub(1).ok_or_else(malformed)?;
        let mut one = [0_u8; 1];
        self.inner
            .read_exact(&mut one)
            .map_err(|_short| malformed())?;
        let [byte] = one;
        Ok(byte)
    }

    fn skip(&mut self, bytes: u64) -> Outcome<()> {
        self.left = self.left.checked_sub(bytes).ok_or_else(malformed)?;
        let copied = std::io::copy(&mut (&mut self.inner).take(bytes), &mut std::io::sink())
            .map_err(|_short| malformed())?;
        if copied == bytes {
            Ok(())
        } else {
            Err(malformed())
        }
    }

    fn varint(&mut self) -> Outcome<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f)
                .checked_shl(shift)
                .ok_or_else(malformed)?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(malformed())
    }

    fn zigzag(&mut self) -> Outcome<i64> {
        let raw = self.varint()?;
        let magnitude = i64::try_from(raw.wrapping_shr(1)).map_err(|_wide| malformed())?;
        // Zigzag: an odd raw value is the negative `-magnitude - 1`.
        Ok(if raw & 1 == 0 { magnitude } else { !magnitude })
    }

    /// Skip one value of compact type `kind`.
    fn skip_value(&mut self, kind: u8, depth: u32) -> Outcome<()> {
        let depth = depth
            .checked_add(1)
            .filter(|depth| *depth <= MAX_DEPTH)
            .ok_or_else(malformed)?;
        match kind {
            1 | 2 => Ok(()),
            3 => self.byte().map(drop),
            4..=6 => self.varint().map(drop),
            7 => self.skip(8),
            8 => {
                let length = self.varint()?;
                self.skip(length)
            }
            9 | 10 => {
                let header = self.byte()?;
                let count = match header.wrapping_shr(4) {
                    15 => self.varint()?,
                    small => u64::from(small),
                };
                self.skip_items(count, header & 0x0f, depth)
            }
            11 => {
                let count = self.varint()?;
                if count == 0 {
                    return Ok(());
                }
                let kinds = self.byte()?;
                for _ in 0..count {
                    self.skip_value(kinds.wrapping_shr(4), depth)?;
                    self.skip_value(kinds & 0x0f, depth)?;
                }
                Ok(())
            }
            12 => self.walk(depth, &mut |thrift, _, kind, depth| {
                thrift.skip_value(kind, depth)
            }),
            _ => Err(malformed()),
        }
    }

    /// Skip `count` list items of compact type `kind`; a bool item is a byte.
    fn skip_items(&mut self, count: u64, kind: u8, depth: u32) -> Outcome<()> {
        if count > self.left {
            return Err(malformed());
        }
        for _ in 0..count {
            if matches!(kind, 1 | 2) {
                self.byte()?;
            } else {
                self.skip_value(kind, depth)?;
            }
        }
        Ok(())
    }

    /// Walk one struct's fields, handing each (id, kind) to `field`, which
    /// must consume its value.
    fn walk(
        &mut self,
        depth: u32,
        field: &mut dyn FnMut(&mut Self, i64, u8, u32) -> Outcome<()>,
    ) -> Outcome<()> {
        let mut last = 0_i64;
        loop {
            let header = self.byte()?;
            if header == 0 {
                return Ok(());
            }
            let kind = header & 0x0f;
            let id = match header.wrapping_shr(4) {
                0 => self.zigzag()?,
                delta => last.checked_add(i64::from(delta)).ok_or_else(malformed)?,
            };
            last = id;
            field(self, id, kind, depth)?;
        }
    }

    fn i32_field(&mut self, kind: u8) -> Outcome<i64> {
        if kind != 5 {
            return Err(malformed());
        }
        self.zigzag()
    }

    /// One `PageHeader`: its type, sizes and data rows.
    fn page_header(&mut self) -> Outcome<PageHeader> {
        let mut header = PageHeader {
            kind: -1,
            uncompressed: -1,
            compressed: -1,
            rows: None,
        };
        let mut v1_rows = None;
        let mut v2_rows = None;
        self.walk(0, &mut |thrift, id, kind, depth| match id {
            1 => {
                header.kind = i32::try_from(thrift.i32_field(kind)?).map_err(|_e| malformed())?;
                Ok(())
            }
            2 => {
                header.uncompressed = thrift.i32_field(kind)?;
                Ok(())
            }
            3 => {
                header.compressed = thrift.i32_field(kind)?;
                Ok(())
            }
            // DataPageHeader.num_values (field 5), DataPageHeaderV2.num_rows (field 8).
            5 | 8 if kind == 12 => {
                let wanted = if id == 5 { 1 } else { 3 };
                let target = if id == 5 { &mut v1_rows } else { &mut v2_rows };
                thrift.walk(depth, &mut |inner, field, kind, depth| {
                    if field == wanted {
                        *target = Some(inner.i32_field(kind)?);
                        Ok(())
                    } else {
                        inner.skip_value(kind, depth)
                    }
                })
            }
            _ => thrift.skip_value(kind, depth),
        })?;
        header.rows = match header.kind {
            DATA_PAGE => v1_rows,
            DATA_PAGE_V2 => v2_rows,
            _ => None,
        };
        if header.uncompressed < 0 || header.compressed < 0 || header.rows.is_some_and(|r| r < 0) {
            return Err(malformed());
        }
        Ok(header)
    }
}

/// One column chunk's pages: its dictionary bytes and each data page's
/// (rows, bytes), in order.
#[derive(Debug, Default)]
struct Chunk {
    dictionary: u64,
    pages: Vec<(u64, u64)>,
}

/// The pages of the column chunk at `start`, `length` bytes long.
fn chunk_pages(
    file: &File,
    (start, length): (u64, u64),
    bound: u64,
    budget: &mut usize,
) -> Outcome<Chunk> {
    let size = file
        .metadata()
        .map_err(|e| engine_error(e.to_string()))?
        .len();
    if start.checked_add(length).is_none_or(|end| end > size) {
        return Err(engine_error("a Parquet column chunk lies past the file"));
    }
    let mut source = file.try_clone().map_err(|e| engine_error(e.to_string()))?;
    source
        .seek(SeekFrom::Start(start))
        .map_err(|e| engine_error(e.to_string()))?;
    let mut thrift = Thrift {
        inner: BufReader::new(source),
        left: length,
    };
    let mut chunk = Chunk::default();
    // A data page of no rows is decoded on the way to the next one: its
    // bytes are counted with that page (or the last, at the chunk's end).
    let mut carried = 0_u64;
    while thrift.left > 0 {
        *budget = budget.checked_sub(1).ok_or_else(|| {
            refusal(
                "bulk.unsupported_encoding",
                "the file declares too much structure",
            )
        })?;
        let header = thrift.page_header()?;
        let bytes = u64::try_from(header.uncompressed).map_err(|_e| malformed())?;
        if bytes > bound {
            return Err(refusal(
                EXHAUSTED,
                "a Parquet page is larger than the load's bound",
            ));
        }
        match header.kind {
            DICTIONARY_PAGE => chunk.dictionary = chunk.dictionary.saturating_add(bytes),
            DATA_PAGE | DATA_PAGE_V2 => {
                let rows =
                    u64::try_from(header.rows.ok_or_else(malformed)?).map_err(|_e| malformed())?;
                if rows == 0 {
                    carried = carried.saturating_add(bytes);
                } else {
                    chunk.pages.push((rows, bytes.saturating_add(carried)));
                    carried = 0;
                }
            }
            INDEX_PAGE => {}
            _ => return Err(malformed()),
        }
        thrift.skip(u64::try_from(header.compressed).map_err(|_e| malformed())?)?;
    }
    match chunk.pages.last_mut() {
        Some((_, bytes)) => *bytes = bytes.saturating_add(carried),
        None => chunk.dictionary = chunk.dictionary.saturating_add(carried),
    }
    Ok(chunk)
}

/// The most page bytes any `rows` consecutive rows of a chunk's data
/// `pages` (each of at least one row) touch: the run that starts on a
/// page's last row reaches furthest, so each page is tried as the first.
fn widest_window(pages: &[(u64, u64)], rows: u64) -> u64 {
    let rows = rows.max(1);
    let (mut widest, mut start) = (0_u64, 0_u64);
    let (mut next, mut next_start, mut sum) = (0_usize, 0_u64, 0_u64);
    for (index, (page_rows, page_bytes)) in pages.iter().enumerate() {
        if next <= index {
            (next, next_start, sum) = (index, start, 0);
        }
        let end = start.saturating_add(*page_rows);
        let limit = end.saturating_sub(1).saturating_add(rows);
        while let Some((more_rows, more_bytes)) = pages.get(next)
            && next_start < limit
        {
            sum = sum.saturating_add(*more_bytes);
            next_start = next_start.saturating_add(*more_rows);
            next = next.saturating_add(1);
        }
        widest = widest.max(sum);
        sum = sum.saturating_sub(*page_bytes);
        start = end;
    }
    widest
}

/// The most bytes any run of `rows` rows decodes, over every chunk.
fn widest_batch(chunks: &[Chunk], rows: u64) -> u64 {
    chunks.iter().fold(0_u64, |total, chunk| {
        total
            .saturating_add(chunk.dictionary)
            .saturating_add(widest_window(&chunk.pages, rows))
            .saturating_add(rows.saturating_mul(VALUE_OVERHEAD))
    })
}

/// NB1: the rows per batch for row group `group` of the projected leaves
/// `leaves`, so that every batch decodes within half a chunk; refuses a
/// page or a single row past that bound, and a group whose pages do not
/// hold its rows.
pub(super) fn group_rows(
    file: &File,
    metadata: &ArrowReaderMetadata,
    group: usize,
    leaves: &[usize],
    meter: &Meter,
) -> Outcome<usize> {
    let row_group = metadata
        .metadata()
        .row_groups()
        .get(group)
        .ok_or_else(|| engine_error("a Parquet row group"))?;
    let group_rows = u64::try_from(row_group.num_rows()).map_err(|_e| malformed())?;
    let bound = u64::try_from(meter.caps.chunk.div_euclid(2).max(1)).unwrap_or(u64::MAX);
    let mut budget = MAX_GROUP_PAGES;
    let mut chunks = Vec::with_capacity(leaves.len());
    for leaf in leaves {
        meter.deadline()?;
        let column = row_group
            .columns()
            .get(*leaf)
            .ok_or_else(|| engine_error("a Parquet column chunk"))?;
        let chunk = chunk_pages(file, column.byte_range(), bound, &mut budget)?;
        let held = chunk
            .pages
            .iter()
            .fold(0_u64, |total, (rows, _)| total.saturating_add(*rows));
        if held != group_rows {
            return Err(engine_error(
                "a Parquet row group's pages do not hold the rows its footer claims",
            ));
        }
        chunks.push(chunk);
    }
    let mut rows = u64::try_from(CHUNK_ROWS).unwrap_or(1);
    while widest_batch(&chunks, rows) > bound {
        if rows == 1 {
            return Err(refusal(
                EXHAUSTED,
                "a Parquet row is larger than the load's bound",
            ));
        }
        rows = rows.div_euclid(2).max(1);
    }
    Ok(usize::try_from(rows).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(dictionary: u64, pages: &[(u64, u64)]) -> Chunk {
        Chunk {
            dictionary,
            pages: pages.to_vec(),
        }
    }

    /// A run pays for every page its rows can touch, and for its column's
    /// dictionary and a view per value.
    #[test]
    fn a_run_pays_for_every_page_it_touches() {
        let tiny_and_giant = [(10, 100), (1, 5_000), (10, 100)];
        assert_eq!(widest_window(&tiny_and_giant, 21), 5_200);
        // Ten rows from the first page's last row reach past the giant one.
        assert_eq!(widest_window(&tiny_and_giant, 10), 5_200);
        assert_eq!(widest_window(&tiny_and_giant, 2), 5_100);
        assert_eq!(widest_window(&tiny_and_giant, 1), 5_000);
        assert_eq!(widest_window(&[(4, 10), (4, 10), (4, 10)], 5), 20);
        assert_eq!(widest_window(&[], 5), 0);
        let chunks = [chunk(700, &[(4, 10), (4, 10)]), chunk(0, &[(8, 64)])];
        assert_eq!(widest_batch(&chunks, 4), 700 + 20 + 64 + 2 * 4 * 16);
    }

    /// A v1 data page header (Thrift compact) is read for its sizes and rows,
    /// with statistics and other fields skipped.
    #[test]
    fn a_page_header_is_read_for_its_sizes_and_rows() {
        // type=0 (i32 0), uncompressed=300 (zigzag 600), compressed=150 (300),
        // data_page_header { num_values=7, encoding=0, stats { max = "ab" } }.
        let bytes: Vec<u8> = vec![
            0x15, 0x00, // field 1, i32: 0
            0x15, 0xd8, 0x04, // field 2, i32: 300
            0x15, 0xac, 0x02, // field 3, i32: 150
            0x2c, // field 5 (delta 2), struct
            0x15, 0x0e, // num_values: 7
            0x15, 0x00, // encoding
            0x3c, // field 5 (delta 3), struct: statistics
            0x18, 0x02, b'a', b'b', // max: binary "ab"
            0x00, // end statistics
            0x00, // end data page header
            0x00, // end page header
        ];
        let length = u64::try_from(bytes.len()).unwrap_or(0);
        let mut thrift = Thrift {
            inner: bytes.as_slice(),
            left: length,
        };
        assert_eq!(
            thrift.page_header().ok(),
            Some(PageHeader {
                kind: DATA_PAGE,
                uncompressed: 300,
                compressed: 150,
                rows: Some(7),
            })
        );
        assert_eq!(thrift.left, 0);
        // Cut short, or nested past the depth bound: malformed, never a panic.
        for cut in 0..bytes.len() {
            let mut short = Thrift {
                inner: bytes.get(..cut).unwrap_or_default(),
                left: length,
            };
            assert!(short.page_header().is_err(), "{cut}");
        }
        // Field 4 (`crc`) as a struct, nested past the depth bound.
        let deep: Vec<u8> = std::iter::repeat_n(0x4c_u8, 20).collect();
        let mut nested = Thrift {
            inner: deep.as_slice(),
            left: 20,
        };
        assert!(nested.page_header().is_err());
    }
}
