#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Declared lifecycle → Mermaid state diagram (T3-LIFE-1, #2672), and the
//! business-facing diagram artifacts with their goldens (T3-LIFE-2, #2673).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_ir::{ObjectTypeDef, PropertyType};
use norn_projections::{
    LIFECYCLE_DIAGRAM_DIR, declared_lifecycle_diagrams, lifecycle_diagram_path,
    lifecycle_state_diagram,
};

const UPDATE_ENV: &str = "NORN_PROJECTIONS_UPDATE_GOLDENS";

fn purchase_order() -> ObjectTypeDef {
    serde_json::from_str(include_str!(
        "../../../fixtures/ir/entity-v2/object-types/purchase-order.object-type.json"
    ))
    .expect("fixture decodes")
}

#[test]
fn a_lifecycle_renders_as_a_state_diagram() {
    let order = purchase_order();
    let PropertyType::Enum { values } = &order.properties[1].property_type else {
        panic!("status is an enum");
    };
    let states: Vec<&str> = values.iter().map(|value| value.value.as_str()).collect();
    assert_eq!(
        lifecycle_state_diagram(&states, &order.lifecycles[0]),
        "stateDiagram-v2
    state \"draft\" as s0
    state \"submitted\" as s1
    state \"approved\" as s2
    state \"rejected\" as s3
    state \"received\" as s4
    state \"cancelled\" as s5
    [*] --> s0
    s0 --> s1 : submit
    s1 --> s2 : approve
    s1 --> s3 : reject
    s3 --> s0 : revise
    s2 --> s4 : receive
    s0 --> s5 : cancel
    s1 --> s5 : cancel
    s2 --> s5 : cancel
    s4 --> [*]
    s5 --> [*]
"
    );
}

#[test]
fn lifecycle_state_labels_are_escaped() {
    let lifecycle = serde_json::from_value(serde_json::json!({
        "property": "stage",
        "initial": ["on \"hold\"; #1"],
        "terminal": ["done: ok"],
        "transitions": [{ "id": "finish", "from": ["on \"hold\"; #1"], "to": "done: ok" }]
    }))
    .expect("decodes");
    assert_eq!(
        lifecycle_state_diagram(&[], &lifecycle),
        "stateDiagram-v2
    state \"on #quot;hold#quot;#59; #35;1\" as s0
    state \"done#58; ok\" as s1
    [*] --> s0
    s0 --> s1 : finish
    s1 --> [*]
"
    );
}

fn rush_purchase_order() -> ObjectTypeDef {
    serde_json::from_str(include_str!(
        "../../../fixtures/ir/entity-v2/object-types/rush-purchase-order.object-type.json"
    ))
    .expect("fixture decodes")
}

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// The diagram artifacts of the fixture lifecycles are the checked-in
/// goldens under `fixtures/projections/lifecycles/`, and nothing else is
/// there. Regenerate with `NORN_PROJECTIONS_UPDATE_GOLDENS=1`.
#[test]
fn lifecycle_diagram_artifacts_match_their_goldens() {
    let diagrams: Vec<_> = [purchase_order(), rush_purchase_order()]
        .iter()
        .flat_map(declared_lifecycle_diagrams)
        .collect();
    let paths: Vec<&str> = diagrams
        .iter()
        .map(|diagram| diagram.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "projections/lifecycles/commerce.PurchaseOrder.status.mmd",
            "projections/lifecycles/commerce.PurchaseOrder.payment_status.mmd",
            "projections/lifecycles/commerce.RushPurchaseOrder.status.mmd",
        ]
    );
    let root = golden_root();
    let update = std::env::var_os(UPDATE_ENV).is_some();
    if update {
        std::fs::create_dir_all(root.join(LIFECYCLE_DIAGRAM_DIR)).expect("golden directory");
    }
    for diagram in &diagrams {
        let path = root.join(&diagram.path);
        if update {
            std::fs::write(&path, &diagram.mermaid).expect("golden writes");
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing golden {}: {error}; regenerate with {UPDATE_ENV}=1",
                path.display()
            )
        });
        assert_eq!(
            golden, diagram.mermaid,
            "{} is stale; regenerate with {UPDATE_ENV}=1 cargo test -p norn-projections --test lifecycle_diagram",
            diagram.path
        );
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(root.join(LIFECYCLE_DIAGRAM_DIR))
        .expect("golden directory")
        .map(|entry| {
            format!(
                "{LIFECYCLE_DIAGRAM_DIR}/{}",
                entry.expect("entry").file_name().to_string_lossy()
            )
        })
        .collect();
    assert_eq!(
        on_disk,
        paths
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<BTreeSet<_>>(),
        "every golden is generated"
    );
}

#[test]
fn a_diagram_artifact_is_titled_and_renders_the_transition_list() {
    let order = purchase_order();
    let diagrams = declared_lifecycle_diagrams(&order);
    let payment = &diagrams[1];
    assert_eq!(
        payment.path,
        lifecycle_diagram_path("commerce.PurchaseOrder", "payment_status")
    );
    assert_eq!(
        payment.mermaid,
        "---
title: commerce.PurchaseOrder payment_status lifecycle
---
stateDiagram-v2
    state \"unpaid\" as s0
    state \"paid\" as s1
    state \"refunded\" as s2
    [*] --> s0
    s0 --> s1 : pay
    s1 --> s2 : refund
    s2 --> [*]
"
    );
}

#[test]
fn a_lifecycle_over_an_inherited_property_has_no_declared_diagram() {
    let mut rush = rush_purchase_order();
    // Drop the local status override: the redeclared lifecycle now governs an
    // inherited property, whose states only the checker's effective view has.
    rush.properties.clear();
    assert_eq!(declared_lifecycle_diagrams(&rush), Vec::new());
}
