//! `norn source pull`: read one source and print sealed raw observations.
//!
//! `SRC-READ-1`: a pull only reads. Shopify receives one GraphQL `query` and a
//! token exchange; Frappe receives GET requests from its declared connector.
//! `SRC-IDENT-1`: canonical references come only from the reviewed crosswalk.
//! SKUs, labels and arrival order never establish identity, and a row outside
//! the crosswalk carries no reference, so mapping holds it back as a finding.
//! `SRC-LIVE-1`: a refused read fails the pull. There is no recorded fallback.
use clap::{Args, Subcommand, ValueEnum};
use norn_ir::connector::{ConnectorKeysetProfile, ConnectorResource, ConnectorRoot};
use norn_ir::{RuntimeObject, RuntimeValue};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The Admin API release this adapter's query is written against.
const SHOPIFY_API_VERSION: &str = "2026-10";
const MAX_PAGES: usize = 1000;
const VARIANT_PAGE: &str = r#"query Variants($after: String) {
  productVariants(first: 25, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes {
      id sku displayName price updatedAt
      inventoryItem {
        inventoryLevels(first: 10) {
          pageInfo { hasNextPage }
          nodes { id updatedAt location { id } quantities(names: ["available"]) { name quantity } }
        }
      }
    }
  }
}"#;

