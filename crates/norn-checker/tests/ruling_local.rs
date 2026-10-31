#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Local (T2) checks of `claim` and `ruling` artifacts (norn#62): each
//! document rule refuses its malformed case, and the valid documents pass.

use norn_checker::ruling::codes::{INVALID_CLAIM, INVALID_RULING};
use norn_checker::{Diagnostic, check_artifact_json};
use serde_json::{Value, json};

fn envelope(kind: &str, id: &str, payload: &Value) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": kind,
        "namespace": "commerce",
        "name": id,
        "version": "0.1.0",
        "lifecycle": "proposed",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": payload
    })
}

fn state(state: &str) -> Value {
    json!({
        "artifactId": "commerce.SalesOrder.entity",
        "version": "0.1.0",
        "member": {
            "kind": "state",
            "typeName": "commerce.SalesOrder",
            "lifecycle": "status",
            "state": state
        }
    })
}

fn derived_claim() -> Value {
    envelope(
        "claim",
        "commerce.sales_order_invoiced_is_derived.claim",
        &json!({
            "claimVersion": "0.1",
            "id": "commerce.sales_order_invoiced_is_derived",
            "statement": {
                "kind": "derived_state",
                "state": state("invoiced"),
                "from": [{
                    "artifactId": "commerce.SalesOrder.entity",
                    "version": "0.1.0",
                    "member": { "kind": "link", "typeName": "commerce.SalesOrder", "link": "lines" }
                }]
            },
            "evidence": [{
                "source": {
                    "kind": "upstream",
                    "system": "erpnext",
                    "release": "v16.0.0",
                    "locator": "erpnext/selling/doctype/sales_order/sales_order.py#set_status"
                },
                "strength": "structural"
            }]
        }),
    )
}

fn total(version: &str) -> Value {
    json!({
        "artifactId": "commerce.SalesOrder.entity",
        "version": version,
        "member": { "kind": "property", "typeName": "commerce.SalesOrder", "property": "total_amount" }
    })
}

fn ruling() -> Value {
    envelope(
        "ruling",
        "commerce.order_total_basis.ruling",
        &json!({
            "rulingVersion": "0.1",
            "id": "commerce.order_total_basis",
            "resolves": [{ "artifactId": "commerce.order_total_divergence.claim", "version": "0.1.0" }],
            "outcome": { "kind": "choose", "definition": total("0.1.0") },
            "affects": [total("0.1.0")],
            "decidedBy": { "principal": "owner", "authority": "owner" },
            "decidedAt": "2026-10-31",
            "alternatives": [{ "outcome": { "kind": "reject" }, "note": "keep both" }]
        }),
    )
}

