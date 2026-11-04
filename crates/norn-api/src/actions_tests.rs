//! Stock Receive uses the same retained admission and action service as transports.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "stock action correspondence setup and assertions"
)]
use super::*;
use crate::authz::{Authorizer, DenyAll};
use norn_cell::{CellStore, InstalledDefinitionInput, Model, engine::NativeEngine};
use norn_ir::{ObjectTypeDef, RuntimeValue};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
    CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
struct ResolvedPrincipal;
impl Authorizer for ResolvedPrincipal {
    fn authorize(&self, r: &AccessRequest<'_>) -> Result<(), Refusal> {
        if r.actor == Some("principal:operator") {
            Ok(())
        } else {
            DenyAll.authorize(r)
        }
    }
    fn action(&self, r: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if r.actor == "principal:operator" {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read(&self, r: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if r.actor == "principal:operator" {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(&self, r: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if r.actor == "principal:operator" {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
}
fn stock_host() -> DevHost<NativeEngine> {
    let mut documents: Vec<Value> = serde_json::from_str(include_str!(
        "../../norn-cell/tests/fixtures/rea-inventory.json"
    ))
    .unwrap();
    let helpers: Vec<Value> = serde_json::from_str(include_str!(
        "../../norn-decisions/tests/fixtures/jewelry-folds.json"
    ))
    .unwrap();
    documents.extend(
        helpers
            .into_iter()
            .filter(|d| d["artifactId"] == "jewelry.fold.allocate_step.rule"),
    );
    for name in [
        "core.Agent.interface",
        "core.Resource.interface",
        "core.Commitment.interface",
        "core.Hold.interface",
        "core.EconomicEvent.interface",
        "core.LedgerEffect.interface",
        "core.Claim.interface",
        "core.Process.interface",
        "core.Exchange.interface",
        "core.value_types.entity",
        "std.value_types.entity",
        "core.units",
    ] {
        documents.push(
            serde_json::from_str(
                &std::fs::read_to_string(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../registry/core/artifacts")
                        .join(format!("{name}.json")),
                )
                .unwrap(),
            )
            .unwrap(),
        );
    }
    let mut types = Vec::new();
    let mut properties = Vec::new();
    let mut interfaces = Vec::new();
    for document in &documents {
        let payload = &document["payload"];
        if let Some(t) = payload.get("objectType") {
            types.push(serde_json::from_value(t.clone()).unwrap());
        }
        if let Some(t) = payload.get("interface") {
            interfaces.push(serde_json::from_value(t.clone()).unwrap());
        }
        if let Some(ts) = payload.get("propertyTypes").and_then(Value::as_array) {
            for t in ts {
                properties.push(serde_json::from_value(t.clone()).unwrap());
            }
        }
    }

    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(Value::Array(documents.clone())).unwrap();
    let model = Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
        .unwrap()
        .with_governing_laws(&artifacts)
        .unwrap();
    let model = prepare_action_model(&model).unwrap();
    let sources: Vec<_> = documents
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
    let scope = crate::scope::CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let input = InstalledDefinitionInput {
        canonical_set: serde_json::to_string(&documents).unwrap(),
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
    let store = CellStore::provision_definitions(
        NativeEngine::open_in_memory().unwrap(),
        model,
        &input,
        &scope.key(),
    )
    .unwrap();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(ResolvedPrincipal);
    let mut seed = CellCommit::empty();
    for (kind, id, values) in [
        (
            "Agent",
            "BUSINESS",
            json!({"id":"BUSINESS","display_name":"Synthetic business","kind":"organization"}),
        ),
        (
            "Agent",
            "PARTY",
            json!({"id":"PARTY","display_name":"Synthetic party","kind":"person"}),
        ),
        ("Vault", "VAULT", json!({"id":"VAULT","company":"BUSINESS"})),
    ] {
        seed.writes.push(FactWrite {
            key: FactKey::new(format!("inventory.Cycle{kind}"), id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(values).unwrap(),
            },
        });
    }
    host.cell().unwrap().commit(seed).unwrap();
    host
}
fn query() -> DefinitionsQuery {
    DefinitionsQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        version: None,
    }
}
fn request(
    host: &DevHost<NativeEngine>,
    action: &str,
    key: &str,
    input: Value,
    version: u64,
) -> ActionRequest {
    let installed = host
        .cell()
        .unwrap()
        .store()
        .read()
        .installed_definitions(None)
        .unwrap()
        .unwrap();
    let admitted =
        crate::action_admission::RetainedActions::load(&installed, host.scope()).unwrap();
    let id = admitted
        .actions
        .iter()
        .find(|(_, a)| a.action_id.ends_with(action))
        .unwrap()
        .1
        .action_id
        .clone();
    ActionRequest {
        action_id: id,
        input: serde_json::from_value(input).unwrap(),
        idempotency_key: key.into(),
        version,
    }
}
fn call(
    host: &DevHost<NativeEngine>,
    action: &str,
    key: &str,
    input: Value,
    actor: Option<&str>,
    version: u64,
) -> Result<ActionResponse, Refusal> {
    call_action_in_epoch(
        host,
        &query(),
        actor,
        request(host, action, key, input, version),
    )
}
#[test]
fn stock_receive_commits_replays_and_refuses_before_partial_effects() {
    let host = stock_host();
    let actor = Some("principal:operator");
    call(
        &host,
        "createexchange",
        "draft",
        json!({"id":"RECEIPT-R1"}),
        actor,
        1,
    )
    .unwrap();
    let input = json!({"id":"RECEIPT-R1","holding":"R1","vault":"VAULT","company":"BUSINESS","party":"PARTY","event":"R1-EVENT","debit":"R1-INVENTORY","credit":"R1-RNB","quantity":2,"rate":10,"valuation_method":"fifo"});
    let before = host.cell().unwrap().store().read().dump().unwrap();
    assert_eq!(
        call(&host, "receive", "receive", input.clone(), None, 1)
            .err()
            .unwrap()
            .status,
        403
    );
    assert_eq!(
        call(&host, "receive", "receive", input.clone(), actor, 99)
            .err()
            .unwrap()
            .code,
        "defs.stale"
    );
    assert_eq!(host.cell().unwrap().store().read().dump().unwrap(), before);
    let accepted = call(&host, "receive", "receive", input.clone(), actor, 1).unwrap();
    assert!(!accepted.replayed);
    let after = host.cell().unwrap().store().read().dump().unwrap();
    let replayed = call(&host, "receive", "receive", input.clone(), actor, 1).unwrap();
    assert!(replayed.replayed);
    assert_eq!(
        serde_json::to_string(&accepted.record).unwrap(),
        serde_json::to_string(&replayed.record).unwrap()
    );
    assert_eq!(host.cell().unwrap().store().read().dump().unwrap(), after);
    let denied = call(
        &host,
        "receive",
        "receive",
        input.clone(),
        Some("principal:denied"),
        1,
    )
    .err()
    .unwrap();
    assert_eq!(denied.status, 403);
    assert_eq!(denied.code, "authz.denied");
    assert_eq!(host.cell().unwrap().store().read().dump().unwrap(), after);
    let mut conflicting = input;
    conflicting["quantity"] = json!(3);
    let refused = call(&host, "receive", "receive", conflicting, actor, 1)
        .err()
        .unwrap();
    assert_eq!(refused.status, 409);
    assert_eq!(refused.code, "action.idempotency_conflict");
    assert_eq!(host.cell().unwrap().store().read().dump().unwrap(), after);
}

#[test]
fn admission_refuses_sqlite_equivalent_internal_type_names() {
    let object: ObjectTypeDef = serde_json::from_value(json!({"name":"norn.Actioncall"})).unwrap();
    let model = Model::from_definitions(&[object], &[]).unwrap();
    assert_eq!(prepare_action_model(&model).err().unwrap().status, 400);
}

#[test]
fn host_permission_does_not_replace_kernel_authority() {
    struct HostOnly;
    impl Authorizer for HostOnly {
        fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }
    }
    let host = stock_host().with_authorizer(HostOnly);
    let before = host.cell().unwrap().store().read().dump().unwrap();
    let refused = call(
        &host,
        "createexchange",
        "no-kernel-grant",
        json!({"id":"DENIED"}),
        Some("principal:operator"),
        1,
    )
    .err()
    .unwrap();
    assert_eq!(refused.status, 403);
    assert_eq!(host.cell().unwrap().store().read().dump().unwrap(), before);
}
