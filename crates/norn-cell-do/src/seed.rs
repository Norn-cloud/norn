//! The public three-systems demo cell (Drop 9), provisioned idempotently.
//!
//! The definitions are the retained catalog Studio already renders
//! (`fixtures/demo/explorer/three-systems.definitions.json`). They are admitted
//! by the shared checker at boot, so the admission receipt and its digest are
//! computed, not copied; an identical provision on a restarted Durable Object
//! reopens the first revision without writing (`provision_definitions`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_api::scope::CellScope;
use norn_cell::{
    CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model,
    engine::SqlEngine,
};
use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};

const DEFINITIONS: &str =
    include_str!("../../../fixtures/demo/explorer/three-systems.definitions.json");

/// Logical install time of the demo revision; fixed so every boot is identical.
const INSTALLED_AT: u64 = 1;

/// A bootstrap failure. The cell is not served; the text is for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedError(pub String);

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SeedError {}

fn fail(context: &str) -> impl Fn(&dyn fmt::Display) -> SeedError + '_ {
    move |error| SeedError(format!("{context}: {error}"))
}

/// The demo cell's physical scope.
#[must_use]
pub fn demo_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

/// Admit the demo definitions and provision (or reopen) them on `engine`.
///
/// # Errors
/// The retained set no longer admits, or the engine holds a different cell.
pub fn provision<E: SqlEngine>(engine: E, scope: &CellScope) -> Result<CellStore<E>, SeedError> {
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(DEFINITIONS).map_err(|e| fail("definitions")(&e))?;
    provision_set(engine, scope, &artifacts)
}

/// Admit `artifacts` with the shared checker and provision, or reopen, them
/// on a fresh `engine` as the cell's first governed definitions version.
///
/// # Errors
/// The set does not admit, or the engine holds a different cell.
pub(crate) fn provision_set<E: SqlEngine>(
    engine: E,
    scope: &CellScope,
    artifacts: &[ArtifactEnvelope],
) -> Result<CellStore<E>, SeedError> {
    let (model, input) = admit_set(artifacts)?;
    let mut store = CellStore::provision_definitions(engine, model, &input, &scope.key())
        .map_err(|e| fail("provision")(&e))?;
    seed_rows(&mut store, scope)?;
    Ok(store)
}

/// Admit `artifacts` with the shared checker (any `core.` or `std.` artifact
/// as the retained trusted base): the projected model and the install input
/// carrying the computed receipt, installed at the fixed logical time.
///
/// # Errors
/// The set does not admit.
pub(crate) fn admit_set(
    artifacts: &[ArtifactEnvelope],
) -> Result<(Model, InstalledDefinitionInput), SeedError> {
    let value = |artifact: &ArtifactEnvelope| {
        serde_json::to_value(artifact)
            .map_err(|e| fail("artifact wire")(&e))
            .and_then(|wire| RuntimeValue::from_json(wire).map_err(|e| fail("artifact value")(&e)))
    };
    // The packs are checked over the core layer as a trusted base, which the
    // cell retains in the same set (`baseArtifactIds` on the definitions read).
    let (base, candidates): (Vec<_>, Vec<_>) = artifacts
        .iter()
        .cloned()
        .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
    let candidate_values = candidates
        .iter()
        .map(value)
        .collect::<Result<Vec<_>, _>>()?;
    let base_values = base.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let checked = check_artifact_set_with_receipt(
        &ReceiptSources::new(&candidate_values, &base_values)
            .map_err(|e| fail("receipt sources")(&e))?,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| fail("admission")(&e))?;
    if !checked.receipt.admitted {
        return Err(SeedError(format!(
            "demo definitions refused: {:?}",
            checked.diagnostics
        )));
    }
    let receipt = checked
        .receipt
        .canonical_json()
        .map_err(|e| fail("receipt")(&e))?;
    let sources = artifacts.iter().map(value).collect::<Result<Vec<_>, _>>()?;
    let canonical_set = RuntimeValue::Array(sources)
        .canonical_json()
        .map_err(|e| fail("canonical set")(&e))?;
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(canonical_set).map_err(|e| fail("set")(&e))?,
        registry: vec![],
        receipt_sha256: checked
            .receipt
            .content_sha256()
            .map_err(|e| fail("receipt digest")(&e))?,
        admission_receipt: Some(String::from_utf8(receipt).map_err(|e| fail("receipt")(&e))?),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: INSTALLED_AT,
        layout: None,
    };
    Ok((model(artifacts)?, input))
}

