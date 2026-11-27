//! #921: recorded responses of an `ERPNext` site and an Odoo database, pulled
//! by the generic engine from each fixture pack's `norn.pull/1` profile, land as
//! canonical objects through the cell's own mapping path (`DevHost::ingest`).
//!
//! The responses are each API's first pages, recorded from the real APIs over
//! synthetic rows (fixtures/connectors/rest); a request for a later page gets an
//! empty page.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unwrap_in_result,
    reason = "recorded REST scenarios"
)]
use norn_api::host::{DevHost, Ingested};
use norn_api::pull::{Limits, Pull, PullProfile, observation};
use norn_api::scope::CellScope;
use norn_api::sources::{ClaimError, HttpRequest, Sources, Step, Trigger};
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::{CellStore, InstalledDefinitionInput, MappingOutcome, Model};
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const SYSTEMS: [&str; 2] = ["erpnext", "odoo"];
const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
const EXTRACTED_AT_MS: u64 = 1_794_484_800_000;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn artifact(path: &str) -> ArtifactEnvelope {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
}

/// The three-systems catalogue with the REST fixture source schemas and mappings.
fn artifacts() -> Vec<ArtifactEnvelope> {
    let mut set: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts");
    for system in SYSTEMS {
        for kind in ["source", "mapping", "connector"] {
            set.push(artifact(&format!(
                "fixtures/packs/{system}-rest/artifacts/{system}.rest.{kind}.json"
            )));
        }
    }
    for name in [
        "source.Synced.interface",
        "source.Connection.entity",
        "source.SyncRun.entity",
        "source.Upload.entity",
        "norn.connect.source",
        "norn.connect.mapping",
    ] {
        set.push(artifact(&format!(
            "registry/packs/sources/artifacts/{name}.json"
        )));
    }
    set
}

fn host() -> DevHost<norn_cell::engine::NativeEngine> {
    host_with(&artifacts())
}

/// The scope every host here serves, unless a test names another.
fn cell_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

fn host_with(artifacts: &[ArtifactEnvelope]) -> DevHost<norn_cell::engine::NativeEngine> {
    host_in(artifacts, cell_scope())
}

fn host_in(
    artifacts: &[ArtifactEnvelope],
    scope: CellScope,
) -> DevHost<norn_cell::engine::NativeEngine> {
    let (base, candidate): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let values = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
        set.iter()
            .map(|a| {
                RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("value")
            })
            .collect()
    };
    let sources = ReceiptSources::new(&values(&candidate), &values(&base)).expect("sources");
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("checked");
    assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("entity root");
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                panic!("{} is outside this profile", artifact.artifact_id)
            }
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(artifacts)
        .expect("governing laws");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(artifacts).expect("set"))
                .expect("value")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("UTF-8"),
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256().expect("receipt digest"),
        admission_receipt: Some(
            String::from_utf8(checked.receipt.canonical_json().expect("receipt")).expect("UTF-8"),
        ),
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(model, input, Some(0), &scope.key())
        .expect("installed");
    DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host")
}

fn profile(system: &str) -> PullProfile {
    let connector = artifact(&format!(
        "fixtures/packs/{system}-rest/artifacts/{system}.rest.connector.json"
    ));
    let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
    PullProfile::from_connector(&payload)
        .expect("valid profile")
        .expect("a pull profile")
}

/// The recorded first page per dataset, then empty pages.
fn recorded(system: &str) -> BTreeMap<String, (u16, Value)> {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root().join(format!("fixtures/connectors/rest/{system}.responses.json")),
        )
        .expect("recorded responses"),
    )
    .expect("JSON");
    document["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|page| {
            (
                page["dataset"].as_str().expect("dataset").to_owned(),
                (
                    u16::try_from(page["status"].as_u64().expect("status")).expect("status"),
                    page["body"].clone(),
                ),
            )
        })
        .collect()
}

fn empty_page(profile: &PullProfile, dataset: &str) -> Value {
    let feed = profile
        .feeds
        .iter()
        .find(|f| f.dataset == dataset)
        .expect("feed");
    let mut body = json!({});
    let mut at = &mut body;
    let parts: Vec<&str> = feed.items_path.trim_start_matches('/').split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        at[*part] = if i + 1 == parts.len() {
            json!([])
        } else {
            json!({})
        };
        at = &mut at[*part];
    }
    body
}

/// Pull every feed of `system` from its recordings; the rows as cell inputs.
fn pull(
    system: &str,
    watermarks: BTreeMap<String, norn_api::pull::Watermark>,
) -> (
    Vec<(norn_kernel::Observed<RuntimeValue>, MappingInput)>,
    BTreeMap<String, norn_api::pull::Watermark>,
) {
    let profile = profile(system);
    let mut pages = recorded(system);
    let mut pull = Pull::new(profile.clone(), watermarks, Limits::default());
    let mut rows = Vec::new();
    while let Some(request) = pull.next_request() {
        let (status, body) = pages
            .remove(&request.dataset)
            .unwrap_or_else(|| (200, empty_page(&profile, &request.dataset)));
        let page = pull
            .accept(status, body.to_string().as_bytes())
            .expect("a page in the declared shape");
        for row in page.rows {
            let raw = observation(&profile, &row, EXTRACTED_AT_MS).expect("sealed");
            rows.push((
                raw,
                MappingInput {
                    mapping: format!("{system}.rest.mapping"),
                    dataset: row.dataset.clone(),
                    connector_instance: "sandbox".into(),
                    partition: row.dataset,
                    recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
                },
            ));
        }
        pull.confirm(&page.advance);
    }
    (rows, pull.watermarks().clone())
}

fn count(outcomes: &[Ingested], wanted: &MappingOutcome) -> usize {
    outcomes.iter().filter(|o| &o.outcome == wanted).count()
}

#[test]
fn rest_rows_land_through_the_mapping_path_once() {
    let host = host();
    for system in SYSTEMS {
        let (rows, watermarks) = pull(system, BTreeMap::new());
        assert_ne!(rows, [], "{system} pulled nothing");
        let outcomes = host.ingest(&rows).expect("ingested");
        let quarantined: Vec<_> = outcomes
            .iter()
            .filter(|o| matches!(o.outcome, MappingOutcome::Quarantined(_)))
            .collect();
        assert_eq!(quarantined.len(), 0, "{system}: {quarantined:#?}");
        assert_eq!(
            count(&outcomes, &MappingOutcome::Landed),
            rows.len(),
            "{system}"
        );
        // The same source events again are the same observations: nothing lands twice.
        let again = host.ingest(&rows).expect("re-ingested");
        assert_eq!(
            count(&again, &MappingOutcome::AlreadyLanded),
            rows.len(),
            "{system}"
        );
        // Every feed's watermark is its last row's position.
        let profile = profile(system);
        assert_eq!(
            watermarks.keys().collect::<BTreeSet<_>>(),
            profile
                .feeds
                .iter()
                .map(|f| &f.dataset)
                .collect::<BTreeSet<_>>()
        );
    }
}

#[test]
fn a_row_without_a_canonical_reference_is_quarantined_with_its_reason() {
    let host = host();
    let (mut rows, _) = pull("odoo", BTreeMap::new());
    let (raw, input) = rows
        .iter_mut()
        .find(|(_, input)| input.dataset == "product.product")
        .expect("a product row");
    let RuntimeValue::Object(object) = &mut raw.input else {
        panic!("a raw observation is an object")
    };
    let mut payload = object["payload"].clone();
    let RuntimeValue::Object(fields) = &mut payload else {
        panic!("the payload is an object")
    };
    fields.insert("x_norn_ref".into(), RuntimeValue::Null);
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&payload).expect("payload"))
        .expect("digest");
    object.insert("payload".into(), payload);
    object.insert("payload_digest".into(), RuntimeValue::String(digest));
    object.insert("event_id".into(), RuntimeValue::String("edited".into()));
    let one = vec![(raw.clone(), input.clone())];
    let outcome = host.ingest(&one).expect("captured");
    assert!(
        matches!(&outcome[0].outcome, MappingOutcome::Quarantined(codes) if !codes.is_empty()),
        "{outcome:#?}"
    );
}

#[test]
fn rest_mappings_pin_their_source_schemas() {
    let pairs = [
        (
            "fixtures/packs/erpnext-rest",
            "erpnext.rest",
            "fixtures/packs/erpnext-rest/rest.ts",
        ),
        (
            "fixtures/packs/odoo-rest",
            "odoo.rest",
            "fixtures/packs/odoo-rest/rest.ts",
        ),
        (
            "registry/packs/sources",
            "norn.connect",
            "registry/packs/sources/records.ts",
        ),
    ];
    let mut wrong = Vec::new();
    for (pack, stem, typed) in pairs {
        let source = artifact(&format!("{pack}/artifacts/{stem}.source.json"));
        let mapping = artifact(&format!("{pack}/artifacts/{stem}.mapping.json"));
        let pin = norn_checker::ArtifactRef::of(&source).expect("pin").sha256;
        let payload = serde_json::to_value(mapping.payload.to_json_value()).expect("payload");
        if payload["source"]["sha256"].as_str() != Some(pin.as_str()) {
            wrong.push(format!("PIN {stem}.source {pin} {typed}"));
        }
    }
    assert_eq!(wrong.len(), 0, "{}", wrong.join("\n"));
}

