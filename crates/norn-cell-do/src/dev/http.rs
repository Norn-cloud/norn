//! A bounded HTTP/1.1 transport for the dev host: one request per
//! connection, served in arrival order, over the same dispatch the Durable
//! Object host uses (`norn_cell_do::server`). It carries method, path, query,
//! the `Authorization` header and a bounded body, nothing else.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::server::{self, Request, Response};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_cell::engine::NativeEngine;

use super::policy::{DevMetrics, DevPrincipals};

/// Longest accepted request head (request line plus headers), in bytes.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest accepted body, in bytes; the action route's own bound is lower.
const MAX_BODY_BYTES: usize = 512 * 1024;

/// The serving state shared with the session: `None` until the first
/// revision is installed.
pub(crate) type Serving = Arc<RwLock<Option<DevHost<NativeEngine>>>>;

/// Serve `listener` until the process exits.
pub(crate) fn serve(
    listener: &TcpListener,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
) {
    for stream in listener.incoming().flatten() {
        // A broken connection affects only itself.
        let _ignored = connection(stream, serving, principals, questions);
    }
}

fn connection(
    mut stream: TcpStream,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let response = match read_request(&stream) {
        Ok(parsed) => dispatch(&parsed, serving, principals, questions),
        Err(refused) => refusal(&refused),
    };
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        _ => "Status",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response.status,
        response.body.len(),
        response.body
    )?;
    stream.flush()
}

struct Parsed {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: String,
}

fn read_request(stream: &TcpStream) -> Result<Parsed, Refusal> {
    let mut reader = BufReader::new(
        stream
            .take(u64::try_from(MAX_HEAD_BYTES.saturating_add(MAX_BODY_BYTES)).unwrap_or(u64::MAX)),
    );
    let mut line = String::new();
    let mut head_bytes = 0_usize;
    let mut next = |reader: &mut BufReader<_>, line: &mut String| -> Result<(), Refusal> {
        line.clear();
        let read = reader
            .read_line(line)
            .map_err(|error| Refusal::invalid(format!("unreadable request: {error}")))?;
        head_bytes = head_bytes.saturating_add(read);
        if read == 0 || head_bytes > MAX_HEAD_BYTES {
            return Err(Refusal::invalid(
                "the request head is missing or exceeds the host bound",
            ));
        }
        Ok(())
    };
    next(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Refusal::invalid("the request line is malformed"));
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut length, mut authorization) = (0_usize, None);
    loop {
        next(&mut reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .parse()
                    .map_err(|error| Refusal::invalid(format!("content-length: {error}")))?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            }
        }
    }
    if length > MAX_BODY_BYTES {
        return Err(Refusal::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a dev host request body is at most 512 KiB",
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).map_err(|error| {
        Refusal::invalid(format!("the body is shorter than content-length: {error}"))
    })?;
    let body = String::from_utf8(body)
        .map_err(|error| Refusal::invalid(format!("the body is not UTF-8: {error}")))?;
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Ok(Parsed {
        path: path.to_owned(),
        query: query.to_owned(),
        method,
        authorization,
        body,
    })
}

fn dispatch(
    parsed: &Parsed,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
) -> Response {
    let Ok(guard) = serving.read() else {
        return refusal(&Refusal::host("the serving state is poisoned"));
    };
    let Some(host) = guard.as_ref() else {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.not_provisioned",
            "the project has no admitted revision yet",
            "fix the refused change shown by norn dev; the first admitted revision is served",
        ));
    };
    let now = super::now_ms().checked_div(1000).unwrap_or_default();
    // The active revision's metrics, derived before the request runs.
    let questions = match questions.questions(host) {
        Ok(questions) => questions,
        Err(refused) => return refusal(&refused),
    };
    server::handle_with(
        host,
        principals,
        Some(&questions),
        &Request {
            method: &parsed.method,
            path: &parsed.path,
            query: &parsed.query,
            authorization: parsed.authorization.as_deref(),
            now,
            body: &parsed.body,
        },
    )
}

fn refusal(refused: &Refusal) -> Response {
    Response {
        status: refused.status,
        body: serde_json::to_string(refused).unwrap_or_else(|_error| String::from("{}")),
    }
}
