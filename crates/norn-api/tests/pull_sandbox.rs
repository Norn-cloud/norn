//! #921: recorded responses of the live `ERPNext` and Odoo sandboxes, pulled by
//! the generic engine from each pack's `norn.pull/1` profile, land as canonical
//! objects through the cell's own mapping path (`DevHost::ingest`).
//!
//! The responses are the sandboxes' first pages, recorded from the real APIs
//! (fixtures/connectors/sandbox); a request for a later page gets an empty page.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::type_complexity,
    reason = "recorded sandbox scenarios"
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

/// The three-systems catalogue with the sandbox source schemas and mappings.
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
                "registry/packs/{system}-sandbox/artifacts/{system}.sandbox.{kind}.json"
            )));
        }
    }
    for name in [
        "source.Synced.interface",
        "source.Connection.entity",
        "source.SyncRun.entity",
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
    let artifacts = artifacts();
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
        .with_governing_laws(&artifacts)
        .expect("governing laws");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::to_value(&artifacts).expect("set"))
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
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(model, input, Some(0), &scope.key())
        .expect("installed");
    DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host")
}

fn profile(system: &str) -> PullProfile {
    let connector = artifact(&format!(
        "registry/packs/{system}-sandbox/artifacts/{system}.sandbox.connector.json"
    ));
    let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
    PullProfile::from_connector(&payload)
        .expect("valid profile")
        .expect("a pull profile")
}

/// The recorded first page per dataset, then empty pages.
fn recorded(system: &str) -> BTreeMap<String, (u16, Value)> {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join(format!(
            "fixtures/connectors/sandbox/{system}.responses.json"
        )))
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
        for row in pull
            .accept(status, body.to_string().as_bytes())
            .expect("a page in the declared shape")
        {
            let raw = observation(&profile, &row, EXTRACTED_AT_MS).expect("sealed");
            rows.push((
                raw,
                MappingInput {
                    mapping: format!("{system}.sandbox.mapping"),
                    dataset: row.dataset.clone(),
                    connector_instance: "sandbox".into(),
                    partition: row.dataset.clone(),
                    recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
                },
            ));
        }
    }
    (rows, pull.watermarks().clone())
}

fn count(outcomes: &[Ingested], wanted: &MappingOutcome) -> usize {
    outcomes.iter().filter(|o| &o.outcome == wanted).count()
}