const CONFIG: &str = r#"[
  {"id": "erpnext.rest", "system": "erpnext", "label": "ERPNext REST", "connector": "erpnext.rest.connector",
   "mapping": "erpnext.rest.mapping", "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"],
   "headers": {"X-Frappe-Site-Name": "frontend"}},
  {"id": "odoo.rest", "system": "odoo", "label": "Odoo REST", "connector": "odoo.rest.connector",
   "mapping": "odoo.rest.mapping", "instance": "sandbox", "baseUrl": "http://odoo-rest:8069",
   "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["odoo_rest_api_key"]}
]"#;

/// The sandbox's single-label hosts, which a deployment admits by listing
/// them (SYNC-URL-1, `NORN_SOURCE_ALLOWED_HOSTS`).
const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest";

fn secrets() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        ),
        (
            "NORN_SECRET_ODOO_REST_API_KEY".to_owned(),
            "odoo-key".to_owned(),
        ),
    ])
}

fn sources() -> Sources {
    sources_in(&cell_scope(), CONFIG)
}

fn sources_in(scope: &CellScope, config: &str) -> Sources {
    Sources::with_allowed_hosts(scope, config, &secrets(), SANDBOX_HOSTS)
        .unwrap_or_else(|error| panic!("configuration: {error}"))
}

/// Run one claimed sync of `id` against the recordings: each feed's first
/// request gets its recorded page (none when `replay` is false, as a source
/// with nothing new after the watermark answers), later ones an empty page.
fn sync(
    host: &DevHost<norn_cell::engine::NativeEngine>,
    sources: &mut Sources,
    id: &str,
    (system, replay): (&str, bool),
    now_ms: u64,
) -> (norn_api::sources::RunSummary, Vec<HttpRequest>) {
    let profile = profile(system);
    let mut pages = if replay {
        recorded(system)
    } else {
        BTreeMap::new()
    };
    let mut sent = Vec::new();
    let mut step = sources.step(host, id, None, now_ms);
    loop {
        match step {
            Step::Done(summary) => return (summary, sent),
            // SYNC-YIELD-1: rows map a page at a time between the host's turns.
            Step::Continue => step = sources.step(host, id, None, now_ms),
            Step::Request(request) => {
                let feed = profile
                    .feeds
                    .iter()
                    .find(|f| request.url.contains(f.request.path.as_str()))
                    .expect("a declared feed");
                let (status, body) = pages
                    .remove(&feed.dataset)
                    .unwrap_or_else(|| (200, empty_page(&profile, &feed.dataset)));
                sent.push(request);
                step = sources.step(
                    host,
                    id,
                    Some((status, body.to_string().as_bytes())),
                    now_ms,
                );
            }
        }
    }
}

#[test]
fn a_manual_sync_lands_rows_and_reports_the_connection_as_objects() {
    let host = host();
    let mut sources = sources();
    sources
        .claim(
            "erpnext.rest",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_800_000,
        )
        .expect("claimed");
    // SYNC-ONE-1: a second claim while it runs is refused.
    assert_eq!(
        sources.claim(
            "erpnext.rest",
            Trigger::Manual,
            "user:other@demo",
            1_794_484_800_001
        ),
        Err(ClaimError::Running)
    );
    let (summary, sent) = sync(
        &host,
        &mut sources,
        "erpnext.rest",
        ("erpnext", true),
        1_794_484_800_000,
    );
    assert_eq!(summary.outcome, "ok", "{summary:#?}");
    assert!(summary.rows_read > 0);
    assert_eq!(summary.rows_landed, summary.rows_read);
    // Credentials and the site header ride every request; the URL is the base plus the planned path.
    for request in &sent {
        assert!(
            request
                .url
                .starts_with("http://erpnext-rest:8000/api/resource/")
        );
        assert!(
            request
                .headers
                .contains(&("Authorization".into(), "token key:secret".into()))
        );
        assert!(
            request
                .headers
                .contains(&("X-Frappe-Site-Name".into(), "frontend".into()))
        );
    }
    let connection = host
        .host_object("source.Connection", "erpnext.rest")
        .expect("read")
        .expect("the connection object");
    assert_eq!(
        connection.get("status").and_then(RuntimeValue::as_str),
        Some("ok")
    );
    let watermark: BTreeMap<String, Value> = serde_json::from_str(
        connection
            .get("watermark")
            .and_then(RuntimeValue::as_str)
            .expect("watermark"),
    )
    .expect("watermark JSON");
    assert_eq!(watermark.len(), 4);
    let run = host
        .host_object("source.SyncRun", &summary.run)
        .expect("read")
        .expect("the run object");
    assert_eq!(
        run.get("run_by").and_then(RuntimeValue::as_str),
        Some("user:owner@demo")
    );
    assert_eq!(
        run.get("trigger").and_then(RuntimeValue::as_str),
        Some("manual")
    );
    // The next sync starts from the recorded watermark, read back from the cell.
    sources
        .claim(
            "erpnext.rest",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_900_000,
        )
        .expect("claimed again");
    let (again, sent) = sync(
        &host,
        &mut sources,
        "erpnext.rest",
        ("erpnext", false),
        1_794_484_900_000,
    );
    assert_eq!(again.outcome, "ok");
    let party = watermark["Party"]["version"]
        .as_str()
        .expect("version")
        .replace(' ', "%20");
    assert!(
        sent[0].url.contains(&party.replace(':', "%3A")),
        "{}",
        sent[0].url
    );
}

#[test]
fn the_schedule_claims_each_due_connection_once_as_its_own_principal() {
    let host = host();
    let mut sources = sources();
    let t0 = 1_794_484_800_000;
    let first = sources.due(t0).expect("a never-synced connection is due");
    // SYNC-SERIAL-1: one sync per cell, so the other waits for this one.
    assert_eq!(sources.due(t0), None, "one sync runs at a time");
    let mut synced = Vec::new();
    let mut next = Some(first);
    while let Some(id) = next {
        let system = id[..id.find('.').expect("dotted")].to_owned();
        let (summary, _) = sync(&host, &mut sources, &id, (&system, true), t0);
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        let run = host
            .host_object("source.SyncRun", &summary.run)
            .expect("read")
            .expect("run");
        assert_eq!(
            run.get("run_by").and_then(RuntimeValue::as_str),
            Some(format!("connection:{id}").as_str())
        );
        assert_eq!(
            run.get("trigger").and_then(RuntimeValue::as_str),
            Some("schedule")
        );
        synced.push(id);
        next = sources.due(t0);
    }
    assert_eq!(synced.len(), 2, "each due connection ran once: {synced:?}");
    assert_eq!(
        sources.due(t0 + 60_000),
        None,
        "not due again within 15 minutes"
    );
    assert!(sources.due(t0 + 15 * 60_000).is_some());
}

/// The error a step that ends at once fails with.
fn failed_at_once(
    host: &DevHost<norn_cell::engine::NativeEngine>,
    sources: &mut Sources,
    id: &str,
) -> String {
    match sources.step(host, id, None, EXTRACTED_AT_MS) {
        Step::Done(summary) => {
            assert_eq!(summary.outcome, "failed", "{summary:#?}");
            summary.error.map_or_else(String::new, |(code, _)| code)
        }
        other @ (Step::Request(_) | Step::Continue) => {
            panic!("expected a failed run, got {other:?}")
        }
    }
}

