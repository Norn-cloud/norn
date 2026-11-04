//! `norn source pull`: read one source and print sealed raw observations.
//!
//! `SRC-READ-1`: a pull only sends GET requests to its declared ERPNext connector.
//! `SRC-IDENT-1`: canonical references come only from the reviewed crosswalk.
//! SKUs, labels and arrival order never establish identity, and a row outside
//! the crosswalk carries no reference, so mapping holds it back as a finding.
//! `SRC-LIVE-1`: a refused read fails the pull. There is no recorded fallback.
use clap::{Args, Subcommand};
const MAX_PAGES: usize = 1000;
use norn_ir::connector::{ConnectorKeysetProfile, ConnectorResource, ConnectorRoot};
use norn_ir::{RuntimeObject, RuntimeValue};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        /// Frappe connector artifact declaring the ERPNext resources.
        #[arg(long)]
        connector: PathBuf,
    },
}

/// Reviewed source-to-canonical references for one system.
#[derive(Debug, Default, PartialEq, Eq)]
struct Crosswalk {
    items: BTreeMap<String, String>,
}

impl Crosswalk {
    fn parse(text: &str) -> Result<Self, String> {
        let document = RuntimeValue::from_json_text(text).map_err(|e| e.to_string())?;
        let section = document
            .get("erpnext")
            .ok_or("crosswalk has no section for this system")?;
        let table = |name: &str| -> Result<BTreeMap<String, String>, String> {
            let Some(RuntimeValue::Object(entries)) = section.get(name) else {
                return Err(format!("crosswalk section lacks {name}"));
            };
            entries
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| (key.clone(), text.to_owned()))
                        .ok_or_else(|| format!("crosswalk {name} entries must be nonblank text"))
                })
                .collect()
        };
        Ok(Self {
            items: table("items")?,
        })
    }
}

/// One source record, shaped by its adapter into a declared dataset row.
#[derive(Debug, PartialEq, Eq)]
struct SourceRow {
    dataset: &'static str,
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

/// Shape one declared ERPNext Item row.
fn erpnext_row(
    dataset: &'static str,
    row: &RuntimeValue,
    crosswalk: &Crosswalk,
) -> Result<SourceRow, String> {
    let name = text(row, "/name")?;
    let code = text(row, "/item_code")?;
    let item = crosswalk.items.get(code);
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
        dataset,
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
    let line = norn_ir::runtime_object! {"dataset" => row.dataset, "raw" => raw};
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
        .find_map(|candidate| match candidate {
            ConnectorResource::Delta {
                resource_id,
                endpoint,
                items_path,
                keyset: Some(keyset),
                additional,
                ..
            } if resource_id == resource => {
                let doctype = additional.get("doctype")?.as_str()?;
                Some((
                    endpoint.replace("{doctype}", doctype),
                    keyset.as_ref(),
                    items_path.clone(),
                ))
            }
            _ => None,
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
    let (resource, dataset) = ("item_catalog", "Item");
    let (path, keyset, items_path) = keyset_read(connector, resource)?;
    let fields = serde_json::to_string(&keyset.fields).map_err(|e| e.to_string())?;
    let order = format!(
        "{} asc, {} asc",
        keyset.watermark_field, keyset.tie_breaker_field
    );
    let size = keyset.page_size.to_string();
    let mut offset = 0_usize;
    let mut pages = 0;
    loop {
        pages += 1;
        if pages > MAX_PAGES {
            return Err("source exceeded the page bound".into());
        }
        let page = accepted(
            agent
                .get(format!("{base}{path}"))
                .query(&keyset.fields_param, &fields)
                .query(&keyset.order_param, &order)
                .query(&keyset.page_param, offset.to_string())
                .query(&keyset.size_param, &size)
                .header("authorization", authorization)
                .call(),
        )?;
        let items = array(&page, &items_path)?;
        for item in items {
            rows.push(erpnext_row(dataset, item, crosswalk)?);
        }
        if items.len() < keyset.page_size as usize {
            break;
        }
        offset += items.len();
    }
    Ok(rows)
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_e| format!("{name} is unavailable"))
}

/// Credentials travel only over TLS, except to this machine.
fn checked_base(base: &str) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    if base.starts_with("https://") || base.starts_with("http://127.0.0.1:") {
        return Ok(base.to_owned());
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
    } = &args.command;
    let crosswalk_text = env(crosswalk_env)?;
    let crosswalk = Crosswalk::parse(&crosswalk_text)?;
    let agent = agent();
    let connector = read_connector(connector)?;
    let base = checked_base(&env("ERPNEXT_BASE_URL")?)?;
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
    let extracted_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let extracted_at = u64::try_from(extracted_at).map_err(|e| e.to_string())?;
    let mut output = String::new();
    for row in rows {
        let line = observation(&pipeline, row, extracted_at)?;
        writeln!(output, "{line}").map_err(|e| e.to_string())?;
    }
    Ok((output, true))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "synthetic adapter scenarios")]
mod tests {
    use super::*;

    #[test]
    fn src_ident_1_only_reviewed_items_carry_canonical_identity() {
        let row = RuntimeValue::from_json_text(r#"{"name":"ITEM-1","item_code":"ITEM-1","item_name":"Sample","stock_uom":"Nos","modified":"2026-11-04 00:00:00"}"#).expect("row");
        let unknown = erpnext_row("Item", &row, &Crosswalk::default()).expect("unreviewed");
        assert!(unknown.record.get("norn_ref").is_none());
        let crosswalk = Crosswalk::parse(r#"{"erpnext":{"items":{"ITEM-1":"demo/item/1"}}}"#)
            .expect("reviewed");
        let known = erpnext_row("Item", &row, &crosswalk).expect("mapped");
        assert_eq!(known.record.get("norn_ref"), Some(&string("demo/item/1")));
        let sealed = observation("test/1", known, 1).expect("sealed");
        let sealed = RuntimeValue::from_json_text(sealed).expect("JSON");
        assert_eq!(text(&sealed, "/raw/source"), Ok("erpnext"));
        assert!(text(&sealed, "/raw/payload_digest").is_ok());
    }

    #[test]
    fn src_read_1_credentials_require_tls_or_loopback() {
        assert!(checked_base("http://example.test").is_err());
        assert!(checked_base("http://127.0.0.1.example.test").is_err());
        assert!(checked_base("http://127.0.0.1:8000").is_ok());
        assert!(checked_base("https://example.test").is_ok());
    }
}
