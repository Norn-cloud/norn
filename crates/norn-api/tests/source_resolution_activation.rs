//! SOURCE-RESOLVE-1 (Sol S7b): a definitions activation and its
//! reconciliation are one restart-safe path. An activation committed without
//! its reconciliation (a seed upgrade, or a governed install interrupted
//! between the two) is reconciled when a host opens the cell, before any
//! read serves the new revision.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "activation conformance over the retained three-systems catalog"
)]
use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::{CellError, CellStore, InstalledDefinitionInput, Model, StoreOptions};
use norn_checker::{
    EntityArtifactSetContext,
    receipt::{ReceiptSources, check_artifact_set_with_receipt},
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const NORTH: &str = "demo/holding/owner/north/gold";

/// A native engine whose next `failures` reconciliations fail at their last
/// step, clearing the pending mark: an interrupted reconciliation, injected
/// from the test alone (the production cell has no hook for it).
struct Interrupting {
    inner: NativeEngine,
    failures: Arc<AtomicUsize>,
}

impl Interrupting {
    fn open(path: &Path, failures: &Arc<AtomicUsize>) -> Self {
        Self {
            inner: NativeEngine::open(path, StoreOptions::default()).expect("engine"),
            failures: Arc::clone(failures),
        }
    }
}

impl SqlEngine for Interrupting {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        let clears_mark = sql.starts_with("DELETE")
            && params
                .iter()
                .any(|p| matches!(p, SqlValue::Text(t) if t == "source_resolution_pending"));
        if clears_mark
            && self
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            return Err(CellError::Engine("reconciliation interrupted".to_owned()));
        }
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.inner.transaction(f)
    }
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.inner.read_cache_revision()
    }
}
const SCOPE: &str = "org_demo/dev";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The retained three-systems catalog, with the holding's writers ranked
/// `ranks` (`ERPNext`, Shopify).
fn catalog(ranks: (u64, u64)) -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("fixtures/demo/explorer/three-systems.definitions.json"),
        )
        .expect("retained catalog"),
    )
    .expect("artifacts");
    for (system, rank) in [("erpnext", ranks.0), ("shopify", ranks.1)] {
        let artifact = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == format!("{system}.three.mapping"))
            .expect("mapping");
        let mut wire = serde_json::to_value(&*artifact).expect("wire");
        for map in wire["payload"]["maps"].as_array_mut().expect("maps") {
            if map["id"] == "holding" {
                map["authority"] = json!({"kind": "precedence", "rank": rank});
            }
        }
        *artifact = serde_json::from_value(wire).expect("edited mapping");
    }
    artifacts
}

/// Admit `artifacts` over their core layer: the model and install input.
fn admit(artifacts: &[ArtifactEnvelope], installed_at: u64) -> (Model, InstalledDefinitionInput) {
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
                panic!("{} is outside the profile", artifact.artifact_id)
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
        installed_at,
        layout: None,
    };
    (model, input)
}

