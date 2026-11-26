//! Bounded HTTP/1.1 request-head reader shared by `dev` and `olapd`.

use std::io::{BufRead, Read as _};

/// Longest accepted request head (request line plus headers), in bytes.
pub(crate) const MAX_HEAD_BYTES: usize = 16 * 1024;

/// A parsed HTTP/1.1 request head before its body is read.
pub(crate) struct RawHead {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) authorization: Option<String>,
    pub(crate) length: usize,
    /// The client keeps the connection for another request: HTTP/1.1
    /// without `connection: close` (or HTTP/1.0 with `connection: keep-alive`),
    /// and without unframed `transfer-encoding`.
    #[cfg(feature = "native")]
    pub(crate) keep_alive: bool,
}

fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    line: &mut String,
    head_bytes: &mut usize,
    head_missing_message: &str,
) -> Result<(), String> {
    line.clear();
    // One byte past the bound, so an over-long head is told apart.
    let left = MAX_HEAD_BYTES.saturating_sub(*head_bytes).saturating_add(1);
    let read = reader
        .by_ref()
        .take(u64::try_from(left).unwrap_or(u64::MAX))
        .read_line(line)
        .map_err(|error| format!("unreadable request: {error}"))?;
    *head_bytes = head_bytes.saturating_add(read);
    if read == 0 || *head_bytes > MAX_HEAD_BYTES {
        return Err(head_missing_message.to_owned());
    }
    Ok(())
}

/// Read and parse a bounded HTTP/1.1 request head, calling `before_line`
/// before each line read (for per-request deadline arming).
pub(crate) fn read_head<R: BufRead, E>(
    reader: &mut R,
    mut before_line: impl FnMut(&R) -> Result<(), E>,
    mut map_invalid: impl FnMut(String) -> E,
    head_missing_message: &str,
    reject_transfer_encoding: bool,
) -> Result<RawHead, E> {
    let mut line = String::new();
    let mut head_bytes = 0_usize;
    before_line(reader)?;
    read_line_bounded(reader, &mut line, &mut head_bytes, head_missing_message)
        .map_err(&mut map_invalid)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(map_invalid("the request line is malformed".to_owned()));
    };
    let http_1_1 = parts.next() != Some("HTTP/1.0");
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut asked_close, mut asked_keep_alive) = (false, false);
    let (mut length, mut authorization, mut transfer_encoding) = (None::<usize>, None, false);
    loop {
        before_line(reader)?;
        read_line_bounded(reader, &mut line, &mut head_bytes, head_missing_message)
            .map_err(&mut map_invalid)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                // A repeated or signed length makes the body boundary
                // ambiguous between this reader and any proxy before it.
                if length.is_some() {
                    return Err(map_invalid("content-length is repeated".to_owned()));
                }
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(map_invalid(
                        "content-length is not a decimal length".to_owned(),
                    ));
                }
                length = Some(
                    value
                        .parse()
                        .map_err(|error| map_invalid(format!("content-length: {error}")))?,
                );
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                if reject_transfer_encoding {
                    return Err(map_invalid("transfer-encoding is not accepted".to_owned()));
                }
                transfer_encoding = true;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            } else if name.eq_ignore_ascii_case("connection") {
                for option in value.split(',').map(str::trim) {
                    if option.eq_ignore_ascii_case("close") {
                        asked_close = true;
                    } else if option.eq_ignore_ascii_case("keep-alive") {
                        asked_keep_alive = true;
                    }
                }
            }
        }
    }
    // `close` wins wherever it appears, in one header or across several;
    // unframed `transfer-encoding` also closes the connection.
    let keep_alive = !asked_close && (http_1_1 || asked_keep_alive) && !transfer_encoding;
    #[cfg(not(feature = "native"))]
    let _ = keep_alive;
    Ok(RawHead {
        method,
        target,
        authorization,
        length: length.unwrap_or(0),
        #[cfg(feature = "native")]
        keep_alive,
    })
}
