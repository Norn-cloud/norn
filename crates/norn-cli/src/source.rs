//! `norn source pull`: read one source and print sealed raw observations.
//!
//! `SRC-READ-1`: a pull only sends GET requests to its declared `ERPNext` connector.
//! `SRC-IDENT-1`: canonical references come only from the reviewed crosswalk.
//! SKUs, labels and arrival order never establish identity, and a row outside
//! the crosswalk carries no reference, so mapping holds it back as a finding.
//! `SRC-LIVE-1`: a refused read fails the pull. There is no recorded fallback.
use clap::{Args, Subcommand};
const MAX_PAGES: usize = 1000;
use norn_ir::connector::{ConnectorKeysetProfile, ConnectorResource, ConnectorRoot};
use norn_ir::{RuntimeObject, RuntimeValue};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Args)]
pub(super) struct SourceArgs {
    #[command(subcommand)]
    command: SourceCommand,
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Read a source and print one sealed raw observation per line.
    Pull {
        /// Environment variable holding the reviewed identity crosswalk JSON.
        #[arg(long)]
        crosswalk_env: String,
        /// Frappe connector artifact declaring the `ERPNext` resources.
        #[arg(long)]
        connector: PathBuf,
        /// Capture time in epoch milliseconds, supplied by the caller's clock.
        #[arg(long)]
        extracted_at_ms: u64,
    },
}

/// Reviewed native document IDs and their canonical references.
type Crosswalk = BTreeMap<String, String>;

fn crosswalk(text: &str) -> Result<Crosswalk, String> {
    let document = RuntimeValue::from_json_text(text).map_err(|e| e.to_string())?;
    let Some(RuntimeValue::Object(items)) = document.pointer("/erpnext/items") else {
        return Err("crosswalk lacks erpnext.items".into());
    };
    items
        .iter()
        .map(|(id, value)| {
            let target = value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("crosswalk targets must be nonblank text")?;
            Ok((id.clone(), target.to_owned()))
        })
        .collect()
}

/// One source record, shaped by its adapter into a declared dataset row.
#[derive(Debug, PartialEq, Eq)]
struct SourceRow {
    id: String,
    version: String,
    record: RuntimeObject,
}

fn text<'a>(value: &'a RuntimeValue, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(RuntimeValue::as_str)
        .ok_or_else(|| format!("source response lacks text at {pointer}"))
}

fn array<'a>(value: &'a RuntimeValue, pointer: &str) -> Result<&'a [RuntimeValue], String> {
    match value.pointer(pointer) {
        Some(RuntimeValue::Array(items)) => Ok(items),
        _ => Err(format!("source response lacks an array at {pointer}")),
    }
}