/// Capture and land `system`'s snapshot with the north holding at `grams`.
fn sync<E: SqlEngine>(store: CellStore<E>, system: &str, grams: u64) -> CellStore<E> {
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join(format!("registry/packs/{system}/synthetic.json")))
            .expect("fixture"),
    )
    .expect("json");
    let mut cell = norn_cell::SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("cell");
    for row in fixture["snapshot"].as_array().expect("rows") {
        let mut record = row["record"].clone();
        let north = ["custom_norn_ref", "metafield_norn_ref"]
            .iter()
            .any(|field| record.get(*field).and_then(Value::as_str) == Some(NORTH));
        if north && record.get("resource_code").is_some() {
            record["quantity"] = json!(grams);
        }
        let source = format!("synthetic-{system}");
        let id = record
            .get("id")
            .or_else(|| record.get("name"))
            .and_then(Value::as_str)
            .expect("source record identity")
            .to_owned();
        let payload = RuntimeValue::from_json_text(record.to_string()).expect("payload");
        let digest = norn_ir::canonical::HashProfile::Norn1
            .hash_json(serde_json::to_string(&payload).expect("payload text"))
            .expect("payload digest");
        let raw = norn_kernel::Observed {
            id: norn_kernel::ObservationId(format!("{source}/{id}")),
            provenance: norn_kernel::Provenance {
                source: source.clone(),
                source_id: id.clone(),
                mapping_version: format!("{system}.three.mapping"),
                observed_at: norn_kernel::LogicalTime::from_millis(1_793_620_800_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>source, "source_id"=>id, "event_id"=>"snapshot", "source_version"=>"1", "pipeline_version"=>format!("{system}.three.mapping"), "kind"=>"upsert", "extracted_at"=>1_793_620_800_000u64, "payload"=>payload, "payload_digest"=>digest},
        };
        let input = norn_cell::mapping_bundle::MappingInput {
            mapping: format!("{system}.three.mapping"),
            dataset: row["dataset"].as_str().expect("dataset").to_owned(),
            connector_instance: "demo".into(),
            partition: "snapshot".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-02T12:00:00Z".into()),
        };
        let (id, _) = cell
            .inject_mapping_bundle(&raw, &input)
            .expect("captured as evidence");
        cell.process_mapping_bundle(&id)
            .unwrap_or_else(|error| panic!("{id} was refused: {error}"));
    }
    cell.into_store()
}

fn quantity<E: SqlEngine>(store: &CellStore<E>) -> Option<RuntimeValue> {
    store
        .read()
        .object(norn_cell::MAIN_WORLD, "canon.Holding", NORTH)
        .expect("read")
        .and_then(|record| record.values.get("quantity").cloned())
}

#[test]
fn an_activation_interrupted_before_its_reconciliation_resumes_before_the_host_serves() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (model, input) = admit(&catalog((0, 2)), 1);
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    store
        .install_definitions(model, input, Some(0), SCOPE)
        .expect("installed");
    let store = sync(store, "erpnext", 2765);
    let store = sync(store, "shopify", 2458);
    // The first activation's reconciliation, as the governed install runs it.
    let mut cell = norn_cell::SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("cell");
    cell.reconcile_source_resolution().expect("reconciled");
    let mut store = cell.into_store();
    assert!(!store.source_resolution_pending().expect("mark"));
    assert_eq!(
        quantity(&store),
        Some(json_decimal(2765)),
        "ERPNext decides"
    );
    // Shopify now outranks ERPNext; the cell stops right after activation.
    let (model, input) = admit(&catalog((2, 0)), 2);
    store
        .install_definitions(model.clone(), input, Some(1), SCOPE)
        .expect("activated");
    drop(store);
    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
    assert!(
        reopened.source_resolution_pending().expect("mark"),
        "the activation's commit carries the pending reconciliation"
    );
    assert_eq!(
        quantity(&reopened),
        Some(json_decimal(2765)),
        "not yet reconciled"
    );
    let host = DevHost::from_store(
        reopened,
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |_| BTreeMap::new(),
    )
    .expect("host");
    // The first read the host serves already has the new revision's winner.
    assert_eq!(
        host.host_object("canon.Holding", NORTH)
            .expect("read")
            .and_then(|values| values.get("quantity").cloned()),
        Some(json_decimal(2458))
    );
}

