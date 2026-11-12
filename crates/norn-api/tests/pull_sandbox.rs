//! #921: recorded responses of the live ERPNext and Odoo sandboxes, pulled by
//! the generic engine from each pack's `norn.pull/1` profile, land as canonical
//! objects through the cell's own mapping path (`DevHost::ingest`).
//!
//! The responses are the sandboxes' first pages, recorded from the real APIs
//! (fixtures/connectors/sandbox); a request for a later page gets an empty page.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "recorded sandbox scenarios"
)]
use norn_api::host::{DevHost, Ingested};
use norn_api::pull::{Limits, Pull, PullProfile, observation};
use norn_api::scope::CellScope;
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
        for kind in ["source", "mapping"] {
            set.push(artifact(&format!(
                "registry/packs/{system}/artifacts/{system}.sandbox.{kind}.json"
            )));
        }
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
    for artifact in artifacts.iter().filter(|a| a.kind() == ArtifactKind::Entity) {
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
        "registry/packs/{system}/artifacts/{system}.sandbox.connector.json"
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
            root().join(format!("fixtures/connectors/sandbox/{system}.responses.json")),
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
    let feed = profile.feeds.iter().find(|f| f.dataset == dataset).expect("feed");
    let mut body = json!({});
    let mut at = &mut body;
    let parts: Vec<&str> = feed.items_path.trim_start_matches('/').split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        at[*part] = if i + 1 == parts.len() { json!([]) } else { json!({}) };
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
    let mut pull = Pull::new(&profile, watermarks, Limits::default());
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
        assert_eq!(count(&outcomes, &MappingOutcome::Landed), rows.len(), "{system}");
        // The same source events again are the same observations: nothing lands twice.
        let again = host.ingest(&rows).expect("re-ingested");
        assert_eq!(count(&again, &MappingOutcome::AlreadyLanded), rows.len(), "{system}");
        // Every feed's watermark is its last row's position.
        let profile = profile(system);
        assert_eq!(
            watermarks.keys().collect::<BTreeSet<_>>(),
            profile.feeds.iter().map(|f| &f.dataset).collect::<BTreeSet<_>>()
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
    let mut wrong = Vec::new();
    for system in SYSTEMS {
        let source = artifact(&format!(
            "registry/packs/{system}/artifacts/{system}.sandbox.source.json"
        ));
        let mapping = artifact(&format!(
            "registry/packs/{system}/artifacts/{system}.sandbox.mapping.json"
        ));
        let pin = norn_checker::ArtifactRef::of(&source).expect("pin").sha256;
        let payload = serde_json::to_value(mapping.payload.to_json_value()).expect("payload");
        if payload["source"]["sha256"].as_str() != Some(pin.as_str()) {
            wrong.push(format!("PIN {system}.sandbox.source {pin}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