/// SOURCE-SCOPE-1 (#1702): a second cell, in another org or another
/// environment, with the same connector and mapping definitions, the same
/// server configuration and the same secrets, never reaches the first cell's
/// connections or credentials: a manual claim finds no such connection, the
/// schedule claims none, and a connection the operator gives it under the same
/// connector never resolves the first cell's secret. A connection stepped in
/// a cell it does not belong to fails before any request, manual or
/// scheduled, and one secret bound in two scopes refuses the configuration.
#[test]
fn another_cell_never_syncs_with_this_cells_connection_or_secret() -> Result<(), String> {
    let theirs = |other: &CellScope, refs: &[&str]| {
        let mut config: Vec<Value> =
            serde_json::from_str(CONFIG).map_err(|error| error.to_string())?;
        config.push(
            json!({"id": "erpnext.other", "system": "erpnext", "label": "Theirs",
            "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
            "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
            "orgId": other.org_id, "environmentId": other.environment_id, "secretRefs": refs}),
        );
        Ok::<String, String>(Value::Array(config).to_string())
    };
    let t0 = EXTRACTED_AT_MS;
    for other in [
        CellScope {
            org_id: "org_other".into(),
            environment_id: "dev".into(),
        },
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "prod".into(),
        },
    ] {
        let config = theirs(&other, &[])?;
        let host = host_in(&artifacts(), other.clone());
        // Manual: the first cell's connection is unknown here; their own
        // connection resolves no secret it does not bind.
        let mut manual = sources_in(&other, &config);
        let ids: Vec<&str> = manual.connections().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["erpnext.other"]);
        assert_eq!(
            manual.claim("erpnext.rest", Trigger::Manual, "user:admin@other", t0),
            Err(ClaimError::Unknown)
        );
        manual
            .claim("erpnext.other", Trigger::Manual, "user:admin@other", t0)
            .map_err(|error| format!("their own connection: {error:?}"))?;
        assert_eq!(
            failed_at_once(&host, &mut manual, "erpnext.other"),
            "source.secret_unbound"
        );
        // Scheduled: only their own connection is ever due, and it fails the same way.
        let mut scheduled = sources_in(&other, &config);
        assert_eq!(scheduled.due(t0).as_deref(), Some("erpnext.other"));
        assert_eq!(
            failed_at_once(&host, &mut scheduled, "erpnext.other"),
            "source.secret_unbound"
        );
        assert_eq!(scheduled.due(t0), None, "nothing else is theirs to sync");
        // The first cell's connection, stepped in their cell, never starts.
        let mut mine = sources_in(&cell_scope(), &config);
        mine.claim("erpnext.rest", Trigger::Manual, "user:owner@demo", t0)
            .map_err(|error| format!("claimed: {error:?}"))?;
        assert_eq!(
            failed_at_once(&host, &mut mine, "erpnext.rest"),
            "source.scope_mismatch"
        );
        let mut mine = sources_in(&cell_scope(), &config);
        let due = mine.due(t0).ok_or("nothing due")?;
        assert_eq!(
            failed_at_once(&host, &mut mine, &due),
            "source.scope_mismatch"
        );
        // One secret bound by connections of two scopes is refused outright.
        let shared = theirs(&other, &["erpnext-rest-token"])?;
        let refused = Sources::with_allowed_hosts(&other, &shared, &secrets(), SANDBOX_HOSTS)
            .err()
            .ok_or("a secret in two scopes was accepted")?;
        assert!(
            refused.contains("NORN_SECRET_ERPNEXT_REST_TOKEN"),
            "{refused}"
        );
    }
    Ok(())
}

#[test]
fn a_refused_read_fails_the_run_and_says_why() {
    let host = host();
    let mut sources = sources();
    sources
        .claim(
            "odoo.rest",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_800_000,
        )
        .expect("claimed");
    let Step::Request(_) = sources.step(&host, "odoo.rest", None, 1_794_484_800_000) else {
        panic!("a first request")
    };
    let Step::Done(summary) =
        sources.step(&host, "odoo.rest", Some((401, b"{}")), 1_794_484_800_000)
    else {
        panic!("the run ends")
    };
    assert_eq!(summary.outcome, "failed");
    assert_eq!(
        summary.error.as_ref().map(|e| e.0.as_str()),
        Some("source.http_status")
    );
    let connection = host
        .host_object("source.Connection", "odoo.rest")
        .expect("read")
        .expect("connection");
    assert_eq!(
        connection.get("status").and_then(RuntimeValue::as_str),
        Some("failed")
    );
    assert!(!sources.running("odoo.rest"));
}

/// UPLOAD-MEASURE-1 and UPLOAD-KEY-1 through admission: a type with
/// additive measures and a unique key installs (as `:define` does) and its
/// rows land.
#[test]
fn an_uploaded_type_with_measures_and_a_key_is_admitted() {
    use norn_api::uploads::{
        ColumnChoice, ColumnKind, LoadRequest, generate, install, load_rows, parse_csv, sha256_hex,
    };
    let host = host();
    let csv = "SKU,Units,Price\nA-1,3,1.50\nA-2,4,2.25\n";
    let uri = "s3://uploads/demo/stock.csv";
    let table = parse_csv(csv).expect("parsed");
    let sha = sha256_hex(csv.as_bytes());
    let measure = |id: &str, kind| ColumnChoice {
        id: id.into(),
        kind,
        include: true,
        measure: true,
    };
    let request = LoadRequest {
        type_name: "Stock".into(),
        columns: vec![
            measure("units", ColumnKind::Integer),
            measure("price", ColumnKind::Decimal),
        ],
        key: vec!["sku".into()],
        sha256: None,
    };
    let generated =
        generate(&table, &request, uri, &sha, "2026-11-12T12:00:00.000Z").expect("generated");
    let version = install(&host, &generated, 2).expect("installed");
    assert!(version > 1);
    let loaded = load_rows(&host, &generated, &table, uri, &sha, EXTRACTED_AT_MS).expect("loaded");
    assert_eq!(
        (loaded.rows_landed, loaded.rows_quarantined),
        (2, 0),
        "{loaded:#?}"
    );
}

/// UPLOAD-NULL-1 end to end: an empty cell lands as null on a nullable
/// property (its source field `required: false`), every row once, with
/// lineage to the object and row.
#[test]
fn an_uploaded_csv_installs_its_type_and_loads_every_row() {
    use norn_api::uploads::{LoadRequest, generate, install, load_rows, parse_csv, sha256_hex};
    let host = host();
    let csv = "Region,Units,Price,Shipped at\nnorth,3,1.50,2026-01-01T00:00:00Z\nsouth,,2,\n";
    let uri = "s3://uploads/demo/sales.csv";
    let table = parse_csv(csv).expect("parsed");
    let sha = sha256_hex(csv.as_bytes());
    let request = LoadRequest {
        type_name: "Sales".into(),
        columns: vec![],
        key: vec![],
        sha256: None,
    };
    let generated =
        generate(&table, &request, uri, &sha, "2026-11-12T12:00:00.000Z").expect("generated");
    let version = install(&host, &generated, 2).expect("installed");
    assert!(version > 1);
    let loaded = load_rows(&host, &generated, &table, uri, &sha, EXTRACTED_AT_MS).expect("loaded");
    assert_eq!(
        (
            loaded.rows_read,
            loaded.rows_landed,
            loaded.rows_quarantined
        ),
        (2, 2, 0),
        "{loaded:#?}"
    );
    let second = host
        .host_object("upload.Sales", &format!("{uri}#row=2"))
        .expect("read")
        .expect("row 2");
    assert_eq!(second.get("units"), Some(&RuntimeValue::Null));
    assert_eq!(second.get("shipped_at"), Some(&RuntimeValue::Null));
    assert_eq!(
        second.get("region").and_then(RuntimeValue::as_str),
        Some("south")
    );
    // Loading the same object again lands nothing new.
    let again = load_rows(&host, &generated, &table, uri, &sha, EXTRACTED_AT_MS).expect("reloaded");
    assert_eq!(again.rows_landed, 2);
    // The REST fixture mappings keep working on the extended definitions.
    let (rows, _) = pull("odoo", BTreeMap::new());
    assert!(
        host.ingest(&rows)
            .expect("ingested")
            .iter()
            .all(|o| !matches!(o.outcome, MappingOutcome::Quarantined(_)))
    );
}

/// SYNC-INSTANCE-1 and SYNC-URL-1: configuration the cell refuses before
/// any sync, a connection claiming the cell's own instance or a plain-HTTP
/// public base URL.
#[test]
fn configuration_refuses_the_cells_own_instance_and_a_public_http_base() {
    let one = |instance: &str, base: &str| {
        format!(
            r#"[{{"id": "x", "system": "erpnext", "label": "X", "connector": "c", "mapping": "m", "instance": "{instance}", "baseUrl": "{base}", "orgId": "org_demo", "environmentId": "dev", "secretRefs": []}}]"#
        )
    };
    let refused = |config: String| {
        Sources::new(&cell_scope(), &config, &BTreeMap::new())
            .err()
            .unwrap_or_else(|| panic!("accepted: {config}"))
    };
    assert!(refused(one("cell", "https://erp.example.test")).contains("the cell's own"));
    assert!(refused(one("sandbox", "http://erp.example.test")).contains("base URL"));
    assert!(
        Sources::new(
            &cell_scope(),
            &one("sandbox", "https://erp.example.test"),
            &BTreeMap::new()
        )
        .is_ok()
    );
    // An internal host is refused unless the operator lists it.
    assert!(refused(one("sandbox", "http://erpnext-rest:8000")).contains("base URL"));
    assert!(refused(one("sandbox", "https://169.254.169.254")).contains("base URL"));
    assert!(
        Sources::with_allowed_hosts(
            &cell_scope(),
            &one("sandbox", "http://erpnext-rest:8000"),
            &BTreeMap::new(),
            SANDBOX_HOSTS
        )
        .is_ok()
    );
}

