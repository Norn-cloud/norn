//! The admitted constitution melt cell (`constitution.melt.convert` writing
//! `constitution.Melt` under the `constitution.fine_mass` law), shared by the
//! action service tests and the HTTP route tests. Only public crate APIs are
//! used, so both crates include this file by path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "fixture admission must succeed before any assertion"
)]
use std::collections::BTreeSet;

use norn_cell::{CellStore, InstalledDefinitionInput, Model, engine::NativeEngine};
use norn_ir::RuntimeValue;
use serde_json::{Value, json};

/// The admitted action identity.
pub(crate) const MELT_ACTION: &str = "constitution.melt.convert";
/// The object type it writes.
pub(crate) const MELT: &str = "constitution.Melt";
/// The admitted writer derives the loss, so the fine-mass law always holds.
pub(crate) const DERIVES_LOSS: &str = "withLoss";
/// The admitted writer keeps the default loss of 0: any real loss breaks the law.
pub(crate) const OMITS_LOSS: &str = "withoutLoss";

/// Base and candidate artifacts of the fixture's `set`, in admission order.
fn documents(set: &str) -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../norn-cell/tests/fixtures/constitution.json"
    ))
    .unwrap();
    let mut documents: Vec<Value> = [
        include_str!("../../../../registry/core/artifacts/core.units.json"),
        include_str!("../../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../../registry/core/artifacts/core.Conversion.interface.json"),
    ]
    .into_iter()
    .map(|text| serde_json::from_str(text).unwrap())
    .collect();
    documents.extend(fixture[set].as_array().unwrap().iter().cloned());
    documents
}

/// A provisioned in-memory store for `org_demo/dev` whose installed,
/// receipt-verified definitions are the fixture's `set`. `prepare` registers the
/// kernel's own action types in the model (the API's `prepare_action_model`).
pub(crate) fn melt_store<P, X>(set: &str, prepare: P) -> CellStore<NativeEngine>
where
    P: FnOnce(&Model) -> Result<Model, X>,
    X: std::fmt::Debug,
{
    admitted_store(&documents(set), prepare)
}

/// A provisioned in-memory store for `org_demo/dev` whose installed,
/// receipt-verified definitions are `documents`, admitted by the checker.
pub(crate) fn admitted_store<P, X>(documents: &[Value], prepare: P) -> CellStore<NativeEngine>
where
    P: FnOnce(&Model) -> Result<Model, X>,
    X: std::fmt::Debug,
{
    let mut types = Vec::new();
    let mut properties = Vec::new();
    let mut interfaces = Vec::new();
    for document in documents {
        let payload = &document["payload"];
        if let Some(object_type) = payload.get("objectType") {
            types.push(serde_json::from_value(object_type.clone()).unwrap());
        }
        if let Some(interface) = payload.get("interface") {
            interfaces.push(serde_json::from_value(interface.clone()).unwrap());
        }
        for property_type in payload
            .get("propertyTypes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            properties.push(serde_json::from_value(property_type.clone()).unwrap());
        }
    }
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(Value::Array(documents.to_vec())).unwrap();
    let model = Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
        .unwrap()
        .with_governing_laws(&artifacts)
        .unwrap();
    let model = prepare(&model).unwrap();
    let sources: Vec<RuntimeValue> = documents
        .iter()
        .cloned()
        .map(RuntimeValue::from_json)
        .collect::<Result<_, _>>()
        .unwrap();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &[]).unwrap(),
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let input = InstalledDefinitionInput {
        canonical_set: serde_json::to_string(documents).unwrap(),
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256().unwrap(),
        admission_receipt: Some(
            String::from_utf8(checked.receipt.canonical_json().unwrap()).unwrap(),
        ),
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    };
    CellStore::provision_definitions(
        NativeEngine::open_in_memory().unwrap(),
        model,
        &input,
        "org_demo/dev",
    )
    .unwrap()
}

/// A request body for one melt. Exact decimals travel as their text (VAL-EXACT-1).
#[must_use]
pub(crate) fn melt_body(id: &str, input: &str, output: &str, key: &str) -> String {
    format!(
        r#"{{"input":{{"id":"{id}","input_fine_g":"{input}","output_fine_g":"{output}"}},"idempotencyKey":"{key}"}}"#
    )
}