/// A host over an [`Interrupting`] cell at `path` whose installed revision
/// ranks `ERPNext` first, with both sources synced and reconciled: north
/// reads `ERPNext`'s 2765 g.
fn reconciled_host(path: &Path, failures: &Arc<AtomicUsize>) -> DevHost<Interrupting> {
    let (model, input) = admit(&catalog((0, 2)), 1);
    let mut store =
        CellStore::from_engine(Interrupting::open(path, failures), model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    store
        .install_definitions(model, input, Some(0), SCOPE)
        .expect("installed");
    let store = sync(store, "erpnext", 2765);
    let store = sync(store, "shopify", 2458);
    let mut cell = norn_cell::SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("cell");
    cell.reconcile_source_resolution().expect("reconciled");
    let host = DevHost::from_store(
        cell.into_store(),
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |_| BTreeMap::new(),
    )
    .expect("host");
    assert_eq!(north(&host).expect("read"), Some(json_decimal(2765)));
    host
}

fn north(host: &DevHost<Interrupting>) -> Result<Option<RuntimeValue>, norn_api::refusal::Refusal> {
    host.host_object("canon.Holding", NORTH)
        .map(|values| values.and_then(|values| values.get("quantity").cloned()))
}

/// Install the revision ranking the holding's writers `ranks` over
/// `expected`, as the governed install does.
fn install(
    host: &DevHost<Interrupting>,
    ranks: (u64, u64),
    installed_at: u64,
    expected: u64,
) -> Result<norn_cell::InstalledDefinitionSet, norn_api::refusal::Refusal> {
    let (model, input) = admit(&catalog(ranks), installed_at);
    let model = norn_api::host::reopen_model(&input.canonical_set, false).unwrap_or(model);
    host.install_definitions(model, input, Some(expected))
}

/// SOURCE-RESOLVE-1 (Sol S7b): a governed install whose reconciliation
/// fails leaves the new revision active and its durable pending mark set.
/// On the same host (no reopen), a read while the mark is set refuses as
/// retryable instead of serving the stale value, and the next request
/// resumes the reconciliation under the definition epoch and reads the new
/// revision's winner.
#[test]
fn a_failed_reconciliation_is_never_served_and_the_next_request_finishes_it() {
    let directory = tempfile::tempdir().expect("directory");
    let failures = Arc::new(AtomicUsize::new(0));
    let host = reconciled_host(&directory.path().join("cell.sqlite"), &failures);
    // Shopify now outranks ERPNext. The install's reconciliation fails, and
    // so do the next two attempts to resume it.
    failures.store(3, Ordering::SeqCst);
    install(&host, (2, 0), 2, 1).expect("the activation commits");
    assert_eq!(
        host.active_definitions_version().ok(),
        None,
        "gated while pending"
    );
    let refused = north(&host).expect_err("never the stale value");
    assert_eq!(refused.code, "defs.stale");
    assert!(refused.retryable);
    // The next request completes the reconciliation and reads the winner.
    assert_eq!(north(&host).expect("read"), Some(json_decimal(2458)));
    assert_eq!(host.active_definitions_version().expect("read"), Some(2));
}

/// SOURCE-RESOLVE-1 (#1478 r5 S5): a reconciliation that fails every time
/// never locks the cell. A corrective install supersedes it (the revision
/// commits although the pending reconciliation still fails), live reads stay
/// gated meanwhile, and once a reconciliation finishes the corrective
/// revision's winner is served.
#[test]
fn a_corrective_install_supersedes_a_reconciliation_that_keeps_failing() {
    let directory = tempfile::tempdir().expect("directory");
    let failures = Arc::new(AtomicUsize::new(0));
    let host = reconciled_host(&directory.path().join("cell.sqlite"), &failures);
    failures.store(usize::MAX, Ordering::SeqCst);
    install(&host, (2, 0), 2, 1).expect("the activation commits");
    assert_eq!(north(&host).expect_err("gated").code, "defs.stale");
    // ERPNext is ranked first again; the install is not refused by the
    // reconciliation it supersedes.
    let corrected = install(&host, (0, 3), 3, 2).expect("the corrective install commits");
    assert_eq!(corrected.version, 3);
    let gated = north(&host).expect_err("still gated while it fails");
    assert!(gated.retryable, "{gated:?}");
    failures.store(0, Ordering::SeqCst);
    assert_eq!(north(&host).expect("read"), Some(json_decimal(2765)));
    assert_eq!(host.active_definitions_version().expect("read"), Some(3));
}

fn json_decimal(value: u64) -> RuntimeValue {
    RuntimeValue::from_json(json!(value)).expect("decimal")
}