/// The scanned-document extractor as a pull source: its recorded page
/// (`POST /extractions`) pulls through `docs.scans.connector` into Scanned
/// Document rows carrying exactly the fields `docs.scans.source` declares,
/// lines and evidence nested, and a later sync asks for rows after the
/// recorded (version, id).
#[test]
fn the_scans_connector_pulls_extractor_rows_into_the_source_schema_fields() {
    let connector = artifact("fixtures/packs/scans-pull/artifacts/docs.scans.connector.json");
    let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
    let profile = PullProfile::from_connector(&payload)
        .expect("valid profile")
        .expect("a pull profile");
    assert_eq!(profile.source, "synthetic-scans");
    let schema = artifact("fixtures/packs/scans/artifacts/docs.scans.source.json");
    let schema = serde_json::to_value(schema.payload.to_json_value()).expect("schema");
    assert_eq!(schema["source"]["system"], "synthetic-scans");
    let declared: BTreeSet<&str> = schema["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .filter(|f| f["dataset"] == "Scanned Document")
        .map(|f| f["name"].as_str().expect("name"))
        .collect();
    let feed = &profile.feeds[0];
    assert_eq!(
        feed.fields
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        declared
    );

    let page = std::fs::read(root().join("fixtures/connectors/scans/extractions.page.json"))
        .expect("recorded page");
    let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
    let first = pull.next_request().expect("a first request");
    assert_eq!(
        (first.method, first.path.as_str()),
        ("POST", "/extractions")
    );
    let body: Value = serde_json::from_str(first.body.as_deref().expect("a body")).expect("JSON");
    assert_eq!(body, json!({"after": ["", ""], "limit": 100}));
    let accepted = pull
        .accept(200, &page)
        .expect("a page in the declared shape");
    let rows = &accepted.rows;
    assert_eq!(rows.len(), 10);
    let nested = |row: &norn_api::pull::Row, field: &str| {
        row.payload
            .get(field)
            .and_then(RuntimeValue::as_array)
            .map_or_else(
                || panic!("{} has no {field} array", row.source_id),
                Vec::len,
            )
    };
    for row in rows {
        assert_eq!(row.dataset, "Scanned Document");
        assert!(row.source_id.starts_with("SCAN-"), "{}", row.source_id);
    }
    // Lines and evidence stay nested arrays (a report may print no lines).
    assert!(rows.iter().map(|r| nested(r, "lines")).sum::<usize>() > 0);
    assert!(rows.iter().all(|r| nested(r, "evidence") > 0));
    // A short page ends the feed; the next sync resumes after its last row.
    assert!(pull.next_request().is_none());
    pull.confirm(&accepted.advance);
    let mut resumed = Pull::new(profile, pull.watermarks().clone(), Limits::default());
    let next = resumed.next_request().expect("a resumed request");
    let body: Value = serde_json::from_str(next.body.as_deref().expect("a body")).expect("JSON");
    let last = rows.last().expect("rows");
    assert_eq!(
        body,
        json!({"after": [last.version, last.source_id], "limit": 100})
    );
}

/// Admits every `SyncSource` for `loader`; `uploader` holds only the
/// uploads grant, not a destination connection's.
struct UploadsOnly;

impl norn_api::authz::Authorizer for UploadsOnly {
    fn authorize(
        &self,
        request: &norn_api::authz::AccessRequest<'_>,
    ) -> Result<(), norn_api::refusal::Refusal> {
        match (&request.operation, request.actor) {
            (norn_api::authz::Operation::SyncSource(_), Some("loader")) => Ok(()),
            (norn_api::authz::Operation::SyncSource(target), Some("uploader"))
                if target == "upload" =>
            {
                Ok(())
            }
            _ => Err(norn_api::authz::read_denied()),
        }
    }
}

/// The completed uploads `u1` and `u2`.
struct OneUpload;

impl norn_api::analytics_routes::Uploads for OneUpload {
    fn record(&self, id: &str) -> Option<norn_api::analytics_routes::UploadRecord> {
        matches!(id, "u1" | "u2").then(|| norn_api::analytics_routes::UploadRecord {
            uri: format!("s3://uploads/demo/{id}.csv"),
            sha256: if id == "u1" { "ab" } else { "ba" }.repeat(32),
            content_type: "text/csv".into(),
            file_name: "items.csv".into(),
        })
    }
    fn issued(&self, id: &str, _scope_key: &str, uri: &str) -> bool {
        self.record(id).is_some_and(|record| record.uri == uri)
    }
    fn presigned_get(&self, uri: &str, _seconds: u64, _now_ms: u64) -> Option<String> {
        Some(format!("https://objects.example/{}", &uri["s3://".len()..]))
    }
    fn presigned_put(&self, key: &str, _seconds: u64, _now_ms: u64) -> Option<(String, String)> {
        Some((
            format!("https://objects.example/put/{key}"),
            format!("s3://uploads/{key}"),
        ))
    }
}

/// An engine that types three rows of every load and quarantines one, and
/// stores the files it lands; `between` runs between the landing and the
/// store (a change of the definitions while the files are in flight).
#[derive(Default)]
struct ThreeAndOne {
    landed: std::sync::Mutex<Vec<norn_cell::analytics::LandRequest>>,
    stored: std::sync::Mutex<Vec<norn_cell::analytics::StoreRequest>>,
    between: Option<Box<dyn Fn() + Send + Sync>>,
}

impl norn_api::analytics_routes::Transport for ThreeAndOne {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        norn_cell::analytics::EngineCapabilities::DUCKDB
    }
    fn resumed(&self) -> Option<norn_api::analytics_routes::EngineAnswer> {
        None
    }
    fn call(
        &self,
        call: &norn_api::analytics_routes::EngineCall,
    ) -> Option<norn_api::analytics_routes::EngineAnswer> {
        let answer = match call.op.as_str() {
            "land" => {
                let request: norn_cell::analytics::LandRequest =
                    serde_json::from_str(&call.body).expect("a landing");
                let file = |rows: u64, byte: &str| norn_cell::analytics::LandedFile {
                    sha256: byte.repeat(32),
                    rows,
                    bytes: 128,
                };
                let answer = norn_cell::analytics::LandResponse {
                    landing: request.landing.clone(),
                    // OLAP-02: the engine mints the attempt.
                    attempt: "aMinted".to_owned(),
                    load: norn_cell::analytics::LoadResponse {
                        load: request.load.load.clone(),
                        rows: 3,
                        rejected: 1,
                        reasons: BTreeMap::from([("bulk.type".to_owned(), 1)]),
                        bytes: 64,
                        millis: 5,
                    },
                    data: Some(file(3, "cd")),
                    held: Some(file(1, "ef")),
                    valid_time: Vec::new(),
                    findings: BTreeMap::new(),
                    held_by_rules: 0,
                    residue_total: 0,
                    residue: Vec::new(),
                    stored: None,
                };
                self.landed.lock().expect("calls").push(request);
                if let Some(between) = &self.between {
                    between();
                }
                answer
            }
            "store" => {
                let request: norn_cell::analytics::StoreRequest =
                    serde_json::from_str(&call.body).expect("a store");
                for (object, file) in [
                    (&request.data, request.landed.data.as_ref().expect("data")),
                    (&request.held, request.landed.held.as_ref().expect("held")),
                ] {
                    assert!(
                        object.uri.ends_with(&format!("/{}.parquet", file.sha256)),
                        "an object is named by its digest: {}",
                        object.uri
                    );
                }
                let cache = norn_cell::analytics::dataset_cache_id(
                    &request.land.load.table,
                    &request.landed.data.as_ref().expect("data").sha256,
                    &request.landed.held.as_ref().expect("held").sha256,
                );
                let answer = norn_cell::analytics::LandResponse {
                    stored: Some(cache),
                    ..request.landed.clone()
                };
                self.stored.lock().expect("calls").push(request);
                answer
            }
            other => panic!("unexpected engine call {other}"),
        };
        Some(norn_api::analytics_routes::EngineAnswer {
            op: call.op.clone(),
            digest: call.digest.clone(),
            status: 200,
            body: serde_json::to_string(&answer).expect("answer"),
        })
    }
}

/// The upload type `upload.Items` and its admitted mapping, as `:define`
/// installs them for a CSV of `sku` and `qty`; `marked` marks the source
/// field `sku`.
fn items_host(marked: bool) -> DevHost<norn_cell::engine::NativeEngine> {
    let host = host().with_authorizer(UploadsOnly);
    let table = norn_api::uploads::parse_csv("sku,qty\nA,1\nB,2\n").expect("csv");
    let request: norn_api::uploads::LoadRequest =
        serde_json::from_value(json!({"typeName": "Items"})).expect("request");
    let mut generated = norn_api::uploads::generate(
        &table,
        &request,
        "s3://uploads/demo/u1.csv",
        &"ab".repeat(32),
        RECORDED_AT,
    )
    .expect("generated");
    if marked {
        // The `sku` field carries a declared marking; the mapping is bound
        // again to the marked source schema's own hash.
        let mut source_hash = None;
        let mut scope = Value::Null;
        for artifact in &mut generated.artifacts {
            if artifact.artifact_id != generated.source_schema {
                continue;
            }
            let mut value = serde_json::to_value(&*artifact).expect("value");
            for field in value["payload"]["fields"].as_array_mut().expect("fields") {
                if field["name"] == "sku" {
                    field["markings"] = json!(["pii.contact"]);
                }
            }
            scope = value["scope"].clone();
            *artifact = serde_json::from_value(value).expect("artifact");
            source_hash = Some(
                norn_checker::ArtifactRef::of(artifact)
                    .expect("hash")
                    .sha256,
            );
        }
        let source_hash = source_hash.expect("the source schema");
        for artifact in &mut generated.artifacts {
            let mut value = serde_json::to_value(&*artifact).expect("value");
            if value["kind"] == "mapping" {
                value["payload"]["source"]["sha256"] = json!(source_hash);
                *artifact = serde_json::from_value(value).expect("artifact");
            }
        }
        generated.artifacts.push(
            serde_json::from_value(json!({
                "irVersion": "0.1", "artifactId": "upload.markings.entity", "kind": "entity",
                "namespace": "upload", "name": "Markings", "version": "0.1.0", "lifecycle": "draft",
                "scope": scope, "dependencies": [], "provenance": {"source": "authoring_sdk"},
                "payload": {"entityVersion": "2.0", "markings": [{
                    "id": "pii.contact", "label": "Contact", "severity": "high",
                    "category": "pii.contact", "mask": {"kind": "redacted"}
                }]}
            }))
            .expect("markings"),
        );
    }
    norn_api::uploads::install(&host, &generated, EXTRACTED_AT_MS).expect("installed");
    host
}

