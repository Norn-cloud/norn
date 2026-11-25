//! BULK-LOAD-1 on the adapter: CSV records as `DuckDB`'s strict reader splits
//! them, by one incremental state machine over the reader's buffer. Every
//! byte is looked at once (a quoted field spanning thousands of lines is
//! still linear), nothing is read past the bound a record or cell may reach,
//! and the deadline is checked at every buffer the reader fills.
//!
//! Fields split at commas; a field that starts with a quote is quoted (a
//! doubled quote is one quote, newlines are part of it, and anything but a
//! comma or the line's end after its closing quote refuses); a quote inside
//! an unquoted field is a literal character; a blank line is skipped; `\r\n`
//! ends a line as `\n` does.

use std::io::BufRead;

use super::super::EngineRefusal;
use super::files::Meter;
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// One record: its fields' bytes.
pub(super) struct Record {
    pub fields: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// At a field's first byte.
    Start,
    /// Inside an unquoted field.
    Unquoted,
    /// Inside a quoted field.
    Quoted,
    /// Just after a quote inside a quoted field.
    Quote,
    /// After a closing quote and a `\r`.
    Closed,
}

/// The record being read.
struct Parse<'m> {
    meter: &'m Meter,
    fields: Vec<Vec<u8>>,
    field: Vec<u8>,
    state: State,
    width: usize,
    content: bool,
    /// The most fields the record may have (B1): past it the record refuses
    /// (with `over`) before another field is allocated.
    max_fields: usize,
    over: &'m dyn Fn() -> EngineRefusal,
}

impl Parse<'_> {
    fn push(&mut self, byte: u8) -> Outcome<()> {
        if self.field.len() >= self.meter.caps.cell {
            return Err(refusal(EXHAUSTED, "a value is wider than the load's bound"));
        }
        self.field.push(byte);
        Ok(())
    }

    fn end_field(&mut self) -> Outcome<()> {
        if self.fields.len() >= self.max_fields {
            return Err((self.over)());
        }
        self.fields.push(std::mem::take(&mut self.field));
        self.state = State::Start;
        Ok(())
    }

    /// Take one byte; `true` when it ends the record's line.
    fn byte(&mut self, byte: u8) -> Outcome<bool> {
        self.width = self.width.saturating_add(1);
        if self.width > self.meter.caps.chunk {
            return Err(refusal(
                EXHAUSTED,
                "a CSV record is wider than the load's bound",
            ));
        }
        match (self.state, byte) {
            (State::Quoted, b'"') => self.state = State::Quote,
            (State::Quoted, other) => self.push(other)?,
            (State::Quote, b'"') => {
                self.push(b'"')?;
                self.state = State::Quoted;
            }
            (State::Quote, b',') => self.end_field()?,
            (State::Quote | State::Closed, b'\n') => return Ok(true),
            (State::Quote | State::Closed, b'\r') => self.state = State::Closed,
            (State::Quote | State::Closed, _) => {
                return Err(engine_error("the CSV has text after a closing quote"));
            }
            (State::Start, b'"') => {
                self.content = true;
                self.state = State::Quoted;
            }
            (State::Start | State::Unquoted, b',') => {
                self.content = true;
                self.end_field()?;
            }
            (State::Start | State::Unquoted, b'\n') => {
                if self.field.last() == Some(&b'\r') {
                    self.field.pop();
                }
                return Ok(true);
            }
            (State::Start | State::Unquoted, other) => {
                self.content |= other != b'\r';
                self.push(other)?;
                self.state = State::Unquoted;
            }
        }
        Ok(false)
    }

    fn finish(mut self) -> Outcome<Record> {
        self.end_field()?;
        Ok(Record {
            fields: self.fields,
        })
    }
}

/// The next record of at most `max_fields` fields, or `None` at the end of
/// the file.
pub(super) fn next_record(
    reader: &mut impl BufRead,
    meter: &Meter,
    max_fields: usize,
) -> Outcome<Option<Record>> {
    next_bounded(reader, meter, max_fields, &|| {
        engine_error("a CSV row has more fields than its header")
    })
}

/// NB5 (#1765): the header record, of at most `max_fields` fields: a header
/// of more is refused as a Parquet file declaring as many fields is.
pub(super) fn next_header(
    reader: &mut impl BufRead,
    meter: &Meter,
    max_fields: usize,
) -> Outcome<Option<Record>> {
    next_bounded(reader, meter, max_fields, &|| {
        refusal("bulk.unsupported_encoding", super::files::STRUCTURE)
    })
}

