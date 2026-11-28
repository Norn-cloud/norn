//! Bounded HTTP/1.1 wire codec and response helpers for `norn dev`.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;

use norn_api::privacy::{DisclosedDocument, serialize_disclosed};
use norn_api::refusal::Refusal;
use serde_json::{Map, Value};

/// Maximum accepted HTTP/1.1 request body size (2 MiB).
pub(super) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const BODY_CHUNK: usize = 8 * 1024;

/// One parsed bounded HTTP/1.1 request.
#[derive(Debug, Clone)]
pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: String,
}

/// One serialized HTTP/1.1 JSON response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HttpResponse {
    pub(super) status: u16,
    pub(super) body: String,
}

/// Parse URL query string (`k=v&...`) with percent-decoding; duplicate keys refuse.
pub(super) fn parse_query(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    if raw.is_empty() {
        return Ok(map);
    }
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(k)?;
        let val = percent_decode(v)?;
        if map.insert(key.clone(), val).is_some() {
            return Err(format!("duplicate query parameter `{key}`"));
        }
    }
    Ok(map)
}

/// Decode `%XX` and `+` sequences in a URL path segment or query component.
pub(super) fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0_usize;
    while let Some(&byte) = bytes.get(idx) {
        if byte == b'%' {
            let hi = bytes
                .get(idx.saturating_add(1))
                .and_then(|b| hex_nibble(*b))
                .ok_or_else(|| "invalid percent-encoding".to_owned())?;
            let lo = bytes
                .get(idx.saturating_add(2))
                .and_then(|b| hex_nibble(*b))
                .ok_or_else(|| "invalid percent-encoding".to_owned())?;
            out.push(hi.wrapping_shl(4) | lo);
            idx = idx.saturating_add(3);
        } else if byte == b'+' {
            out.push(b' ');
            idx = idx.saturating_add(1);
        } else {
            out.push(byte);
            idx = idx.saturating_add(1);
        }
    }
    let Ok(decoded) = String::from_utf8(out) else {
        return Err("decoded URL component is not valid UTF-8".to_owned());
    };
    Ok(decoded)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(byte.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

/// Read one bounded HTTP/1.1 request from `reader` (refusing chunked transfer and > 2 MiB bodies).
pub(super) fn read_http_request(
    reader: &mut BufReader<&TcpStream>,
) -> Result<HttpRequest, HttpResponse> {
    let (method, target, headers) = read_head(reader)?;
    if headers.contains_key("transfer-encoding") {
        return Err(refusal_response(&Refusal::invalid(
            "transfer-encoding is not supported; send content-length",
        )));
    }
    let content_length = match headers.get("content-length") {
        None => 0_usize,
        Some(raw) => {
            let Ok(parsed) = raw.trim().parse::<usize>() else {
                return Err(refusal_response(&Refusal::invalid(
                    "invalid content-length header",
                )));
            };
            parsed
        }
    };
    if content_length > MAX_BODY_BYTES {
        return Err(synthetic_refusal(
            413,
            "request.too_large",
            "limit",
            "the request body exceeds the host bound",
            "norn dev accepts request bodies up to 2 MiB",
        ));
    }
    let body = read_exact_body(reader, content_length)?;
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Ok(HttpRequest {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body,
    })
}

fn read_head(
    reader: &mut BufReader<&TcpStream>,
) -> Result<(String, String, BTreeMap<String, String>), HttpResponse> {
    let mut total = 0_usize;
    let mut line_buf = Vec::new();
    let request_line = read_header_line(reader, &mut line_buf, &mut total)?;
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(refusal_response(&Refusal::invalid(
            "malformed HTTP/1.1 request line",
        )));
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(refusal_response(&Refusal::invalid(
            "unsupported HTTP version",
        )));
    }
    let mut headers = BTreeMap::new();
    let mut count = 0_usize;
    loop {
        let line = read_header_line(reader, &mut line_buf, &mut total)?;
        if line.is_empty() {
            break;
        }
        count = count.saturating_add(1);
        if count > MAX_HEADERS {
            return Err(refusal_response(&Refusal::invalid("too many HTTP headers")));
        }
        let Some((name, val)) = line.split_once(':') else {
            return Err(refusal_response(&Refusal::invalid(
                "malformed HTTP header line",
            )));
        };
        let lower = name.trim().to_ascii_lowercase();
        let value = val.trim().to_owned();
        if lower == "content-length"
            && let Some(prev) = headers.get(&lower)
            && prev != &value
        {
            return Err(refusal_response(&Refusal::invalid(
                "conflicting content-length headers",
            )));
        }
        headers.insert(lower, value);
    }
    Ok((method.to_owned(), target.to_owned(), headers))
}