fn scope() -> norn_api::scope::ScopeQuery {
    norn_api::scope::ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

fn items(upload: &str, key: Option<&str>) -> String {
    let mut body = json!({"uploadId": upload, "objectType": "upload.Items"});
    if let Some(key) = key {
        body["requestKey"] = json!(key);
    }
    body.to_string()
}

/// MANIFEST-COMMIT-1 / MANIFEST-CURRENT-1 / MANIFEST-CACHE-1: a dataset load
/// lands two files, stores each at the object its digest names, and commits
/// one manifest whose cache identity is made of those digests; the manifest
/// becomes the type's current dataset.
#[test]
fn a_dataset_load_commits_one_manifest_of_its_stored_files() {
    use norn_api::analytics_routes::load_dataset;
    let host = items_host(false);
    let engine = ThreeAndOne::default();
    let loaded = load_dataset(
        &host,
        &scope(),
        Some("loader"),
        &items("u1", None),
        &OneUpload,
        &engine,
        EXTRACTED_AT_MS,
    )
    .expect("loaded");
    let manifest = norn_api::analytics_routes::current_manifest(&host, "upload.Items")
        .expect("read")
        .expect("the current manifest");
    assert_eq!(manifest.hash, loaded.manifest);
    assert_eq!(manifest.commit_seq, loaded.commit_seq);
    assert_eq!(
        manifest.manifest.data.uri,
        format!(
            "s3://uploads/datasets/{}/upload.Items/{}.parquet",
            host.scope().key(),
            "cd".repeat(32)
        )
    );
    assert_eq!(
        manifest.manifest.load,
        norn_cell::analytics::dataset_cache_id(
            &manifest.manifest.table,
            &"cd".repeat(32),
            &"ef".repeat(32)
        ),
        "the cache identity is the committed digests'"
    );
    assert_eq!(
        (manifest.manifest.data.rows, manifest.manifest.held.rows),
        (3, 1)
    );
    let landed = engine.landed.lock().expect("calls");
    let stored = engine.stored.lock().expect("calls");
    assert_eq!((landed.len(), stored.len()), (1, 1));
    assert_eq!(
        manifest.manifest.pins, landed[0].pins,
        "the manifest commits the pins the landing ran by"
    );
    assert_eq!(
        (landed[0].attempt.as_str(), stored[0].land.attempt.as_str()),
        ("", "aMinted"),
        "a landing names no attempt; the store takes the one the engine minted"
    );
}

/// MANIFEST-01: a law installed while a landing is in flight refuses its
/// commit as stale: the landing was checked by the laws pinned at its first
/// step, and the commit takes those pins, never the definitions read again.
#[test]
fn a_law_installed_between_landing_and_commit_refuses_the_commit() {
    use norn_api::analytics_routes::load_dataset;
    let host = std::sync::Arc::new(items_host(false));
    let installer = std::sync::Arc::clone(&host);
    let engine = ThreeAndOne {
        between: Some(Box::new(move || {
            let law: ArtifactEnvelope = serde_json::from_value(json!({
                "artifactId": "upload.items.qty_nonnegative.law", "kind": "law",
                "dependencies": [{"artifactId": "upload.Items.entity", "kind": "entity", "required": true, "version": "0.1.0"}],
                "irVersion": "0.1", "lifecycle": "active", "name": "qty_nonnegative", "namespace": "upload",
                "provenance": {"source": "system"}, "scope": {}, "version": "0.1.0",
                "payload": {"id": "qty_nonnegative", "lawVersion": "0.1", "modality": {"kind": "always"},
                    "owner": "upload", "provenance": {"source": "system"}, "rationale": "qty_nonnegative",
                    "response": {"kind": "predicate", "expression": "self.qty >= 0"},
                    "scope": {"objectTypes": ["upload.Items"], "root": {"kind": "entity", "id": "upload.Items"}},
                    "severity": "error", "subject": "upload.Items"}
            }))
            .expect("law");
            installer
                .install_artifacts(&[law], EXTRACTED_AT_MS + 1)
                .expect("a law installed in flight");
        })),
        ..ThreeAndOne::default()
    };
    let refused = load_dataset(
        &host,
        &scope(),
        Some("loader"),
        &items("u1", None),
        &OneUpload,
        &engine,
        EXTRACTED_AT_MS,
    )
    .expect_err("stale");
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (409, "dataset.stale_pins")
    );
    assert_eq!(
        engine.landed.lock().expect("calls")[0].checks,
        [],
        "the landing was checked by no law"
    );
    assert!(
        norn_api::analytics_routes::current_manifest(&host, "upload.Items")
            .expect("read")
            .is_none(),
        "nothing was admitted"
    );
}

/// The admitted Items mapping, renamed to its `revision`: an edit of the
/// mapping artifact that changes its pin and nothing it reads.
fn renamed_items_mapping(
    host: &DevHost<norn_cell::engine::NativeEngine>,
    revision: u64,
) -> ArtifactEnvelope {
    let active = host.active_definitions().expect("read").expect("installed");
    let set: Vec<Value> = serde_json::from_str(&active.canonical_set).expect("set");
    let mut mapping = set
        .into_iter()
        .find(|artifact| artifact["artifactId"] == "upload.items.mapping")
        .expect("the Items mapping");
    mapping["name"] = json!(format!("Items rows, revision {revision}"));
    serde_json::from_value(mapping).expect("envelope")
}

/// The active definitions version and the pin of its Items mapping, as a
/// manifest names it (`<id>@<sha256 of the canonical artifact>`).
fn items_mapping_pin(host: &DevHost<norn_cell::engine::NativeEngine>) -> (u64, String) {
    let active = host.active_definitions().expect("read").expect("installed");
    let set: Vec<Value> = serde_json::from_str(&active.canonical_set).expect("set");
    let mapping = set
        .iter()
        .find(|artifact| artifact["artifactId"] == "upload.items.mapping")
        .expect("the Items mapping");
    let pin = norn_api::analytics_routes::dataset_mapping_pin("upload.items.mapping", mapping)
        .expect("canonical bytes");
    (active.version, pin)
}

/// #2882: a mapping pin hashes the mapping's Norn1 canonical bytes, so the
/// same mapping spelled with its keys in another order, at any depth, pins
/// the same; a changed value pins differently.
#[test]
fn a_mapping_pin_is_stable_under_key_reordering() {
    use norn_api::analytics_routes::dataset_mapping_pin;
    let pin = |text: &str| {
        let mapping: Value = serde_json::from_str(text).expect("mapping");
        dataset_mapping_pin("m", &mapping).expect("pin")
    };
    let first = pin(
        r#"{"artifactId":"m","payload":{"source":{"schemaId":"s","table":"t"},"fields":[{"name":"a","to":"x"}]},"version":"1.0.0"}"#,
    );
    let reordered = pin(
        r#"{"version":"1.0.0","payload":{"fields":[{"to":"x","name":"a"}],"source":{"table":"t","schemaId":"s"}},"artifactId":"m"}"#,
    );
    assert_eq!(first, reordered, "key order never changes the pin");
    let expected = norn_api::uploads::sha256_hex(
        br#"{"artifactId":"m","payload":{"fields":[{"name":"a","to":"x"}],"source":{"schemaId":"s","table":"t"}},"version":"1.0.0"}"#,
    );
    assert_eq!(
        first,
        format!("m@{expected}"),
        "the pin is the canonical bytes' digest"
    );
    let changed = pin(
        r#"{"artifactId":"m","payload":{"source":{"schemaId":"s","table":"u"},"fields":[{"name":"a","to":"x"}]},"version":"1.0.0"}"#,
    );
    assert_ne!(first, changed, "a changed value changes the pin");
}