fn findings(artifact: &Value) -> Vec<(String, String)> {
    check_artifact_json(artifact)
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn refused(code: &str, pointer: &str) -> Vec<(String, String)> {
    vec![(code.to_owned(), pointer.to_owned())]
}

#[test]
fn valid_claims_and_rulings_pass() {
    assert_eq!(findings(&derived_claim()), []);
    assert_eq!(findings(&ruling()), []);
}

#[test]
fn a_claim_states_its_evidence_and_names_the_right_members() {
    let mut claim = derived_claim();
    claim["payload"]["evidence"] = json!([]);
    assert_eq!(
        findings(&claim),
        refused(INVALID_CLAIM, "/payload/evidence")
    );

    let mut claim = derived_claim();
    claim["payload"]["evidence"][0]["source"] =
        json!({ "kind": "artifact", "artifactId": "x", "version": "1", "sha256": "ABC" });
    assert_eq!(
        findings(&claim),
        refused(INVALID_CLAIM, "/payload/evidence/0/source")
    );

    let mut claim = derived_claim();
    claim["payload"]["statement"]["state"] = total("0.1.0");
    assert_eq!(
        findings(&claim),
        refused(INVALID_CLAIM, "/payload/statement/state/member")
    );

    let mut claim = derived_claim();
    claim["payload"]["statement"]["from"] = json!([]);
    assert_eq!(
        findings(&claim),
        refused(INVALID_CLAIM, "/payload/statement/from")
    );

    let mut claim = derived_claim();
    claim["payload"]["note"] = json!(null);
    assert_eq!(findings(&claim), refused(INVALID_CLAIM, "/payload"));
}

#[test]
fn a_divergence_needs_two_different_observations_of_one_property() {
    let observation = json!({
        "mapping": { "artifactId": "erpnext.sales_order.mapping", "version": "0.1.0" },
        "map": "sales_order",
        "record": { "name": "SO-1", "grand_total": "114.00" }
    });
    let mut claim = derived_claim();
    claim["payload"]["statement"] = json!({
        "kind": "divergence",
        "witness": { "target": total("0.1.0"), "left": observation, "right": observation }
    });
    assert_eq!(
        findings(&claim),
        refused(INVALID_CLAIM, "/payload/statement/witness")
    );
}

#[test]
fn a_ruling_outcome_names_only_what_it_affects() {
    let mut other = ruling();
    other["payload"]["outcome"]["definition"] = total("0.2.0");
    assert_eq!(
        findings(&other),
        refused(INVALID_RULING, "/payload/outcome/definition")
    );

    let mut scoped = ruling();
    scoped["payload"]["outcome"] = json!({ "kind": "scoped", "cases": [{ "scope": { "tenantId": "a" }, "definition": total("0.1.0") }] });
    assert_eq!(
        findings(&scoped),
        refused(INVALID_RULING, "/payload/outcome/cases")
    );

    let mut parameter = ruling();
    parameter["payload"]["outcome"] = json!({ "kind": "parameterise", "parameter": total("0.1.0"), "default": "net", "setBy": [] });
    assert_eq!(
        findings(&parameter),
        refused(INVALID_RULING, "/payload/outcome/setBy")
    );
}

#[test]
fn a_ruling_is_dated_decided_and_never_its_own_successor() {
    let mut undated = ruling();
    undated["payload"]["decidedAt"] = json!("31/10/2026");
    assert_eq!(
        findings(&undated),
        refused(INVALID_RULING, "/payload/decidedAt")
    );

    let mut empty = ruling();
    empty["payload"]["resolves"] = json!([]);
    assert_eq!(
        findings(&empty),
        refused(INVALID_RULING, "/payload/resolves")
    );

    let mut itself = ruling();
    itself["payload"]["supersedes"] =
        json!({ "artifactId": "commerce.order_total_basis.ruling", "version": "0.1.0" });
    assert_eq!(
        findings(&itself),
        refused(INVALID_RULING, "/payload/supersedes")
    );

    let mut same = ruling();
    same["payload"]["alternatives"] = json!([{ "outcome": same["payload"]["outcome"].clone() }]);
    assert_eq!(
        findings(&same),
        refused(INVALID_RULING, "/payload/alternatives/0/outcome")
    );
}

#[test]
fn scoped_cases_are_disjoint_and_revisit_evidence_is_pinned() {
    let mut overlapping = ruling();
    overlapping["payload"]["outcome"] = json!({ "kind": "scoped", "cases": [
        { "scope": { "tenantId": "a" }, "definition": total("0.1.0") },
        { "scope": { "tenantId": "a" }, "definition": total("0.1.0") }
    ]});
    assert_eq!(
        findings(&overlapping),
        refused(INVALID_RULING, "/payload/outcome/cases/1/scope")
    );
    let mut disjoint = overlapping.clone();
    disjoint["payload"]["outcome"]["cases"][1]["scope"] = json!({ "tenantId": "b" });
    assert_eq!(findings(&disjoint), []);

    let mut deferred = ruling();
    deferred["payload"]["outcome"] = json!({ "kind": "defer", "revisit": { "kind": "evidence",
        "source": { "kind": "artifact", "artifactId": "", "version": "", "sha256": "bad" } } });
    assert_eq!(
        findings(&deferred),
        refused(INVALID_RULING, "/payload/outcome/revisit/source")
    );
}

#[test]
fn references_name_something_and_dates_exist() {
    for date in ["2026-02-29", "2026-02-31", "2026-04-31", "2026-13-01"] {
        let mut undated = ruling();
        undated["payload"]["decidedAt"] = json!(date);
        assert_eq!(
            findings(&undated),
            refused(INVALID_RULING, "/payload/decidedAt"),
            "{date}"
        );
    }
    let mut leap = ruling();
    leap["payload"]["decidedAt"] = json!("2028-02-29");
    assert_eq!(findings(&leap), []);

    let mut blank_claim = ruling();
    blank_claim["payload"]["resolves"] = json!([{ "artifactId": "", "version": "" }]);
    assert_eq!(
        findings(&blank_claim),
        refused(INVALID_RULING, "/payload/resolves/0")
    );

    let mut blank_state = derived_claim();
    blank_state["payload"]["statement"]["state"] = state("");
    assert_eq!(
        findings(&blank_state),
        refused(INVALID_CLAIM, "/payload/statement/state")
    );
}

#[test]
fn a_composition_pack_may_ship_claims_and_rulings() {
    let pack = envelope(
        "business_pack",
        "commerce.rulings.pack",
        &json!({
            "packId": "commerce.rulings", "packVersion": "0.1.0", "packKind": "composition",
            "artifacts": [
                { "artifactId": "commerce.sales_order_invoiced_is_derived.claim", "kind": "claim", "version": "0.1.0" },
                { "artifactId": "commerce.order_total_basis.ruling", "kind": "ruling", "version": "0.1.0" }
            ]
        }),
    );
    assert_eq!(findings(&pack), []);
}