fn read_header_line(
    reader: &mut BufReader<&TcpStream>,
    buf: &mut Vec<u8>,
    total: &mut usize,
) -> Result<String, HttpResponse> {
    buf.clear();
    let remaining = MAX_HEAD_BYTES.saturating_sub(*total).saturating_add(1);
    let read = reader
        .by_ref()
        .take(u64::try_from(remaining).unwrap_or(u64::MAX))
        .read_until(b'\n', buf)
        .map_err(|err| refusal_response(&Refusal::invalid(format!("unreadable head: {err}"))))?;
    if read == 0 {
        return Err(refusal_response(&Refusal::invalid(
            "unexpected EOF while reading HTTP request head",
        )));
    }
    *total = total.saturating_add(read);
    if *total > MAX_HEAD_BYTES {
        return Err(refusal_response(&Refusal::invalid(
            "HTTP request head exceeds 16 KiB bound",
        )));
    }
    let Ok(text) = std::str::from_utf8(buf) else {
        return Err(refusal_response(&Refusal::invalid(
            "HTTP head is not valid UTF-8",
        )));
    };
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

fn read_exact_body(
    reader: &mut BufReader<&TcpStream>,
    length: usize,
) -> Result<String, HttpResponse> {
    let mut body = Vec::with_capacity(length.min(BODY_CHUNK));
    let mut chunk = vec![0_u8; BODY_CHUNK];
    while body.len() < length {
        let want = length.saturating_sub(body.len()).min(BODY_CHUNK);
        let Some(slice) = chunk.get_mut(..want) else {
            break;
        };
        match reader.read(slice) {
            Ok(0) => {
                return Err(refusal_response(&Refusal::invalid(
                    "request body shorter than content-length",
                )));
            }
            Ok(n) => {
                if let Some(filled) = slice.get(..n) {
                    body.extend_from_slice(filled);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => {
                return Err(refusal_response(&Refusal::invalid(format!(
                    "failed reading request body: {err}"
                ))));
            }
        }
    }
    let Ok(decoded) = String::from_utf8(body) else {
        return Err(refusal_response(&Refusal::invalid(
            "request body is not valid UTF-8",
        )));
    };
    Ok(decoded)
}

/// Write one HTTP/1.1 JSON response with `connection: close`.
pub(super) fn write_http_response(
    mut stream: &TcpStream,
    response: &HttpResponse,
) -> std::io::Result<()> {
    let reason = status_reason(response.status);
    let head = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        response.body.len()
    );
    let mut wire = Vec::with_capacity(head.len().saturating_add(response.body.len()));
    wire.extend_from_slice(head.as_bytes());
    wire.extend_from_slice(response.body.as_bytes());
    stream.write_all(&wire)?;
    stream.flush()
}

const fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Convert a pre-serialized JSON string result into an [`HttpResponse`].
pub(super) fn encoded_json_response(
    status: u16,
    encoded: Result<String, serde_json::Error>,
) -> HttpResponse {
    match encoded {
        Ok(body) => HttpResponse { status, body },
        Err(err) => refusal_response(&Refusal::host(err)),
    }
}

/// Serialize a sealed [`DisclosedDocument`] into a `200 OK` JSON response.
pub(super) fn disclosed_ok(doc: &impl DisclosedDocument) -> HttpResponse {
    match serialize_disclosed(doc) {
        Ok(body) => HttpResponse { status: 200, body },
        Err(refusal) => refusal_response(&refusal),
    }
}

/// Serialize a [`Refusal`] using its exact HTTP status code and wire JSON.
pub(super) fn refusal_response(refusal: &Refusal) -> HttpResponse {
    HttpResponse {
        status: refusal.status,
        body: serde_json::to_string(refusal).unwrap_or_else(|_err| "{}".to_owned()),
    }
}

/// Return a `405 Method Not Allowed` refusal document.
pub(super) fn method_not_allowed(allowed: &str) -> HttpResponse {
    synthetic_refusal(
        405,
        "request.method_not_allowed",
        "invalid",
        "method not allowed",
        &format!("this route accepts {allowed}"),
    )
}

/// Construct a `norn/refusal@2` response for transport-level refusals (404/405/413).
pub(super) fn synthetic_refusal(
    status: u16,
    code: &str,
    category: &str,
    message: &str,
    explanation: &str,
) -> HttpResponse {
    let mut obj = Map::new();
    obj.insert("format".to_owned(), Value::from("norn/refusal@2"));
    obj.insert("retryable".to_owned(), Value::Bool(false));
    obj.insert("reason".to_owned(), Value::Null);
    obj.insert("code".to_owned(), Value::from(code));
    obj.insert("category".to_owned(), Value::from(category));
    obj.insert("message".to_owned(), Value::from(message));
    obj.insert("explanation".to_owned(), Value::from(explanation));
    HttpResponse {
        status,
        body: Value::Object(obj).to_string(),
    }
}