/// MANIFEST-01/03: mapping edits installed while loads run never commit a
/// manifest that pairs one revision's mapping with another's pins: every
/// committed manifest's mapping pin is the mapping of the definitions
/// version it is pinned to, and every other load refuses as stale. The
/// loads pause between them, so the edits are not starved of the cell.
#[test]
fn mapping_edits_racing_loads_never_commit_a_mixed_snapshot() {
    use norn_api::analytics_routes::load_dataset;
    use std::sync::atomic::{AtomicBool, Ordering};
    /// Ends the loads however the edits end, a panic among them.
    struct Finished<'a>(&'a AtomicBool);
    impl Drop for Finished<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    const EDITS: u64 = 12;
    let host = items_host(false);
    let (version, pin) = items_mapping_pin(&host);
    let pins = std::sync::Mutex::new(BTreeMap::from([(version, pin)]));
    let done = AtomicBool::new(false);
    let committed = std::thread::scope(|threads| {
        threads.spawn(|| {
            let _finished = Finished(&done);
            for revision in 1..=EDITS {
                host.install_artifacts(
                    &[renamed_items_mapping(&host, revision)],
                    EXTRACTED_AT_MS + revision,
                )
                .expect("the edit installs");
                let (version, pin) = items_mapping_pin(&host);
                pins.lock().expect("pins").insert(version, pin);
            }
        });
        let engine = ThreeAndOne::default();
        let mut committed = Vec::new();
        loop {
            let last = done.load(Ordering::SeqCst);
            match load_dataset(
                &host,
                &scope(),
                Some("loader"),
                &items("u1", None),
                &OneUpload,
                &engine,
                EXTRACTED_AT_MS,
            ) {
                Ok(_) => committed.push(
                    norn_api::analytics_routes::current_manifest(&host, "upload.Items")
                        .expect("read")
                        .expect("the committed manifest")
                        .manifest
                        .pins,
                ),
                Err(refused) => assert_eq!(
                    (refused.status, refused.code.as_str()),
                    (409, "dataset.stale_pins"),
                    "a racing load refuses only as stale"
                ),
            }
            if last {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        committed
    });
    assert_ne!(committed, [], "the last load commits");
    let pins = pins.into_inner().expect("pins");
    for manifest in &committed {
        assert_eq!(
            Some(&manifest.mapping),
            pins.get(&manifest.definitions_version),
            "a manifest pinned to version {} names that version's mapping",
            manifest.definitions_version
        );
    }
}

/// MANIFEST-04: a request is fingerprinted by what it asks, not by what the
/// definitions resolve it to: an identical retry after a mapping edit keeps
/// its first receipt and lands nothing.
#[test]
fn a_retry_after_a_mapping_edit_keeps_its_first_receipt() {
    use norn_api::analytics_routes::load_dataset;
    let host = items_host(false);
    let engine = ThreeAndOne::default();
    let load = || {
        load_dataset(
            &host,
            &scope(),
            Some("loader"),
            &items("u1", Some("load-1")),
            &OneUpload,
            &engine,
            EXTRACTED_AT_MS,
        )
    };
    let first = load().expect("loaded");
    let (before, _) = items_mapping_pin(&host);
    host.install_artifacts(&[renamed_items_mapping(&host, 1)], EXTRACTED_AT_MS + 1)
        .expect("the edit installs");
    assert!(
        items_mapping_pin(&host).0 > before,
        "the mapping was edited"
    );
    let again = load().expect("the retry replays");
    assert_eq!(
        (
            again.manifest.as_str(),
            again.receipt.as_str(),
            again.commit_seq
        ),
        (
            first.manifest.as_str(),
            first.receipt.as_str(),
            first.commit_seq
        ),
        "the retry keeps its first receipt"
    );
    assert_eq!(
        engine.landed.lock().expect("calls").len(),
        1,
        "a replay lands nothing"
    );
}

/// MANIFEST-01: an answer resumes a landing only if it names that landing's
/// whole identity; an answer of another landing of the same file (another
/// typing) is never continued.
#[test]
fn an_answer_of_another_landing_is_not_continued() {
    use norn_api::analytics_routes::{EngineAnswer, EngineCall, Transport, load_dataset};
    struct Forged(ThreeAndOne, std::sync::Mutex<Option<EngineAnswer>>);
    impl Transport for Forged {
        fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
            self.0.capabilities()
        }
        fn resumed(&self) -> Option<EngineAnswer> {
            self.1.lock().expect("slot").take()
        }
        fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
            self.0.call(call)
        }
    }
    let host = items_host(false);
    let forged = norn_cell::analytics::LandResponse {
        landing: "another landing".into(),
        attempt: "a0".into(),
        load: norn_cell::analytics::LoadResponse {
            load: "l-other".into(),
            rows: 99,
            rejected: 0,
            reasons: BTreeMap::new(),
            bytes: 1,
            millis: 1,
        },
        data: Some(norn_cell::analytics::LandedFile {
            sha256: "00".repeat(32),
            rows: 99,
            bytes: 1,
        }),
        held: Some(norn_cell::analytics::LandedFile {
            sha256: "11".repeat(32),
            rows: 0,
            bytes: 1,
        }),
        valid_time: Vec::new(),
        findings: BTreeMap::new(),
        held_by_rules: 0,
        residue_total: 0,
        residue: Vec::new(),
        stored: Some("l-other".into()),
    };
    let engine = Forged(
        ThreeAndOne::default(),
        std::sync::Mutex::new(Some(EngineAnswer {
            op: "store".into(),
            digest: "not this landing".into(),
            status: 200,
            body: serde_json::to_string(&forged).expect("body"),
        })),
    );
    let loaded = load_dataset(
        &host,
        &scope(),
        Some("loader"),
        &items("u1", None),
        &OneUpload,
        &engine,
        EXTRACTED_AT_MS,
    )
    .expect("loaded");
    assert_eq!((loaded.rows, loaded.data.sha256), (3, "cd".repeat(32)));
    assert_eq!(engine.0.landed.lock().expect("calls").len(), 1);
}

/// MANIFEST-04: a request key binds one request: a retry answers with the
/// first receipt and lands nothing; the same key with another request is
/// refused; a load without a key always commits, so loading an earlier file
/// again makes it current again.
#[test]
fn a_request_key_binds_one_request_and_its_first_receipt() {
    use norn_api::analytics_routes::load_dataset;
    let host = items_host(false);
    let engine = ThreeAndOne::default();
    let load = |upload: &str, key: Option<&str>| {
        load_dataset(
            &host,
            &scope(),
            Some("loader"),
            &items(upload, key),
            &OneUpload,
            &engine,
            EXTRACTED_AT_MS,
        )
    };
    let first = load("u1", Some("load-1")).expect("loaded");
    let again = load("u1", Some("load-1")).expect("replayed");
    assert_eq!(
        (
            again.manifest.as_str(),
            again.receipt.as_str(),
            again.commit_seq
        ),
        (
            first.manifest.as_str(),
            first.receipt.as_str(),
            first.commit_seq
        ),
        "the same request keeps its first receipt"
    );
    assert_eq!(
        engine.landed.lock().expect("calls").len(),
        1,
        "a replay lands nothing"
    );
    let other = load("u2", Some("load-1")).expect_err("another request");
    assert_eq!(
        (other.status, other.code.as_str()),
        (409, "dataset.request_conflict")
    );
    let unkeyed = load("u1", None).expect("loaded");
    let unkeyed_again = load("u1", None).expect("loaded");
    assert!(
        unkeyed_again.commit_seq > unkeyed.commit_seq,
        "no key: every load commits"
    );
    assert_eq!(
        norn_api::analytics_routes::current_manifest(&host, "upload.Items")
            .expect("read")
            .expect("current")
            .commit_seq,
        unkeyed_again.commit_seq
    );
}