#[derive(Debug, Args)]
pub(super) struct SourceArgs {
    #[command(subcommand)]
    command: SourceCommand,
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    /// Read a source and print one sealed raw observation per line.
    Pull {
        system: System,
        /// Environment variable holding the reviewed identity crosswalk JSON.
        #[arg(long)]
        crosswalk_env: String,
        /// Frappe connector artifact declaring the ERPNext resources.
        #[arg(long, required_if_eq("system", "erpnext"))]
        connector: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum System {
    Shopify,
    Erpnext,
}

impl System {
    const fn name(self) -> &'static str {
        match self {
            Self::Shopify => "shopify",
            Self::Erpnext => "erpnext",
        }
    }
}

/// Reviewed source-to-canonical references for one system.
#[derive(Debug, Default, PartialEq, Eq)]
struct Crosswalk {
    items: BTreeMap<String, String>,
    locations: BTreeMap<String, String>,
}

impl Crosswalk {
    fn parse(text: &str, system: System) -> Result<Self, String> {
        let document = RuntimeValue::from_json_text(text).map_err(|e| e.to_string())?;
        let section = document
            .get(system.name())
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
            locations: table("locations")?,
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

/// Add the reviewed item and location references a stock row needs.
fn stock_refs(
    record: &mut RuntimeObject,
    system: System,
    item: Option<&String>,
    location: Option<&String>,
) {
    if let Some(item) = item {
        record.insert("item_ref".into(), string(item));
    }
    if let Some(location) = location {
        record.insert("location".into(), string(location));
    }
    if let (Some(item), Some(location)) = (item, location) {
        let reference = format!("{item}/holding/{}/{location}", system.name());
        record.insert("norn_ref".into(), RuntimeValue::String(reference));
    }
}

/// Shape one `VARIANT_PAGE` response.
fn shopify_rows(page: &RuntimeValue, crosswalk: &Crosswalk) -> Result<Vec<SourceRow>, String> {
    let mut rows = Vec::new();
    for variant in array(page, "/data/productVariants/nodes")? {
        let id = text(variant, "/id")?;
        let sku = text(variant, "/sku").unwrap_or_default();
        let item = crosswalk.items.get(id);
        let mut record = RuntimeObject::from([
            ("id".to_owned(), string(id)),
            ("code".to_owned(), string(sku)),
            ("sku".to_owned(), string(sku)),
            ("label".to_owned(), string(text(variant, "/displayName")?)),
            ("price".to_owned(), string(text(variant, "/price")?)),
        ]);
        if let Some(item) = item {
            record.insert("norn_ref".into(), string(item));
        }
        rows.push(SourceRow {
            dataset: "ProductVariant",
            id: id.to_owned(),
            version: text(variant, "/updatedAt")?.to_owned(),
            record,
        });
        let levels = "/inventoryItem/inventoryLevels";
        if variant.pointer(&format!("{levels}/pageInfo/hasNextPage"))
            != Some(&RuntimeValue::Bool(false))
        {
            return Err(format!("inventory levels of {id} exceed one page"));
        }
        for level in array(variant, &format!("{levels}/nodes"))? {
            let quantity = array(level, "/quantities")?
                .iter()
                .find(|entry| entry.get("name").and_then(RuntimeValue::as_str) == Some("available"))
                .and_then(|entry| entry.get("quantity"))
                .ok_or("inventory level lacks an available quantity")?;
            let level_id = text(level, "/id")?;
            let mut record = RuntimeObject::from([
                ("id".to_owned(), string(level_id)),
                ("resource_code".to_owned(), string(sku)),
                ("quantity".to_owned(), quantity.clone()),
            ]);
            let location = crosswalk.locations.get(text(level, "/location/id")?);
            stock_refs(&mut record, System::Shopify, item, location);
            rows.push(SourceRow {
                dataset: "InventoryLevel",
                id: level_id.to_owned(),
                version: text(level, "/updatedAt")?.to_owned(),
                record,
            });
        }
    }
    Ok(rows)
}

/// Shape one Frappe `item_catalog` or `stock_balance` row.
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
    if dataset == "Item" {
        record.insert("code".into(), string(code));
        record.insert("label".into(), string(text(row, "/item_name")?));
        if let Some(item) = item {
            record.insert("norn_ref".into(), string(item));
        }
    } else {
        let quantity = row.get("actual_qty").ok_or("Bin lacks actual_qty")?;
        record.insert("resource_code".into(), string(code));
        record.insert("quantity".into(), quantity.clone());
        let location = crosswalk.locations.get(text(row, "/warehouse")?);
        stock_refs(&mut record, System::Erpnext, item, location);
    }
    Ok(SourceRow {
        dataset,
        id: name.to_owned(),
        version: text(row, "/modified")?.to_owned(),
        record,
    })
}

/// Seal a row as `norn.raw_observation/v1`; the cell rechecks every member.
fn observation(
    system: System,
    pipeline: &str,
    row: SourceRow,
    extracted_at: u64,
) -> Result<String, String> {
    let payload = RuntimeValue::Object(row.record);
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let raw = norn_ir::runtime_object! {
        "contract" => "norn.raw_observation/v1",
        "source" => system.name(),
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

/// Read a body, refusing any status but 200 with the source's own error member.
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
        let detail = RuntimeValue::from_json_text(&body)
            .ok()
            .and_then(|value| value.get("errors").and_then(|e| e.as_str().map(str::to_owned)));
        return Err(format!(
            "source refused the read: HTTP {status}{}",
            detail.map_or_else(String::new, |detail| format!(" ({detail})"))
        ));
    }
    RuntimeValue::from_json_text(&body).map_err(|e| e.to_string())
}

/// Exchange app credentials for a token; its granted scopes bound every read.
fn shopify_token(agent: &ureq::Agent, base: &str, id: &str, secret: &str) -> Result<String, String> {
    let response = agent
        .post(format!("{base}/admin/oauth/access_token"))
        .send_form([
            ("grant_type", "client_credentials"),
            ("client_id", id),
            ("client_secret", secret),
        ]);
    Ok(text(&accepted(response)?, "/access_token")?.to_owned())
}

fn pull_shopify(
    agent: &ureq::Agent,
    base: &str,
    token: &str,
    crosswalk: &Crosswalk,
) -> Result<Vec<SourceRow>, String> {
    let url = format!("{base}/admin/api/{SHOPIFY_API_VERSION}/graphql.json");
    let mut rows = Vec::new();
    let mut after = RuntimeValue::Null;
    for _ in 0..MAX_PAGES {
        let body = norn_ir::runtime_object! {
            "query" => VARIANT_PAGE,
            "variables" => norn_ir::runtime_object! {"after" => after.clone()},
        };
        let body = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        let page = accepted(
            agent
                .post(&url)
                .header("content-type", "application/json")
                .header("x-shopify-access-token", token)
                .send(body.as_bytes()),
        )?;
        if let Some(errors) = page.get("errors") {
            let errors = serde_json::to_string(errors).map_err(|e| e.to_string())?;
            return Err(format!("source refused the query: {errors}"));
        }
        rows.extend(shopify_rows(&page, crosswalk)?);
        let info = "/data/productVariants/pageInfo";
        if page.pointer(&format!("{info}/hasNextPage")) != Some(&RuntimeValue::Bool(true)) {
            return Ok(rows);
        }
        after = string(text(&page, &format!("{info}/endCursor"))?);
    }
    Err("source exceeded the page bound".into())
}

/// The declared keyset read of one connector resource, as a request path.
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
    for (resource, dataset) in [("item_catalog", "Item"), ("stock_balance", "Bin")] {
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

fn read_connector(path: &PathBuf) -> Result<ConnectorRoot, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let artifact: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let payload = artifact.get("payload").ok_or("connector has no payload")?;
    serde_json::from_value(payload.clone()).map_err(|e| e.to_string())
}

pub(super) fn execute(args: &SourceArgs) -> Result<(String, bool), String> {
    let SourceCommand::Pull {
        system,
        crosswalk_env,
        connector,
    } = &args.command;
    let crosswalk_text = env(crosswalk_env)?;
    let crosswalk = Crosswalk::parse(&crosswalk_text, *system)?;
    let agent = agent();
    let rows = match system {
        System::Shopify => {
            let base = checked_base(&format!("https://{}", env("SHOPIFY_SHOP")?))?;
            let token = shopify_token(
                &agent,
                &base,
                &env("SHOPIFY_CLIENT_ID")?,
                &env("SHOPIFY_CLIENT_SECRET")?,
            )?;
            pull_shopify(&agent, &base, &token, &crosswalk)?
        }
        System::Erpnext => {
            let connector = read_connector(connector.as_ref().ok_or("--connector is required")?)?;
            let base = checked_base(&env("ERPNEXT_BASE_URL")?)?;
            let authorization = format!(
                "token {}:{}",
                env("ERPNEXT_API_KEY")?,
                env("ERPNEXT_API_SECRET")?
            );
            pull_frappe(&agent, &base, &authorization, &connector, &crosswalk)?
        }
    };
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
        let line = observation(*system, &pipeline, row, extracted_at)?;
        writeln!(output, "{line}").map_err(|e| e.to_string())?;
    }
    Ok((output, true))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "synthetic adapter tests")]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    const CROSSWALK: &str = r#"{"shopify":{
        "items":{"gid://shopify/ProductVariant/1":"demo/item/a"},
        "locations":{"gid://shopify/Location/9":"north"}},
      "erpnext":{"items":{"SYN-A":"demo/item/a"},"locations":{"Stores - SYN":"north"}}}"#;
    const PAGE: &str = r#"{"data":{"productVariants":{"pageInfo":{"hasNextPage":false},"nodes":[
      {"id":"gid://shopify/ProductVariant/1","sku":"SYN-1","displayName":"Synthetic A","price":"12.50",
       "updatedAt":"2026-11-04T00:00:00Z","inventoryItem":{"inventoryLevels":{"pageInfo":{"hasNextPage":false},
       "nodes":[{"id":"gid://shopify/InventoryLevel/5","updatedAt":"2026-11-04T01:00:00Z",
       "location":{"id":"gid://shopify/Location/9"},"quantities":[{"name":"available","quantity":7}]}]}}},
      {"id":"gid://shopify/ProductVariant/2","sku":"SYN-1","displayName":"Same SKU","price":"1.00",
       "updatedAt":"2026-11-04T00:00:00Z","inventoryItem":{"inventoryLevels":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}
    ]}}}"#;

    fn crosswalk(system: System) -> Crosswalk {
        Crosswalk::parse(CROSSWALK, system).expect("crosswalk")
    }

    /// Serve canned responses in order, retaining each request's head and body.
    fn serve(
        responses: Vec<(u16, String)>,
    ) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let (mut head, mut length) = (String::new(), 0_usize);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("header");
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().expect("length");
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut request = vec![0; length];
                reader.read_exact(&mut request).expect("body");
                head.push_str(&String::from_utf8(request).expect("UTF-8"));
                seen.push(head);
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("response");
            }
            seen
        });
        (base, handle)
    }

    #[test]
    fn src_ident_1_only_reviewed_rows_carry_a_canonical_reference() {
        let page = RuntimeValue::from_json_text(PAGE).expect("page");
        let rows = shopify_rows(&page, &crosswalk(System::Shopify)).expect("rows");
        let reference = |index: usize| rows[index].record.get("norn_ref").and_then(|v| v.as_str());
        assert_eq!(rows.len(), 3);
        assert_eq!(reference(0), Some("demo/item/a"));
        assert_eq!(reference(1), Some("demo/item/a/holding/shopify/north"));
        assert_eq!(reference(2), None, "a shared SKU establishes no identity");
    }

    #[test]
    fn src_ident_1_an_unreviewed_location_leaves_stock_without_identity() {
        let page = RuntimeValue::from_json_text(PAGE).expect("page");
        let mut reviewed = crosswalk(System::Shopify);
        reviewed.locations.clear();
        let rows = shopify_rows(&page, &reviewed).expect("rows");
        assert!(!rows[1].record.contains_key("norn_ref"));
        assert_eq!(
            rows[1].record.get("item_ref").and_then(|v| v.as_str()),
            Some("demo/item/a")
        );
    }

    #[test]
    fn truncated_inventory_levels_refuse_the_pull() {
        let page = PAGE.replacen(
            r#""inventoryLevels":{"pageInfo":{"hasNextPage":false}"#,
            r#""inventoryLevels":{"pageInfo":{"hasNextPage":true}"#,
            1,
        );
        let page = RuntimeValue::from_json_text(page).expect("page");
        let error = shopify_rows(&page, &crosswalk(System::Shopify)).expect_err("refused");
        assert!(error.contains("exceed one page"), "{error}");
    }

    #[test]
    fn erpnext_rows_keep_the_exact_quantity_and_reviewed_identity() {
        let row = RuntimeValue::from_json_text(
            r#"{"name":"bin-1","item_code":"SYN-A","warehouse":"Stores - SYN","actual_qty":12.500,
                "stock_uom":"Nos","modified":"2026-11-04 10:00:00.000000"}"#,
        )
        .expect("row");
        let row = erpnext_row("Bin", &row, &crosswalk(System::Erpnext)).expect("shaped");
        assert_eq!(
            row.record.get("norn_ref").and_then(|v| v.as_str()),
            Some("demo/item/a/holding/erpnext/north")
        );
        let line = observation(System::Erpnext, "test", row, 1).expect("sealed");
        assert!(line.contains("12.5"), "{line}");
        assert!(line.contains(r#""contract":"norn.raw_observation/v1""#), "{line}");
    }

    #[test]
    fn src_live_1_a_refused_read_fails_with_the_sources_reason() {
        let (base, server) = serve(vec![
            (200, r#"{"access_token":"synthetic-token"}"#.to_owned()),
            (402, r#"{"errors":"Unavailable Shop"}"#.to_owned()),
        ]);
        let agent = agent();
        let token = shopify_token(&agent, &base, "id", "secret").expect("token");
        let error =
            pull_shopify(&agent, &base, &token, &crosswalk(System::Shopify)).expect_err("refused");
        assert_eq!(error, "source refused the read: HTTP 402 (Unavailable Shop)");
        server.join().expect("server");
    }

    #[test]
    fn src_read_1_shopify_sends_one_query_per_page_and_follows_the_cursor() {
        let first = PAGE.replacen(
            r#""pageInfo":{"hasNextPage":false},"nodes":[
      {"id":"gid://shopify/ProductVariant/1""#,
            r#""pageInfo":{"hasNextPage":true,"endCursor":"next"},"nodes":[
      {"id":"gid://shopify/ProductVariant/1""#,
            1,
        );
        let (base, server) = serve(vec![(200, first), (200, PAGE.to_owned())]);
        let rows = pull_shopify(&agent(), &base, "t", &crosswalk(System::Shopify)).expect("rows");
        assert_eq!(rows.len(), 6);
        let requests = server.join().expect("server");
        assert!(requests.iter().all(|r| r.starts_with("POST /admin/api/2026-10/graphql.json")));
        assert!(requests.iter().all(|r| !r.to_lowercase().contains("mutation")));
        assert!(requests[1].contains(r#""after":"next""#), "{}", requests[1]);
    }

    #[test]
    fn src_read_1_frappe_reads_only_its_declared_resources_with_get() {
        let artifact = include_str!("../../../fixtures/connectors/frappe.connector.json");
        let artifact: serde_json::Value = serde_json::from_str(artifact).expect("artifact");
        let connector: ConnectorRoot =
            serde_json::from_value(artifact["payload"].clone()).expect("connector");
        let item = r#"{"data":[{"name":"SYN-A","item_code":"SYN-A","item_name":"Synthetic A",
            "stock_uom":"Nos","modified":"2026-11-04 10:00:00.000000"}]}"#;
        let bin = r#"{"data":[{"name":"bin-1","item_code":"SYN-A","warehouse":"Stores - SYN",
            "actual_qty":7.0,"stock_uom":"Nos","modified":"2026-11-04 10:00:00.000000"}]}"#;
        let (base, server) = serve(vec![(200, item.to_owned()), (200, bin.to_owned())]);
        let rows = pull_frappe(&agent(), &base, "token k:s", &connector, &crosswalk(System::Erpnext))
            .expect("rows");
        assert_eq!(rows.iter().map(|r| r.dataset).collect::<Vec<_>>(), ["Item", "Bin"]);
        let requests = server.join().expect("server");
        assert!(requests[0].starts_with("GET /api/resource/Item?"), "{}", requests[0]);
        assert!(requests[1].starts_with("GET /api/resource/Bin?"), "{}", requests[1]);
    }

    #[test]
    fn credentials_never_travel_over_plain_http() {
        assert!(checked_base("http://example.test").is_err());
        assert!(checked_base("http://127.0.0.1.example.test").is_err());
        assert_eq!(checked_base("https://a.test/").as_deref(), Ok("https://a.test"));
    }
}