/// The normalized model of the set's entity roots and their governing laws.
fn model(artifacts: &[ArtifactEnvelope]) -> Result<Model, SeedError> {
    let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|e| fail(&artifact.artifact_id)(&e))?;
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) | EntityRoot::V2Extension(_) | EntityRoot::V1(_) => {
                return Err(SeedError(format!(
                    "{} is outside the three-systems profile",
                    artifact.artifact_id
                )));
            }
        }
    }
    Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .map_err(|e| fail("model")(&e))?
        .with_governing_laws(artifacts)
        .map_err(|e| fail("governing laws")(&e))
}

/// The idempotency key of the demo rows' one commit: a restarted Durable
/// Object replays it without writing, and nothing is ever wiped.
pub const SEED_ROWS_KEY: &str = "demo.seed.rows/1";

/// Logical time of the demo rows' commit; fixed so every boot is identical.
const SEEDED_AT: u64 = 2;

/// Synthetic catalog items and parties (no real or shop data) in the main
/// world, committed once through the governed write path. The demo grants
/// give the `viewer` role Population alone, so these rows are listed for it
/// with their values withheld (D67), and read whole by `admin`.
pub const SEED_ITEMS: [(&str, &str, &str); 6] = [
    ("SYN-ITEM-01", "BAR-1KG", "Synthetic gold bar, 1 kg"),
    ("SYN-ITEM-02", "BAR-100G", "Synthetic gold bar, 100 g"),
    ("SYN-ITEM-03", "COIN-1OZ", "Synthetic silver coin, 1 oz"),
    ("SYN-ITEM-04", "GRAIN-AU", "Synthetic gold grain"),
    ("SYN-ITEM-05", "SCRAP-AG", "Synthetic silver scrap"),
    ("SYN-ITEM-06", "WIRE-PT", "Synthetic platinum wire"),
];

/// Synthetic parties: id, display name and kind.
pub const SEED_PARTIES: [(&str, &str, &str); 4] = [
    ("SYN-PARTY-01", "Synthetic Refinery A", "organization"),
    ("SYN-PARTY-02", "Synthetic Vault B", "organization"),
    ("SYN-PARTY-03", "Synthetic Assayer", "person"),
    ("SYN-PARTY-04", "Synthetic Ledger Feed", "system"),
];