/// MANIFEST-03 + SYNC-REPORT-1: a dataset load is decided as an observation
/// load of the same rows: the uploads grant and the destination
/// connection's own, an admitted mapping onto the type, and no write-side
/// marking it cannot attach. It reaches the engine with a presigned URL and
/// is reported as a run of the type's `upload` connection.
#[test]
fn a_dataset_load_is_decided_as_an_observation_load_and_reported_as_its_run() {
    use norn_api::analytics_routes::load_dataset;
    let host = items_host(false);
    let engine = ThreeAndOne::default();
    for actor in [None, Some("viewer"), Some("uploader")] {
        let refused = load_dataset(
            &host,
            &scope(),
            actor,
            &items("u1", None),
            &OneUpload,
            &engine,
            0,
        )
        .expect_err("only the destination's loader loads");
        assert_eq!(refused.status, 403, "{actor:?}");
    }
    // canon.Item is mapped by two REST mappings: a load names one, and one
    // that computes its properties is not one bulk ingest can enforce.
    let canon = |mapping: Option<&str>| {
        let mut body = json!({"uploadId": "u1", "objectType": "canon.Item"});
        if let Some(mapping) = mapping {
            body["mapping"] = json!(mapping);
        }
        load_dataset(
            &host,
            &scope(),
            Some("loader"),
            &body.to_string(),
            &OneUpload,
            &engine,
            0,
        )
        .expect_err("refused")
    };
    assert_eq!(canon(None).status, 400);
    assert_eq!(
        canon(Some("erpnext.rest.mapping")).code,
        "bulk.unsupported_mapping"
    );
    assert_eq!(canon(Some("no.such.mapping")).code, "bulk.unmapped");
    let marked = items_host(true);
    let refused = load_dataset(
        &marked,
        &scope(),
        Some("loader"),
        &items("u1", None),
        &OneUpload,
        &engine,
        0,
    )
    .expect_err("a marked source field");
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (422, "bulk.unsupported_marking")
    );
    assert_eq!(engine.landed.lock().expect("calls").len(), 0);
    let loaded = load_dataset(
        &host,
        &scope(),
        Some("loader"),
        &items("u1", None),
        &OneUpload,
        &engine,
        EXTRACTED_AT_MS,
    )
    .expect("loaded");
    assert_eq!(
        (loaded.connection_id.as_str(), loaded.rows, loaded.rejected),
        ("upload.items", 3, 1)
    );
    let calls = engine.landed.lock().expect("calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].load.table.name, "upload.Items");
    // The GET URL is signed into each land call; the landing's identity is
    // the request without it (MANIFEST-01).
    assert_eq!(
        calls[0].load.file.url,
        "https://objects.example/uploads/demo/u1.csv"
    );
    let text = |object: &norn_ir::RuntimeObject, field: &str| {
        object
            .get(field)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let connection = host
        .host_object("source.Connection", "upload.items")
        .expect("read")
        .expect("the upload connection");
    for (field, value) in [
        ("system", "upload"),
        ("label", "items.csv"),
        ("target", "s3://uploads/demo/u1.csv"),
        ("auth", "presigned_upload"),
        ("status", "partial"),
        ("last_trigger", "upload"),
        ("quarantine_reasons", "bulk.type=1"),
        ("mapping_id", "upload.items.mapping"),
    ] {
        assert_eq!(text(&connection, field).as_deref(), Some(value), "{field}");
    }
    let run = host
        .host_object("source.SyncRun", &format!("upload.items/{EXTRACTED_AT_MS}"))
        .expect("read")
        .expect("the load's run");
    assert_eq!(text(&run, "run_by").as_deref(), Some("loader"));
    assert_eq!(text(&run, "outcome").as_deref(), Some("partial"));
    let count = |field: &str| run.get(field).and_then(RuntimeValue::as_i64);
    assert_eq!(
        (
            count("rows_read"),
            count("rows_landed"),
            count("rows_quarantined")
        ),
        (Some(4), Some(3), Some(1))
    );
}

/// The host issued nothing; every signing is counted.
#[derive(Default)]
struct IssuedNothing(std::sync::Mutex<usize>);

impl norn_api::analytics_routes::Uploads for IssuedNothing {
    fn record(&self, _id: &str) -> Option<norn_api::analytics_routes::UploadRecord> {
        None
    }
    fn issued(&self, _id: &str, _scope_key: &str, _uri: &str) -> bool {
        false
    }
    fn presigned_get(&self, uri: &str, _seconds: u64, _now_ms: u64) -> Option<String> {
        *self.0.lock().expect("signed") += 1;
        Some(format!("https://objects.example/{}", &uri["s3://".len()..]))
    }
    fn presigned_put(&self, key: &str, _seconds: u64, _now_ms: u64) -> Option<(String, String)> {
        *self.0.lock().expect("signed") += 1;
        Some((
            format!("https://objects.example/put/{key}"),
            format!("s3://uploads/{key}"),
        ))
    }
}

/// S5: a `source.Upload` record in the cell that the host did not issue for
/// this scope (here one naming another scope's object, as a forged
/// observation would have landed it) is refused as no such upload before
/// anything is signed or the engine is called.
#[test]
fn a_forged_upload_record_is_refused_before_any_signing_or_engine_call() {
    use norn_api::analytics_routes::load_dataset;
    use norn_api::sources::{KeptUpload, keep_upload};
    use norn_api::uploads::{Store, sha256_hex};
    let host = host();
    let forged = KeptUpload {
        upload_id: "u-forged".into(),
        store: Store::Uploads,
        uri: "s3://uploads/uploads/org_b/prod/secret.csv".into(),
        sha256: sha256_hex(b"secret"),
        size: 6,
        content_type: "text/csv".into(),
        file_name: "secret.csv".into(),
        csv: true,
        columns: Vec::new(),
        owner: "user:owner".into(),
        completed_at: "2026-11-12T12:00:05Z".into(),
    };
    keep_upload(&host, &forged, EXTRACTED_AT_MS).expect("the forged record lands");
    let host = host.with_authorizer(UploadsOnly);
    let scope = norn_api::scope::ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let body = json!({"uploadId": "u-forged", "objectType": "canon.Item"}).to_string();
    let uploads = IssuedNothing::default();
    let engine = ThreeAndOne::default();
    let refused = load_dataset(
        &host,
        &scope,
        Some("loader"),
        &body,
        &uploads,
        &engine,
        EXTRACTED_AT_MS,
    )
    .expect_err("a record the host did not issue never loads");
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (404, "dataset.not_found")
    );
    assert_eq!(*uploads.0.lock().expect("signed"), 0, "nothing was signed");
    assert_eq!(
        engine.landed.lock().expect("calls").len(),
        0,
        "no engine call"
    );
}

/// UPLOAD-RECORD-1: a completed upload is kept in the cell through its own
/// mapping and read back as kept; the same bytes kept again change nothing,
/// new bytes are the upload's next version, and an unknown id is not kept.
#[test]
fn a_completed_upload_is_kept_in_the_cell_and_read_back() {
    use norn_api::sources::{KeptUpload, keep_upload, kept_upload};
    use norn_api::uploads::{Store, parse_csv, sha256_hex};
    let host = host();
    let csv = "Region,Units\nnorth,3\n";
    let kept = KeptUpload {
        upload_id: "u7570.0123456789abcdef0123456789abcdef".into(),
        store: Store::Uploads,
        uri: "s3://uploads/uploads/org/dev/sales.csv".into(),
        sha256: sha256_hex(csv.as_bytes()),
        size: 21,
        content_type: "text/csv".into(),
        file_name: "sales.csv".into(),
        csv: true,
        columns: parse_csv(csv).expect("parsed").columns,
        owner: "user:owner".into(),
        // The cell keeps an instant in its canonical RFC 3339 form.
        completed_at: "2026-11-12T12:00:05Z".into(),
    };
    assert_eq!(kept_upload(&host, &kept.upload_id).expect("read"), None);
    keep_upload(&host, &kept, EXTRACTED_AT_MS).expect("kept");
    let mut read = kept_upload(&host, &kept.upload_id)
        .expect("read")
        .expect("the record");
    // The profile's scale is not part of the record; the load re-profiles.
    for column in &mut read.columns {
        column.scale = 0;
    }
    let mut expected = kept.clone();
    for column in &mut expected.columns {
        column.scale = 0;
    }
    assert_eq!(read, expected);
    keep_upload(&host, &kept, EXTRACTED_AT_MS).expect("kept again");
    let replaced = KeptUpload {
        sha256: sha256_hex(b"other bytes"),
        size: 11,
        ..kept.clone()
    };
    keep_upload(&host, &replaced, EXTRACTED_AT_MS + 1).expect("next version");
    assert_eq!(
        kept_upload(&host, &kept.upload_id)
            .expect("read")
            .expect("the record")
            .sha256,
        replaced.sha256
    );
    assert_eq!(kept_upload(&host, "u00.ffff").expect("read"), None);

    // A cell whose definitions predate source.Upload gets the cell's sources
    // pack through the governed install on its first record, then keeps it.
    let older = host_with(
        &artifacts()
            .into_iter()
            .filter(|a| {
                !matches!(
                    a.artifact_id.as_str(),
                    "source.Upload.entity" | "norn.connect.source" | "norn.connect.mapping"
                )
            })
            .collect::<Vec<_>>(),
    );
    assert_eq!(kept_upload(&older, &kept.upload_id).expect("read"), None);
    let before = older
        .active_definitions()
        .expect("read")
        .expect("active")
        .version;
    keep_upload(&older, &kept, EXTRACTED_AT_MS).expect("pack installed, record kept");
    assert!(
        older
            .active_definitions()
            .expect("read")
            .expect("active")
            .version
            > before
    );
    assert_eq!(
        kept_upload(&older, &kept.upload_id)
            .expect("read")
            .expect("the record")
            .sha256,
        kept.sha256
    );
}

/// SYNC-LEASE-1: a claimed run the host stops stepping (its fetch loop died)
/// holds its connection only for the lease; then a claim takes it over.
#[test]
fn an_abandoned_run_releases_its_connection_after_the_lease() {
    use norn_api::sources::RUN_LEASE_MS;
    let mut sources = sources();
    let first = sources
        .claim("odoo.rest", Trigger::Manual, "user:a", 1_000)
        .expect("claimed");
    assert!(matches!(
        sources.claim(
            "odoo.rest",
            Trigger::Manual,
            "user:b",
            1_000 + RUN_LEASE_MS - 1
        ),
        Err(ClaimError::Running)
    ));
    let second = sources
        .claim("odoo.rest", Trigger::Manual, "user:b", 1_000 + RUN_LEASE_MS)
        .expect("taken over");
    assert_ne!(first, second);
    assert!(sources.running("odoo.rest"));
}