fn string(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

/// Shape one declared `ERPNext` Item row.
fn erpnext_row(row: &RuntimeValue, crosswalk: &Crosswalk) -> Result<SourceRow, String> {
    let name = text(row, "/name")?;
    let code = text(row, "/item_code")?;
    let item = crosswalk.get(name);
    let mut record = RuntimeObject::from([
        ("name".to_owned(), string(name)),
        ("uom".to_owned(), string(text(row, "/stock_uom")?)),
    ]);
    record.insert("code".into(), string(code));
    record.insert("label".into(), string(text(row, "/item_name")?));
    if let Some(item) = item {
        record.insert("norn_ref".into(), string(item));
    }
    Ok(SourceRow {
        id: name.to_owned(),
        version: text(row, "/modified")?.to_owned(),
        record,
    })
}

/// Seal a row as `norn.raw_observation/v1`; the cell rechecks every member.
fn observation(pipeline: &str, row: SourceRow, extracted_at: u64) -> Result<String, String> {
    let payload = RuntimeValue::Object(row.record);
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let raw = norn_ir::runtime_object! {
        "contract" => "norn.raw_observation/v1",
        "source" => "erpnext",
        "source_id" => row.id,
        "event_id" => row.version.clone(),
        "source_version" => row.version,
        "pipeline_version" => pipeline,
        "kind" => "upsert",
        "extracted_at" => extracted_at,
        "payload" => payload,
        "payload_digest" => digest,
    };
    let line = norn_ir::runtime_object! {"dataset" => "Item", "raw" => raw};
    serde_json::to_string(&line).map_err(|e| e.to_string())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

/// Read a body, refusing any status but 200 without disclosing source error bodies.
fn accepted(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<RuntimeValue, String> {
    let mut response = response.map_err(|e| format!("source request failed: {e}"))?;
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("source response unreadable: {e}"))?;
    if status != 200 {
        return Err(format!("source refused the read: HTTP {status}"));
    }
    RuntimeValue::from_json_text(&body).map_err(|e| e.to_string())
}

fn keyset_read<'a>(
    connector: &'a ConnectorRoot,
    resource: &str,
) -> Result<(String, &'a ConnectorKeysetProfile, String), String> {
    connector
        .resources
        .iter()
        .find_map(|candidate| {
            let ConnectorResource::Delta {
                resource_id,
                endpoint,
                items_path,
                keyset: Some(keyset),
                additional,
                ..
            } = candidate
            else {
                return None;
            };
            if resource_id != resource {
                return None;
            }
            let doctype = additional.get("doctype")?.as_string()?;
            Some((
                endpoint.replace("{doctype}", doctype),
                keyset.as_ref(),
                items_path.clone(),
            ))
        })
        .ok_or_else(|| format!("connector declares no keyset resource {resource}"))
}

fn pull_frappe(
    agent: &ureq::Agent,
    base: &str,
    authorization: &str,
    connector: &ConnectorRoot,
    crosswalk: &Crosswalk,
) -> Result<Vec<SourceRow>, String> {
    let mut rows = Vec::new();
    let (path, keyset, items_path) = keyset_read(connector, "item_catalog")?;
    let url = checked_url(base, &path)?;
    let fields = serde_json::to_string(&keyset.fields).map_err(|e| e.to_string())?;
    let order = format!(
        "{} asc, {} asc",
        keyset.watermark_field, keyset.tie_breaker_field
    );
    let size = keyset.page_size.to_string();
    let page_size = usize::try_from(keyset.page_size).map_err(|e| e.to_string())?;
    if page_size == 0 {
        return Err("source page size must be positive".into());
    }
    let mut cursor: Option<(String, String)> = None;
    for _ in 0..MAX_PAGES {
        let mut request = agent
            .get(&url)
            .query(&keyset.fields_param, &fields)
            .query(&keyset.order_param, &order)
            .query(&keyset.page_param, "0")
            .query(&keyset.size_param, &size);
        if let Some((modified, name)) = &cursor {
            let filters = serde_json::json!([[keyset.watermark_field, ">=", modified]]);
            let alternatives = serde_json::json!([
                [keyset.watermark_field, ">", modified],
                [keyset.tie_breaker_field, ">", name]
            ]);
            request = request
                .query(&keyset.filters_param, filters.to_string())
                .query(&keyset.or_filter_param, alternatives.to_string());
        }
        let page = accepted(request.header("authorization", authorization).call())?;
        let items = array(&page, &items_path)?;
        let Some(last) = items.last() else {
            return Ok(rows);
        };
        let next = (
            text(last, &format!("/{}", keyset.watermark_field))?.to_owned(),
            text(last, &format!("/{}", keyset.tie_breaker_field))?.to_owned(),
        );
        if cursor.as_ref().is_some_and(|old| next <= *old) {
            return Err("source keyset cursor did not advance".into());
        }
        cursor = Some(next);
        for item in items {
            rows.push(erpnext_row(item, crosswalk)?);
        }
        if items.len() < page_size {
            return Ok(rows);
        }
    }
    Err("source exceeded the page bound".into())
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_e| format!("{name} is unavailable"))
}

/// Credentials travel only over TLS, except to this machine.
fn checked_url(base: &str, path: &str) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err("connector endpoint must be a path beginning with '/'".into());
    }
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let uri: ureq::http::Uri = url.parse().map_err(|_error| "invalid source URL")?;
    let authority = uri.authority().ok_or("source URL lacks an authority")?;
    if authority.as_str().contains('@') || uri.query().is_some() {
        return Err("source URL must not contain credentials or a query".into());
    }
    if uri.scheme_str() == Some("https")
        || (uri.scheme_str() == Some("http") && uri.host() == Some("127.0.0.1"))
    {
        return Ok(url);
    }
    Err("source URL must be HTTPS or loopback".into())
}

fn read_connector(path: &Path) -> Result<ConnectorRoot, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let artifact: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let payload = artifact.get("payload").ok_or("connector has no payload")?;
    serde_json::from_value(payload.clone()).map_err(|e| e.to_string())
}