/// Commit the demo rows once; a restarted cell replays the commit.
fn seed_rows<E: SqlEngine>(store: &mut CellStore<E>, scope: &CellScope) -> Result<(), SeedError> {
    let text = |value: &str| RuntimeValue::from(value);
    let items = SEED_ITEMS
        .iter()
        .map(|(id, code, label)| FactWriteRow::Put {
            object_type: "canon.Item".into(),
            key: (*id).into(),
            values: BTreeMap::from([
                ("id".to_owned(), text(id)),
                ("code".to_owned(), text(code)),
                ("label".to_owned(), text(label)),
            ]),
        });
    let parties = SEED_PARTIES
        .iter()
        .map(|(id, name, kind)| FactWriteRow::Put {
            object_type: "canon.Party".into(),
            key: (*id).into(),
            values: BTreeMap::from([
                ("display_name".to_owned(), text(name)),
                ("kind".to_owned(), text(kind)),
            ]),
        });
    // A seeded cell keeps its one receipt: nothing is re-sent, so a later
    // clock never conflicts with it. A cell that committed past the seed time
    // before it was seeded takes the rows at its clock floor, never behind it.
    let floor = {
        let reader = store.read();
        if reader
            .receipt_by_idempotency_key(MAIN_WORLD, SEED_ROWS_KEY)
            .map_err(|e| fail("demo rows")(&e))?
            .is_some()
        {
            return Ok(());
        }
        reader.clock_floor().map_err(|e| fail("demo rows")(&e))?
    };
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: scope.key(),
            logical_time: SEEDED_AT.max(floor),
            idempotency_key: Some(SEED_ROWS_KEY.into()),
            writes: items.chain(parties).collect(),
            ..CommitBatch::default()
        })
        .map(|_outcome| ())
        .map_err(|e| fail("demo rows")(&e))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #![allow(clippy::unwrap_used, reason = "seed regression")]
    use super::{
        DEFINITIONS, SEED_ITEMS, SEED_PARTIES, SEED_ROWS_KEY, demo_scope, provision_set, seed_rows,
    };
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model};
    use norn_ir::{ArtifactEnvelope, ArtifactKind};
    use std::collections::BTreeMap;

    /// LABEL-LOCALE-1 on the host's real path: admission, model projection,
    /// install and the first commit, for the recorded set with its label
    /// catalog and for the same set without it. The catalog changes the
    /// installed set and its admission receipt (a governed install of its
    /// own), and nothing else: the model, the DDL and every seeded fact's
    /// committed snapshot are the same.
    #[test]
    fn label_catalogs_change_no_model_and_no_seeded_fact() {
        let labelled: Vec<ArtifactEnvelope> = serde_json::from_str(DEFINITIONS).unwrap();
        let plain: Vec<ArtifactEnvelope> = labelled
            .iter()
            .filter(|artifact| artifact.kind() != ArtifactKind::Labels)
            .cloned()
            .collect();
        assert!(
            plain.len() < labelled.len(),
            "the recording carries a label catalog"
        );
        let provisioned = |artifacts: &[ArtifactEnvelope]| {
            let store = provision_set(
                NativeEngine::open_in_memory().unwrap(),
                &demo_scope(),
                artifacts,
            )
            .unwrap();
            let reader = store.read();
            let installed = reader.installed_definitions(None).unwrap().unwrap();
            let seeded = reader
                .receipt_by_idempotency_key(MAIN_WORLD, SEED_ROWS_KEY)
                .unwrap()
                .unwrap();
            let written: Vec<_> = seeded
                .body
                .written
                .iter()
                .map(|row| {
                    (
                        row.object_type.clone(),
                        row.key.clone(),
                        row.version,
                        row.snapshot_sha256.clone(),
                    )
                })
                .collect();
            assert!(written.iter().all(|(_, _, _, snapshot)| snapshot.is_some()));
            (
                (
                    installed.model_hash,
                    installed.ddl_hash,
                    seeded.body.request_hash,
                    written,
                ),
                (installed.receipt_sha256, installed.canonical_set),
            )
        };
        let (plain_model, plain_install) = provisioned(&plain);
        let (labelled_model, labelled_install) = provisioned(&labelled);
        assert_eq!(plain_model, labelled_model);
        assert_eq!(plain_model.3.len(), SEED_ITEMS.len() + SEED_PARTIES.len());
        assert_ne!(plain_install.0, labelled_install.0);
        assert_ne!(plain_install.1, labelled_install.1);
    }

    /// The two seeded types alone, with every property optional.
    fn model() -> Model {
        let item = serde_json::from_value(serde_json::json!({"name":"canon.Item","properties":[
            {"id":"id","type":{"kind":"text"},"constraints":{"nullable":true}},
            {"id":"code","type":{"kind":"text"},"constraints":{"nullable":true}},
            {"id":"label","type":{"kind":"text"},"constraints":{"nullable":true}}]}))
        .unwrap();
        let party = serde_json::from_value(serde_json::json!({"name":"canon.Party","properties":[
            {"id":"display_name","type":{"kind":"text"}},
            {"id":"kind","type":{"kind":"enum","values":[{"value":"person"},{"value":"organization"},{"value":"system"}]}}]}))
        .unwrap();
        Model::from_definitions(&[item, party], &[]).unwrap()
    }

    /// A cell whose clock passed the seed time before it was seeded takes the
    /// rows at its clock floor; a seeded cell sends nothing again.
    #[test]
    fn a_cell_past_the_seed_time_is_seeded_at_its_clock_floor() {
        let mut store = CellStore::open_in_memory(model()).unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: demo_scope().key(),
                logical_time: 1_000,
                writes: vec![FactWriteRow::Put {
                    object_type: "canon.Item".into(),
                    key: "EARLIER".into(),
                    values: BTreeMap::new(),
                }],
                ..CommitBatch::default()
            })
            .unwrap();
        seed_rows(&mut store, &demo_scope()).unwrap();
        let receipt = store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, SEED_ROWS_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.body.logical_time, 1_000);
        let head = store.verified_head().unwrap().commits;
        seed_rows(&mut store, &demo_scope()).unwrap();
        assert_eq!(store.verified_head().unwrap().commits, head);
        let rows = store
            .read()
            .list_after(MAIN_WORLD, "canon.Party", None, 100)
            .unwrap()
            .len();
        assert_eq!(rows, SEED_PARTIES.len());
        assert_eq!(SEED_ITEMS.len(), 6);
    }
}