/// SYNC-PAGE-1: a host's `pageSize` replaces every feed's request size,
/// within the pull's item bound.
#[test]
fn a_host_page_size_sets_every_feeds_page() {
    let config = |size: u32| {
        CONFIG.replace(
            r#""headers": {"X-Frappe-Site-Name": "frontend"}}"#,
            &format!(r#""headers": {{"X-Frappe-Site-Name": "frontend"}}, "pageSize": {size}}}"#),
        )
    };
    for refused in [0, 1001] {
        assert!(
            Sources::with_allowed_hosts(
                &cell_scope(),
                &config(refused),
                &BTreeMap::new(),
                SANDBOX_HOSTS
            )
            .is_err(),
            "{refused}"
        );
    }
    let mut sources = Sources::with_allowed_hosts(
        &cell_scope(),
        &config(250),
        &BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]),
        SANDBOX_HOSTS,
    )
    .expect("configuration");
    let host = host();
    sources
        .claim("erpnext.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS)
        .expect("claimed");
    match sources.step(&host, "erpnext.rest", None, EXTRACTED_AT_MS) {
        Step::Request(request) => {
            assert!(
                request.url.contains("limit_page_length=250"),
                "{}",
                request.url
            );
        }
        other @ (Step::Continue | Step::Done(_)) => panic!("expected a request, got {other:?}"),
    }
}

/// SYNC-YIELD-2: a host paces a sync's steps; a step maps at most the rows
/// it was given and says how many it mapped, and the rest follow in later
/// steps.
#[test]
fn a_paced_sync_maps_at_most_its_rows_per_step() {
    let host = host();
    let mut sources = sources();
    let mut pages = recorded("odoo");
    sources
        .claim("odoo.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS)
        .expect("claimed");
    sources.pace("odoo.rest", 3);
    let Step::Request(first) = sources.step(&host, "odoo.rest", None, EXTRACTED_AT_MS) else {
        panic!("a first request");
    };
    assert_eq!(sources.mapped("odoo.rest"), 0);
    let dataset = profile("odoo").feeds[0].dataset.clone();
    let (status, body) = pages.remove(&dataset).expect("a recorded page");
    let rows = body
        .pointer("/records")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert!(rows > 3, "{} has {rows} rows", first.url);
    let step = sources.step(
        &host,
        "odoo.rest",
        Some((status, body.to_string().as_bytes())),
        EXTRACTED_AT_MS,
    );
    assert!(matches!(step, Step::Continue), "{step:?}");
    assert_eq!(sources.mapped("odoo.rest"), 3);
    let step = sources.step(&host, "odoo.rest", None, EXTRACTED_AT_MS);
    assert!(matches!(step, Step::Continue), "{step:?}");
    assert_eq!(sources.mapped("odoo.rest"), 3);
}

/// SYNC-SERIAL-1: one sync runs per cell at a time. While one connection
/// syncs, another's claim is refused naming it and the schedule claims
/// nothing; once it ends, the next one runs.
#[test]
fn one_sync_runs_per_cell_at_a_time() {
    let host = host();
    let mut sources = sources();
    sources
        .claim("odoo.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS)
        .expect("claimed");
    assert_eq!(
        sources.claim("erpnext.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS),
        Err(ClaimError::Busy("odoo.rest".to_owned()))
    );
    assert_eq!(sources.due(EXTRACTED_AT_MS), None);
    let _ = sync(
        &host,
        &mut sources,
        "odoo.rest",
        ("odoo", true),
        EXTRACTED_AT_MS,
    );
    assert!(!sources.running("odoo.rest"));
    sources
        .claim(
            "erpnext.rest",
            Trigger::Manual,
            "user:a",
            EXTRACTED_AT_MS + 1,
        )
        .expect("the next one runs");
}

/// TIMER-ALARM-1: a host that was evicted keeps the schedule the cell
/// recorded. A fresh `Sources` (the host's in-memory state is gone) reads
/// each connection's last sync end back from its durable connection record,
/// so the next sync falls due `everyMinutes` after it, not at once.
#[test]
fn an_evicted_host_keeps_the_recorded_sync_schedule() {
    let host = host();
    let started_ms = 1_794_484_800_000;
    let ended_ms = started_ms + 5;
    norn_api::sources::report_upload(
        &host,
        &norn_api::sources::UploadRun {
            id: "upload.items",
            label: "items.csv",
            uri: "s3://uploads/demo/items.csv",
            sha256: &"ab".repeat(32),
            mapping: "",
            actor: "viewer@demo",
            started_ms,
            ended_ms,
            counts: (1, 1, 0),
            reasons: BTreeMap::new(),
        },
    );
    assert!(
        host.host_object("source.Connection", "upload.items")
            .expect("read")
            .is_some(),
        "the run is recorded"
    );
    let config = serde_json::json!([{ "id": "upload.items", "system": "erpnext", "label": "items",
        "connector": "c", "mapping": "m", "instance": "i",
        "baseUrl": "https://erp.example.test", "everyMinutes": 15,
        "orgId": "org_demo", "environmentId": "dev", "secretRefs": [] }])
    .to_string();
    let mut evicted = Sources::new(&cell_scope(), &config, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("configuration: {error}"));
    let now = started_ms + 60_000;
    evicted.restore_schedule(&host).expect("restored");
    assert_eq!(evicted.next_due(now), Some(ended_ms + 15 * 60_000));
    assert_eq!(evicted.due(now), None, "not due before its schedule");
    assert_eq!(
        evicted.due(ended_ms + 15 * 60_000).as_deref(),
        Some("upload.items")
    );
}

/// TIMER-ALARM-1 (round 2 S8): a cell from before connection records were
/// recorded has no `source.Connection` type. That is no recorded schedule,
/// not a restore error: the host opens, and its scheduled connection is due
/// as one never synced.
#[test]
fn a_cell_without_connection_records_restores_no_schedule() {
    let catalog: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts");
    let host = host_with(&catalog);
    assert!(
        !host
            .object_type_names()
            .expect("names")
            .iter()
            .any(|name| name == "source.Connection"),
        "the older cell has no connection records"
    );
    let config = serde_json::json!([{ "id": "erpnext.rest", "system": "erpnext", "label": "items",
        "connector": "c", "mapping": "m", "instance": "i",
        "baseUrl": "https://erp.example.test", "everyMinutes": 15,
        "orgId": "org_demo", "environmentId": "dev", "secretRefs": [] }])
    .to_string();
    let mut sources = Sources::new(&cell_scope(), &config, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("configuration: {error}"));
    sources
        .restore_schedule(&host)
        .expect("no recorded schedule is not an error");
    let now = 1_794_484_800_000;
    assert_eq!(sources.next_due(now), Some(now));
}

/// PULL-WATERMARK-1: a mid-page ingest failure must NOT persist the accepted
/// page's advanced watermark for rows still waiting in the drain queue.
#[test]
fn a_mid_page_ingest_failure_does_not_advance_the_durable_watermark() {
    let good_host = host();
    // A second host without `odoo.rest.mapping` causes `host.ingest` to fail on
    // the second drained chunk of the same page while still recording the run.
    let without_mapping: Vec<_> = artifacts()
        .into_iter()
        .filter(|a| a.artifact_id.as_str() != "odoo.rest.mapping")
        .collect();
    let failing_host = host_with(&without_mapping);
    let mut sources = sources();
    sources
        .claim("odoo.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS)
        .expect("claimed");
    sources.pace("odoo.rest", 1);

    let Step::Request(first_req) = sources.step(&good_host, "odoo.rest", None, EXTRACTED_AT_MS)
    else {
        panic!("expected initial request");
    };
    let profile = profile("odoo");
    let feed = profile
        .feeds
        .iter()
        .find(|f| first_req.url.contains(f.request.path.as_str()))
        .expect("feed");
    let (status, body) = recorded("odoo")
        .remove(&feed.dataset)
        .expect("recorded multi-row page");
    // Accept the page and drain only the first row (`pace = 1`).
    let step = sources.step(
        &good_host,
        "odoo.rest",
        Some((status, body.to_string().as_bytes())),
        EXTRACTED_AT_MS,
    );
    assert!(
        matches!(step, Step::Continue),
        "multi-row page with pace=1 must yield Continue after first row"
    );
    // Now fail ingest on the second chunk of the same page.
    let Step::Done(summary) = sources.step(&failing_host, "odoo.rest", None, EXTRACTED_AT_MS + 1)
    else {
        panic!("ingest failure must terminate the run");
    };
    assert_eq!(summary.outcome, "failed");
    // The durable `source.Connection` end watermark persisted on `failing_host`
    // must remain empty (`"{}"`) rather than advancing past uncaptured rows.
    let recorded_conn = failing_host
        .host_object("source.Connection", "odoo.rest")
        .expect("read connection")
        .expect("end run recorded");
    assert_eq!(
        recorded_conn.get("watermark"),
        Some(&RuntimeValue::String("{}".into())),
        "persisted end watermark must not advance on mid-page ingest failure: {recorded_conn:?}"
    );

    // Restarting with fresh `Sources` state against the same `failing_host`
    // store (which reloads the persisted watermark) must re-request from the
    // initial cursor, not the unfinished page's advanced cursor.
    let mut restarted = self::sources();
    restarted
        .claim("odoo.rest", Trigger::Manual, "user:a", EXTRACTED_AT_MS + 2)
        .expect("re-claim");
    let Step::Request(retry_req) =
        restarted.step(&failing_host, "odoo.rest", None, EXTRACTED_AT_MS + 2)
    else {
        panic!("expected retry request");
    };
    assert_eq!(
        retry_req.url, first_req.url,
        "failed mid-page drain must not advance the durable cursor past uncaptured rows"
    );
    assert_eq!(retry_req.body, first_req.body);
}