fn next_bounded(
    reader: &mut impl BufRead,
    meter: &Meter,
    max_fields: usize,
    over: &dyn Fn() -> EngineRefusal,
) -> Outcome<Option<Record>> {
    let mut parse = Parse {
        meter,
        max_fields,
        over,
        fields: Vec::new(),
        field: Vec::new(),
        state: State::Start,
        width: 0,
        content: false,
    };
    loop {
        meter.deadline()?;
        let buffer = reader
            .fill_buf()
            .map_err(|error| engine_error(error.to_string()))?;
        if buffer.is_empty() {
            return match parse.state {
                State::Quoted => Err(engine_error("the CSV ends inside a quoted field")),
                State::Start | State::Unquoted | State::Quote | State::Closed
                    if parse.content || !parse.field.is_empty() || !parse.fields.is_empty() =>
                {
                    parse.finish().map(Some)
                }
                State::Start | State::Unquoted | State::Quote | State::Closed => Ok(None),
            };
        }
        let mut used = 0_usize;
        let mut ended = false;
        for byte in buffer {
            used = used.saturating_add(1);
            if parse.byte(*byte)? {
                ended = true;
                break;
            }
        }
        reader.consume(used);
        if ended {
            if parse.content {
                return parse.finish().map(Some);
            }
            // A blank line: skipped.
            parse.fields.clear();
            parse.field.clear();
            parse.width = 0;
            parse.state = State::Start;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::super::files::{Caps, Meter};
    use super::next_record;

    fn caps(bytes: usize) -> Caps {
        Caps {
            cell: bytes,
            chunk: bytes,
            flush: bytes,
            total: u64::MAX,
            footer: 1 << 20,
            deadline: Duration::from_secs(60),
        }
    }

    /// A reader that counts the bytes it hands out.
    struct Counting<R> {
        inner: R,
        read: Rc<AtomicU64>,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.read
                .fetch_add(u64::try_from(read).unwrap_or(0), Ordering::SeqCst);
            Ok(read)
        }
    }

    fn records(text: &[u8]) -> Vec<Vec<Vec<u8>>> {
        let meter = Meter::new(caps(1 << 20));
        let mut reader = BufReader::new(text);
        let mut out = Vec::new();
        while let Some(record) = next_record(&mut reader, &meter, 4096).expect("a record") {
            out.push(record.fields);
        }
        out
    }

    #[test]
    fn records_split_as_duckdb_splits_them() {
        let got = records(b"\xEF\xBB\xBFa,b\r\n\r\ngo\"ld,\"x,\"\"y\"\"\nz\"\n\n\"\",\n");
        let text = |row: &[Vec<u8>]| -> Vec<String> {
            row.iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect()
        };
        let rows: Vec<Vec<String>> = got.iter().map(|row| text(row)).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.get(1),
            Some(&vec!["go\"ld".to_owned(), "x,\"y\"\nz".to_owned()])
        );
        assert_eq!(rows.get(2), Some(&vec![String::new(), String::new()]));
        let meter = Meter::new(caps(1 << 20));
        let mut after = BufReader::new(&b"\"a\"b\n"[..]);
        assert!(
            next_record(&mut after, &meter, 4096).is_err(),
            "text after a closing quote"
        );
    }

    #[test]
    fn a_line_without_an_end_refuses_at_its_bound_without_reading_past_it() {
        let read = Rc::new(AtomicU64::new(0));
        let endless = Counting {
            inner: std::io::repeat(b'w'),
            read: Rc::clone(&read),
        };
        let mut reader = BufReader::with_capacity(8192, endless);
        let meter = Meter::new(caps(1 << 20));
        let refused = next_record(&mut reader, &meter, 4096)
            .map(|_| ())
            .expect_err("an endless line");
        assert_eq!(refused.code, super::EXHAUSTED);
        assert!(read.load(Ordering::SeqCst) <= (1 << 20) + 8192);
    }

    #[test]
    fn a_line_of_commas_refuses_at_its_field_bound() {
        let read = Rc::new(AtomicU64::new(0));
        let commas = Counting {
            inner: std::io::repeat(b','),
            read: Rc::clone(&read),
        };
        let mut reader = BufReader::with_capacity(8192, commas);
        let meter = Meter::new(caps(1 << 30));
        let refused = next_record(&mut reader, &meter, 4096)
            .map(|_| ())
            .expect_err("an endless line of commas");
        assert_eq!(refused.code, "analytics.engine");
        assert!(read.load(Ordering::SeqCst) <= 4096 + 8192);
    }

    #[test]
    fn an_open_quote_over_many_lines_is_linear_and_bounded() {
        let mut text = vec![b'"'];
        text.extend(std::iter::repeat_n(b'\n', 200_000));
        let meter = Meter::new(caps(1 << 20));
        let started = std::time::Instant::now();
        let mut reader = BufReader::new(text.as_slice());
        assert!(
            next_record(&mut reader, &meter, 4096).is_err(),
            "ends inside a quote"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let endless = std::io::repeat(b'\n');
        let mut reader = BufReader::new(Read::chain(&b"\""[..], endless));
        let refused = next_record(&mut reader, &meter, 4096)
            .map(|_| ())
            .expect_err("an endless quoted field");
        assert_eq!(refused.code, super::EXHAUSTED);
        let mut late = caps(1 << 20);
        late.deadline = Duration::ZERO;
        let meter = Meter::new(late);
        let mut reader = BufReader::new(&b"a\n"[..]);
        let refused = next_record(&mut reader, &meter, 4096)
            .map(|_| ())
            .expect_err("past the deadline");
        assert_eq!(refused.code, super::EXHAUSTED);
    }
}