#[test]
fn sandbox_rows_land_through_the_mapping_path_once() {
    let host = host();
    for system in SYSTEMS {
        let (rows, watermarks) = pull(system, BTreeMap::new());
        assert!(!rows.is_empty(), "{system} pulled nothing");
        let outcomes = host.ingest(&rows).expect("ingested");
        let quarantined: Vec<_> = outcomes
            .iter()
            .filter(|o| matches!(o.outcome, MappingOutcome::Quarantined(_)))
            .collect();
        assert!(quarantined.is_empty(), "{system}: {quarantined:#?}");
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
fn sandbox_mappings_pin_their_source_schemas() {
    let pairs = [
        (
            "erpnext-sandbox",
            "erpnext.sandbox",
            "registry/packs/erpnext-sandbox/sandbox.ts",
        ),
        (
            "odoo-sandbox",
            "odoo.sandbox",
            "registry/packs/odoo-sandbox/sandbox.ts",
        ),
        (
            "sources",
            "norn.connect",
            "registry/packs/sources/records.ts",
        ),
    ];
    let mut wrong = Vec::new();
    for (pack, stem, typed) in pairs {
        let source = artifact(&format!(
            "registry/packs/{pack}/artifacts/{stem}.source.json"
        ));
        let mapping = artifact(&format!(
            "registry/packs/{pack}/artifacts/{stem}.mapping.json"
        ));
        let pin = norn_checker::ArtifactRef::of(&source).expect("pin").sha256;
        let payload = serde_json::to_value(mapping.payload.to_json_value()).expect("payload");
        if payload["source"]["sha256"].as_str() != Some(pin.as_str()) {
            wrong.push(format!("PIN {stem}.source {pin} {typed}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

const CONFIG: &str = r#"[
  {"id": "erpnext.sandbox", "system": "erpnext", "label": "ERPNext sandbox", "connector": "erpnext.sandbox.connector",
   "mapping": "erpnext.sandbox.mapping", "instance": "sandbox", "baseUrl": "http://erpnext-sandbox:8000",
   "headers": {"X-Frappe-Site-Name": "frontend"}},
  {"id": "odoo.sandbox", "system": "odoo", "label": "Odoo sandbox", "connector": "odoo.sandbox.connector",
   "mapping": "odoo.sandbox.mapping", "instance": "sandbox", "baseUrl": "http://odoo-sandbox:8069"}
]"#;

fn sources() -> Sources {
    Sources::new(
        CONFIG,
        BTreeMap::from([
            (
                "NORN_SECRET_ERPNEXT_SANDBOX_TOKEN".to_owned(),
                "key:secret".to_owned(),
            ),
            (
                "NORN_SECRET_ODOO_SANDBOX_API_KEY".to_owned(),
                "odoo-key".to_owned(),
            ),
        ]),
    )
    .expect("configuration")
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
            // SYNC-YIELD-1: rows map a few at a time between the host's turns.
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
            "erpnext.sandbox",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_800_000,
        )
        .expect("claimed");
    // SYNC-ONE-1: a second claim while it runs is refused.
    assert_eq!(
        sources.claim(
            "erpnext.sandbox",
            Trigger::Manual,
            "user:other@demo",
            1_794_484_800_001
        ),
        Err(ClaimError::Running)
    );
    let (summary, sent) = sync(
        &host,
        &mut sources,
        "erpnext.sandbox",
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
                .starts_with("http://erpnext-sandbox:8000/api/resource/")
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
        .host_object("source.Connection", "erpnext.sandbox")
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
            "erpnext.sandbox",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_900_000,
        )
        .expect("claimed again");
    let (again, sent) = sync(
        &host,
        &mut sources,
        "erpnext.sandbox",
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
    let second = sources.due(t0).expect("the other one is due too");
    assert_ne!(first, second);
    assert_eq!(sources.due(t0), None, "both are running");
    for (id, system) in [
        (first.as_str(), &first[..first.find('.').expect("dotted")]),
        (
            second.as_str(),
            &second[..second.find('.').expect("dotted")],
        ),
    ] {
        let (summary, _) = sync(&host, &mut sources, id, (system, true), t0);
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
    }
    assert_eq!(
        sources.due(t0 + 60_000),
        None,
        "not due again within 15 minutes"
    );
    assert!(sources.due(t0 + 15 * 60_000).is_some());
}

#[test]
fn a_refused_read_fails_the_run_and_says_why() {
    let host = host();
    let mut sources = sources();
    sources
        .claim(
            "odoo.sandbox",
            Trigger::Manual,
            "user:owner@demo",
            1_794_484_800_000,
        )
        .expect("claimed");
    let Step::Request(_) = sources.step(&host, "odoo.sandbox", None, 1_794_484_800_000) else {
        panic!("a first request")
    };
    let Step::Done(summary) =
        sources.step(&host, "odoo.sandbox", Some((401, b"{}")), 1_794_484_800_000)
    else {
        panic!("the run ends")
    };
    assert_eq!(summary.outcome, "failed");
    assert_eq!(
        summary.error.as_ref().map(|e| e.0.as_str()),
        Some("source.http_status")
    );
    let connection = host
        .host_object("source.Connection", "odoo.sandbox")
        .expect("read")
        .expect("connection");
    assert_eq!(
        connection.get("status").and_then(RuntimeValue::as_str),
        Some("failed")
    );
    assert!(!sources.running("odoo.sandbox"));
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
    // The sandbox mappings keep working on the extended definitions.
    let (rows, _) = pull("odoo", BTreeMap::new());
    assert!(
        host.ingest(&rows)
            .expect("ingested")
            .iter()
            .all(|o| !matches!(o.outcome, MappingOutcome::Quarantined(_)))
    );
}
