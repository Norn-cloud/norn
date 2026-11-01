#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! One call of a `fire_transition` action on an existing object (T3-LIFE-3,
//! #2867): `norn_checker::check_action_firing` over the cases in
//! `fixtures/ir/action-firing/*.case.json`.
//!
//! Each case names an artifact set that the set check admits, the action
//! artifact the agent calls, and the object it is called on (the artifact of
//! its type and its stored values), and lists the exact `(code, pointer)`
//! findings of the call. The demo's agent refusal is
//! `submit-invoice-before-approval`: `submit_invoice` on a sales order that is
//! still `draft` is `lifecycle.validation.undeclared_write`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use norn_checker::{
    EffectiveCatalog, EntityArtifactSetContext, check_action_firing, check_artifact_set,
};
use norn_ir::{ActionRoot, ArtifactEnvelope, JsonObject, PropertyTypeName};
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("fixture should be readable"))
        .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()))
}

fn cases() -> Vec<(String, Value)> {
    let dir = fixtures().join("ir/action-firing");
    let mut out: Vec<(String, Value)> = fs::read_dir(&dir)
        .expect("case directory should be readable")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.to_string_lossy().ends_with(".case.json"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            (name, read_json(&path))
        })
        .collect();
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

/// `(code, pointer)` of every finding of the case's call.
fn run(name: &str, case: &Value) -> Vec<Vec<String>> {
    let documents: Vec<Value> = case["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .map(|path| read_json(&fixtures().join(path.as_str().expect("fixture path"))))
        .collect();
    let artifacts: Vec<ArtifactEnvelope> = documents
        .iter()
        .map(|document| serde_json::from_value(document.clone()).expect("envelope decodes"))
        .collect();
    let registry = BTreeSet::<PropertyTypeName>::new();
    assert_eq!(
        check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &registry,
                base: &[],
                tenant: None,
            },
        ),
        Vec::new(),
        "{name}: the artifact set is admitted; the refusal is the call's"
    );
    let action_id = case["action"].as_str().expect("action artifact id");
    let action: ActionRoot = serde_json::from_value(
        documents
            .iter()
            .find(|document| document["artifactId"] == action_id)
            .expect("the action is in the set")["payload"]
            .clone(),
    )
    .expect("action payload decodes");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let object_type = catalog
        .object_type(
            case["object"]["entityArtifactId"]
                .as_str()
                .expect("object type artifact"),
        )
        .expect("the object's type is defined")
        .expect("the object's type is available");
    let values: JsonObject =
        serde_json::from_value(case["object"]["values"].clone()).expect("values decode");
    check_action_firing(&action, object_type, &values)
        .into_iter()
        .map(|finding| {
            vec![
                finding.code.as_str().to_owned(),
                finding.span.map(|span| span.pointer).unwrap_or_default(),
            ]
        })
        .collect()
}

#[test]
fn every_firing_case_reports_exactly_its_expected_findings() {
    let cases = cases();
    assert!(cases.len() >= 3, "expected the action-firing case fixtures");
    for (name, case) in &cases {
        let expected: Vec<Vec<String>> =
            serde_json::from_value(case["expect"].clone()).expect("expect is a list of pairs");
        assert_eq!(run(name, case), expected, "{name}");
    }
}

/// The demo's agent refusal is pinned: `submit_invoice` before approval is
/// `lifecycle.validation.undeclared_write`, and after approval it fires.
#[test]
fn submit_invoice_before_approval_is_an_undeclared_write() {
    let cases = cases();
    let outcome = |file: &str| {
        let (name, case) = cases
            .iter()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("{file} is missing"));
        run(name, case)
    };
    assert_eq!(
        outcome("submit-invoice-before-approval.case.json"),
        vec![vec![
            norn_checker::lifecycles::codes::UNDECLARED_WRITE.to_owned(),
            "/payload/effectProgram/transition".to_owned(),
        ]]
    );
    assert_eq!(
        outcome("submit-invoice-after-approval.case.json"),
        Vec::<Vec<String>>::new()
    );
}

#[test]
fn receipt_effect_admits_reference_and_writer_closure_but_refuses_invalid_bindings() {
    let mut entity =
        read_json(&fixtures().join("ir/entity-v2/artifact-set/commerce-sales-order.entity.json"));
    entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .extend([
            serde_json::json!({"id":"customer", "type":{"kind":"text"}}),
            serde_json::json!({"id":"receipt_digest", "type":{"kind":"text"}}),
        ]);
    entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(serde_json::json!({"id":"receipt_key","type":{"kind":"text"}}));
    entity["payload"]["objectType"]["identity"][0]["properties"]
        .as_array_mut()
        .expect("identity")
        .push(serde_json::json!("receipt_key"));
    let mut action =
        read_json(&fixtures().join("ir/commerce-sales-order-submit-invoice.action.json"));
    action["payload"]["inputSchema"]["properties"]["receipt"] =
        serde_json::json!({"type":"string"});
    action["payload"]["inputSchema"]["required"]
        .as_array_mut()
        .expect("required")
        .push(serde_json::json!("receipt"));
    action["payload"]["effectProgram"]["acceptReceipt"] = serde_json::json!({
        "referenceField":"receipt", "manifestDigest":format!("sha256:{}", "a".repeat(64)),
        "digestProperty":"receipt_digest", "resultBindings":{"customer":"customer"}
    });
    let check = |action: &Value| {
        let artifacts = [
            serde_json::from_value(entity.clone()).expect("entity"),
            serde_json::from_value(action.clone()).expect("action"),
        ];
        check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &[],
                tenant: None,
            },
        )
    };
    assert!(check(&action).is_empty(), "{:?}", check(&action));
    for target in [
        "status",
        "missing",
        "_norn_private",
        "so_number",
        "receipt_digest",
        "receipt_key",
    ] {
        let mut invalid = action.clone();
        invalid["payload"]["effectProgram"]["acceptReceipt"]["resultBindings"] =
            serde_json::json!({target:"customer"});
        assert!(
            check(&invalid)
                .iter()
                .any(|finding| finding.code.as_str() == "artifact_set.action.target_not_writable")
        );
    }
    proptest::test_runner::TestRunner::new(proptest::test_runner::Config::with_cases(32))
        .run(&"[a-z]{1,16}", |suffix| {
            let mut invalid = action.clone();
            let target = format!("unknown_{suffix}");
            invalid["payload"]["effectProgram"]["acceptReceipt"]["resultBindings"] =
                serde_json::json!({target:"customer"});
            proptest::prop_assert!(!check(&invalid).is_empty());
            Ok(())
        })
        .expect("generated undeclared receipt targets always refuse");
    let mut identity = action.clone();
    for target in ["so_number", "receipt_key"] {
        identity["payload"]["effectProgram"]["acceptReceipt"]["digestProperty"] =
            serde_json::json!(target);
        assert!(!check(&identity).is_empty());
    }
    let mut null = action.clone();
    null["payload"]["effectProgram"]["acceptReceipt"] = Value::Null;
    assert!(!check(&null).is_empty());
    let mut body = action.clone();
    body["payload"]["inputSchema"]["properties"]["receipt"] =
        serde_json::json!({"type":"object", "properties":{}, "additionalProperties":false});
    assert!(!check(&body).is_empty());
    let mut open = action.clone();
    open["payload"]["writes"] = serde_json::json!([]);
    assert!(!check(&open).is_empty());
}
