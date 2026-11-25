//! PRINCIPAL-INPUT-1 (#1091) on `norn dev`: the call record's
//! `principalInputs` column joins the cell with the first saved set that
//! declares principal inputs and stays under every later save, so saving a
//! set without them (a rollback) installs, and a restarted session reopens
//! the cell under the model its installed revision recorded.
use std::sync::{Arc, RwLock};

use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::CellStore;
use norn_cell::engine::NativeEngine;
use serde_json::{Value, json};

use super::{admit, keeps};
use crate::dev::http::Serving;
use crate::dev::{checkpoint_key, definitions, policy};

#[path = "../../tests/support/review_pack.rs"]
mod review_pack;
use review_pack::{admin_role, approval_rule, approve_action, review_base};

const ACTOR: &str = "local:developer";

fn scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}

/// The review pack: with `ruled`, its approval pins the decision that reads
/// `principal.isAdmin`; without, no artifact declares a principal input.
fn pack(ruled: bool) -> Vec<Value> {
    let mut pack = review_base(&admin_role());
    if ruled {
        pack.extend([
            approval_rule("0.1.0", true),
            approve_action("0.2.0", Some("0.1.0")),
        ]);
    } else {
        pack.push(approve_action("0.1.0", None));
    }
    pack
}

/// Save `artifacts` as `norn dev` does: admitted against what the live cell
/// serves, then committed over the active revision. The new version.
fn saved(serving: &Serving, actions: &policy::DevActions, artifacts: &[Value], at: u64) -> u64 {
    let admitted = admit(artifacts, at, keeps(serving))
        .map_err(|refused| refused.diagnostics)
        .expect("admitted");
    let guard = serving.write().expect("serving");
    let host = guard.as_ref().expect("host");
    let active = host
        .active_definitions()
        .expect("definitions")
        .expect("active");
    let committed = definitions::commit(
        host,
        ACTOR,
        &policy::dev_kinds(),
        &[],
        admitted,
        &active,
        actions,
        at,
    );
    match committed {
        Ok(definitions::Committed::Installed(installed, _events)) => installed.version,
        Ok(definitions::Committed::Unchanged) => panic!("the save changed nothing"),
        Err(rejected) => panic!("{}: {}", rejected.phase, json!(rejected.diagnostics)),
    }
}

#[test]
fn a_dev_cell_keeps_principal_inputs_through_a_rollback_and_a_restart() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let serving: Serving = Arc::new(RwLock::new(None));
    let actions = policy::dev_actions();

    // The first save provisions the cell; nothing declares a principal input.
    let first = admit(&pack(false), 1_000, keeps(&serving))
        .map_err(|refused| refused.diagnostics)
        .expect("admitted");
    let engine =
        NativeEngine::open(&path, checkpoint_key::dev_store_options(&path)).expect("engine");
    let store = CellStore::provision_definitions(engine, first.model, &first.input, &scope().key())
        .expect("provisioned");
    let host =
        DevHost::from_store(store, scope(), |command| command.payload.clone()).expect("host");
    let base = host
        .active_definitions()
        .expect("definitions")
        .expect("active")
        .version;
    *serving.write().expect("serving") = Some(host);
    assert!(!keeps(&serving));

    // A save that declares principal inputs: the column joins the cell.
    assert_eq!(saved(&serving, &actions, &pack(true), 2_000), base + 1);
    assert!(keeps(&serving));
    // The rollback: the set declares none again, and it still installs.
    assert_eq!(saved(&serving, &actions, &pack(false), 3_000), base + 2);
    assert!(keeps(&serving));

    // A restarted session reopens the cell under the model the installed
    // revision recorded, with the kept column.
    drop(serving);
    let (reopened, _loaded) = crate::dev::reopen(&path, &scope(), ACTOR, &[], &actions)
        .map_err(|failure| failure.message)
        .expect("the cell reopens");
    let host = reopened.expect("an installed cell");
    assert!(host.serving().expect("serving").principal_inputs);
    assert_eq!(
        host.active_definitions()
            .expect("definitions")
            .expect("active")
            .version,
        base + 2
    );
}

/// A catalog the action policy could not compile is refused by dev admission
/// too, before anything is installed.
#[test]
fn dev_admission_refuses_a_catalog_its_access_rules_contradict() {
    let owners = json!({ "ruleId": "owners", "actions": [review_pack::APPROVE],
        "resourceType": review_pack::DOCUMENT,
        "scope": { "dimensions": ["org", "environment"] },
        "allOf": [{ "resource": { "name": "owner", "op": "eq",
            "value": { "principalAttribute": "id" } } }] });
    let with_catalog = |kind: &str| {
        let catalog = json!([{ "name": "id", "type": kind, "source": { "kind": "attribute" } }]);
        let mut pack = review_base(&catalog);
        pack[1]["payload"]["rules"]
            .as_array_mut()
            .expect("rules")
            .push(owners.clone());
        pack.push(approve_action("0.1.0", None));
        pack
    };
    let refused = admit(&with_catalog("long"), 1_000, false)
        .map(|_admitted| ())
        .map_err(|refused| refused.diagnostics)
        .expect_err("refused");
    assert_eq!(
        refused[0]["code"], "artifact_set.policy.principal_catalog_conflict",
        "{refused:?}"
    );
    assert!(admit(&with_catalog("string"), 1_000, false).is_ok());
}
