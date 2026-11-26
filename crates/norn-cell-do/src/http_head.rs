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

/// RFC 9110 §5.6.2 `tchar` token check for HTTP field names.
fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// RFC 9110 §5.5 / RFC 9112 §2.2 field value check: reject control bytes
/// (including bare CR and NUL) other than HTAB.
fn is_header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (byte >= 0x20 && byte != 0x7f))
}

fn strip_eol(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

fn parse_request_line(line: &str) -> Result<(String, String, bool), String> {
    let mut parts = strip_eol(line).split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("the request line is malformed".to_owned());
    };
    if !is_header_name(method) || target.is_empty() {
        return Err("the request line is malformed".to_owned());
    }
    let http_1_1 = match version {
        "HTTP/1.1" => true,
        "HTTP/1.0" => false,
        _ => return Err("unsupported HTTP version".to_owned()),
    };
    Ok((method.to_owned(), target.to_owned(), http_1_1))
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
    let (method, target, http_1_1) = parse_request_line(&line).map_err(&mut map_invalid)?;
    let (mut asked_close, mut asked_keep_alive) = (false, false);
    let (mut length, mut authorization, mut transfer_encoding) = (None::<usize>, None, false);
    loop {
        before_line(reader)?;
        read_line_bounded(reader, &mut line, &mut head_bytes, head_missing_message)
            .map_err(&mut map_invalid)?;
        let header = strip_eol(&line);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(map_invalid("malformed header line".to_owned()));
        };
        if !is_header_name(name) {
            return Err(map_invalid("malformed header name".to_owned()));
        }
        if !is_header_value(value) {
            return Err(map_invalid("malformed header value".to_owned()));
        }
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
            if authorization.is_some() {
                return Err(map_invalid("authorization is repeated".to_owned()));
            }
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::read_head;

    fn parse(raw: &[u8], reject_te: bool) -> Result<super::RawHead, String> {
        let mut cursor = Cursor::new(raw);
        read_head(
            &mut cursor,
            |_| Ok(()),
            |msg| msg,
            "head missing",
            reject_te,
        )
    }

    #[test]
    fn rejects_smuggled_header_names_duplicate_auth_and_malformed_request_lines() {
        for (raw, expected) in [
            (
                b"POST /v1/sync HTTP/1.1\r\nTransfer-Encoding : chunked\r\n\r\n".as_slice(),
                "malformed header name",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\nContent-Length : 5\r\n\r\n".as_slice(),
                "malformed header name",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\n obs-fold: x\r\n\r\n".as_slice(),
                "malformed header name",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\nno-colon-header\r\n\r\n".as_slice(),
                "malformed header line",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\nAuthorization: Bearer a\r\nAuthorization: Bearer b\r\n\r\n".as_slice(),
                "authorization is repeated",
            ),
            (
                b"POST /v1/sync\r\n\r\n".as_slice(),
                "the request line is malformed",
            ),
            (
                b"POST /v1/sync HTTP/1.1 EXTRA\r\n\r\n".as_slice(),
                "the request line is malformed",
            ),
            (
                b"POST  /v1/sync HTTP/1.1\r\n\r\n".as_slice(),
                "the request line is malformed",
            ),
            (
                b" POST /v1/sync HTTP/1.1\r\n\r\n".as_slice(),
                "the request line is malformed",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\nX-Bad: a\rb\r\n\r\n".as_slice(),
                "malformed header value",
            ),
            (
                b"POST /v1/sync HTTP/1.1\r\nX-Bad: a\x00b\r\n\r\n".as_slice(),
                "malformed header value",
            ),
            (
                b"POST /v1/sync HTTP/2.0\r\n\r\n".as_slice(),
                "unsupported HTTP version",
            ),
        ] {
            assert_eq!(
                parse(raw, true).err().as_deref(),
                Some(expected),
                "{raw:?}"
            );
        }
        assert!(parse(b"GET /healthz HTTP/1.0\r\n\r\n", true).is_ok());
    }
}