pub(super) fn execute(args: &SourceArgs) -> Result<(String, bool), String> {
    let SourceCommand::Pull {
        crosswalk_env,
        connector,
        extracted_at_ms,
    } = &args.command;
    let crosswalk_text = env(crosswalk_env)?;
    let crosswalk = crosswalk(&crosswalk_text)?;
    let agent = agent();
    let connector = read_connector(connector)?;
    let base = env("ERPNEXT_BASE_URL")?;
    let authorization = format!(
        "token {}:{}",
        env("ERPNEXT_API_KEY")?,
        env("ERPNEXT_API_SECRET")?
    );
    let rows = pull_frappe(&agent, &base, &authorization, &connector, &crosswalk)?;
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&crosswalk_text).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let pipeline = format!("norn-source/1+crosswalk.{digest}");
    let lines = rows
        .into_iter()
        .map(|row| observation(&pipeline, row, *extracted_at_ms))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((lines.join("\n"), true))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "synthetic adapter scenarios")]
mod tests {
    use super::*;

    #[test]
    fn src_ident_1_only_reviewed_items_carry_canonical_identity() {
        let row = RuntimeValue::from_json_text(r#"{"name":"ROW-1","item_code":"ITEM-1","item_name":"Sample","stock_uom":"Nos","modified":"2026-11-04 00:00:00"}"#).expect("row");
        let unknown = erpnext_row(&row, &Crosswalk::default()).expect("unreviewed");
        assert!(!unknown.record.contains_key("norn_ref"));
        let crosswalk =
            crosswalk(r#"{"erpnext":{"items":{"ROW-1":"demo/item/1"}}}"#).expect("reviewed");
        let known = erpnext_row(&row, &crosswalk).expect("mapped");
        assert_eq!(known.record.get("norn_ref"), Some(&string("demo/item/1")));
        let sealed = observation("test/1", known, 1).expect("sealed");
        let sealed = RuntimeValue::from_json_text(sealed).expect("JSON");
        assert_eq!(text(&sealed, "/raw/source"), Ok("erpnext"));
        assert!(text(&sealed, "/raw/payload_digest").is_ok());
    }

    #[test]
    fn src_read_1_keyset_keeps_the_boundary_item_when_an_earlier_item_moves() {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let server = std::thread::spawn(move || {
            let row = |name: &str, modified: &str| serde_json::json!({"name":name,"item_code":name,"item_name":name,"stock_uom":"Nos","modified":modified});
            let mut requests = Vec::new();
            for step in 0..4 {
                let (mut socket, _) = listener.accept().expect("request");
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("timeout");
                let mut reader = BufReader::new(socket.try_clone().expect("socket"));
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("header");
                    if line.is_empty() || line == "\r\n" {
                        break;
                    }
                    request.push_str(&line);
                }
                let keyset = request.contains("or_filters=");
                let data = match (step, keyset) {
                    (0, _) => vec![row("ROW-1", "2026-11-04 00:00:00")],
                    (1, true) => vec![row("ROW-2", "2026-11-04 00:00:00")],
                    (1, false) | (2, true) => vec![row("ROW-1", "2026-11-04 00:00:01")],
                    _ => Vec::new(),
                };
                let done = data.is_empty();
                let body = serde_json::json!({"data":data}).to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("response");
                requests.push(request);
                if done {
                    break;
                }
            }
            requests
        });
        let mut connector = read_connector(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/connectors/frappe.connector.json"
        )))
        .expect("connector");
        for resource in &mut connector.resources {
            if let ConnectorResource::Delta {
                resource_id,
                keyset: Some(keyset),
                ..
            } = resource
                && resource_id == "item_catalog"
            {
                keyset.page_size = 1;
            }
        }
        let rows = pull_frappe(
            &agent(),
            &base,
            "token fixture:fixture",
            &connector,
            &Crosswalk::default(),
        )
        .expect("sweep");
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["ROW-1", "ROW-2", "ROW-1"]
        );
        let requests = server.join().expect("server");
        assert_eq!(requests.len(), 4);
        assert!(
            requests
                .iter()
                .all(|request| request.contains("limit_start=0"))
        );
        let second = requests.get(1).expect("second page");
        assert!(
            second.contains("filters=")
                && second.contains("or_filters=")
                && second.contains("ROW-1")
        );
    }

    #[test]
    fn src_read_1_credentials_require_tls_or_loopback() {
        for path in ["@evil.example/x", "api/x", "https://evil.example/x"] {
            assert!(checked_url("https://erp.example", path).is_err());
        }
        assert!(checked_url("http://example.test", "/api/resource/Item").is_err());
        assert!(checked_url("http://127.0.0.1.example.test", "/api/resource/Item").is_err());
        assert!(checked_url("http://127.0.0.1:8000@example.test", "/api/resource/Item").is_err());
        assert!(checked_url("https://user:password@example.test", "/api/resource/Item").is_err());
        assert!(checked_url("http://127.0.0.1:8000", "/api/resource/Item").is_ok());
        assert!(checked_url("https://example.test", "/api/resource/Item").is_ok());
    }
}
