//! The ml pack's promotion sign-off (#1171 M3): `registry/packs/ml/promote.approval.json`
//! is an APPROVAL-1 definition a hosted cell admits. It gates an action the
//! pack declares, and its deciders are roles of the hosted grant table, so an
//! admin or owner decides a promotion. A dev host declares no roles, so it
//! installs no approvals.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "fixture correspondence"
)]
use std::collections::BTreeSet;

use norn_api::approval_definitions::check;
use norn_cell_do::policy::DEMO_GRANTS;
use serde_json::Value;

const APPROVAL: &str = include_str!("../../../registry/packs/ml/promote.approval.json");
const PROMOTE: &str =
    include_str!("../../../registry/packs/ml/artifacts/ml.modelversion.promote.action.json");

fn definition() -> Value {
    serde_json::from_str(APPROVAL).expect("approval json")
}

fn pack_actions() -> BTreeSet<String> {
    let promote: Value = serde_json::from_str(PROMOTE).expect("promote action");
    BTreeSet::from([promote["payload"]["actionId"]
        .as_str()
        .expect("action id")
        .to_owned()])
}

#[test]
fn the_promotion_sign_off_is_admitted_on_a_hosted_cell() {
    let definition = definition();
    assert_eq!(definition["kind"], "approval");
    assert_eq!(definition["id"], "ml.modelversion.promote.approval");
    assert_eq!(definition["payload"]["action"], "ml.modelversion.promote");
    let roles: Vec<&str> = DEMO_GRANTS.entries.iter().map(|(role, _)| *role).collect();
    let findings = check(
        &definition["payload"],
        &pack_actions(),
        &roles,
        &BTreeSet::new(),
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn a_cell_without_those_roles_or_that_action_refuses_it() {
    let definition = definition();
    let no_roles = check(
        &definition["payload"],
        &pack_actions(),
        &[],
        &BTreeSet::new(),
    );
    assert!(!no_roles.is_empty(), "a dev host declares no roles");
    let roles: Vec<&str> = DEMO_GRANTS.entries.iter().map(|(role, _)| *role).collect();
    let no_action = check(
        &definition["payload"],
        &BTreeSet::new(),
        &roles,
        &BTreeSet::new(),
    );
    assert!(!no_action.is_empty(), "the gated action must be admitted");
}
