#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "PARAM-TAX integration fixtures"
)]
//! PARAM-TAX / PARAM-PIN-1: governed settings, exact evaluation and durable receipts.
mod common;
use norn_cell::{SqliteCell, StoreOptions};
use norn_ir::{
    ActionRoot, ArtifactEnvelope, ObjectTypeDef, Property, PropertyId, PropertyType, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionCallRecord,
    ActionDenial, ActionOutcome, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellHost,
    DecisionTurn, FactEvent, FactKey, LogicalTime, TypeCatalog, World, commit_action,
};
use std::{collections::BTreeMap, path::Path, process::Command};
type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
struct Policy;
impl ActionAuthorizer for Policy {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.action.action_id == "demo.tax.set" && request.actor != "config:reviewer" {
            return Err(ActionDenial::Forbidden {
                reason: "configuration_review_required".into(),
            });
        }
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}
fn definitions() -> (Vec<ObjectTypeDef>, Vec<ActionRoot>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let compiled = Command::new("bun")
        .current_dir(&root)
        .args([
            "tools/norn-ts/src/cli.ts",
            "compile",
            "tools/norn-ts/fixtures/parameters",
            "--org",
            "org_demo",
            "--env",
            "dev",
        ])
        .output()
        .expect("typed compiler");
    assert!(
        compiled.status.success(),
        "{} {}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_slice(&compiled.stdout).expect("canonical artifacts");
    let mut types = Vec::new();
    let mut actions = Vec::new();
    for artifact in artifacts {
        let payload = serde_json::to_value(artifact.payload).expect("typed payload");
        if let Some(object) = payload.get("objectType") {
            types.push(serde_json::from_value(object.clone()).expect("object type"));
        }
        if payload.get("actionId").is_some() {
            actions.push(serde_json::from_value(payload).expect("action"));
        }
    }
    // The host's receipt storage schema is infrastructure, expressed in typed Rust.
    let receipt = ObjectTypeDef {
        name: ACTION_CALL_TYPE.into(),
        description: None,
        status: Default::default(),
        extends: None,
        implements: vec![],
        links: vec![],
        owned_collections: vec![],
        identity: vec![],
        invariants: vec![],
        lifecycles: vec![],
        markings: vec![],
        accounting_leg: None,
        properties: [
            "actionId",
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "parameterReads",
            "actor",
            "turn",
            "transition",
            "from",
            "to",
            "rowOperations",
        ]
        .into_iter()
        .map(|name| Property {
            id: PropertyId(name.into()),
            description: None,
            property_type: PropertyType::JsonExt { schema: None },
            constraints: norn_ir::PropertyConstraints {
                nullable: true,
                ..Default::default()
            },
            default: None,
            markings: vec![],
            computed: None,
        })
        .collect(),
    };
    types.push(receipt);
    (types, actions)
}
fn values(text: &str) -> JsonObject {
    RuntimeValue::from_json_text(text)
        .expect("exact input")
        .as_object()
        .expect("record")
        .clone()
}
fn call<'a>(action: &'a ActionRoot, key: &str, input: JsonObject, actor: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: &action.action_id,
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: actor.into(),
        input,
        idempotency_key: key.into(),
        object_type: None,
        turn: DecisionTurn {
            id: key.into(),
            now: LogicalTime::from_millis(1),
            world: World::Main,
            consumes: None,
        },
    }
}
fn rate(id: &str, value: &str, from: &str, to: &str) -> JsonObject {
    values(&format!(
        r#"{{"id":"{id}","settingKey":"tax.rate","value":{value},"effectiveFrom":"{from}","effectiveTo":"{to}","reviewState":"approved","orgId":"org_demo","environmentId":"dev","tenantId":null,"workspaceId":null,"businessUnitId":null}}"#
    ))
}
fn invoice(id: &str, rate: &str, at: &str) -> JsonObject {
    values(&format!(
        r#"{{"id":"{id}","registry":"tax","currency":"USD","net":100,"rate":{rate},"effectiveAt":"{at}"}}"#
    ))
}
fn recorded(cell: &Cell, key: &str) -> ActionCallRecord {
    ActionCallRecord::from_values(
        cell.load(&FactKey::new(
            ACTION_CALL_TYPE,
            format!("demo.tax.post/{key}"),
        ))
        .expect("receipt read")
        .values
        .as_ref()
        .expect("receipt"),
    )
    .expect("exact receipt decode")
}
#[test]
fn param_tax_governed_rates_commit_reopen_and_preserve_both_receipts() {
    let (types, actions) = definitions();
    let catalog = TypeCatalog::new(
        types
            .iter()
            .filter(|ty| ty.name.as_str() != ACTION_CALL_TYPE),
    );
    let action = |id: &str| {
        actions
            .iter()
            .find(|action| action.action_id == id)
            .expect("authored action")
    };
    let folder = tempfile::tempdir().expect("database folder");
    let path = folder.path().join("tax.sqlite");
    let open = || {
        Cell::open(
            &path,
            common::model_of(&types),
            StoreOptions::default(),
            "org_demo/dev",
            |command| command.payload.clone(),
        )
        .expect("native cell")
    };
    let mut cell = open();
    let june = rate(
        "june",
        "0.10",
        "2026-01-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    let july = rate(
        "july",
        "0.12",
        "2026-07-01T00:00:00Z",
        "2099-01-01T00:00:00Z",
    );
    let denied = commit_action(
        &mut cell,
        &catalog,
        &call(
            action("demo.tax.set"),
            "denied",
            june.clone(),
            "invoice:clerk",
        ),
        &Policy,
    )
    .expect_err("governed configuration");
    assert_eq!(denied.code(), "action.permission_denied");
    for (id, input) in [("june", june.clone()), ("july", july.clone())] {
        commit_action(
            &mut cell,
            &catalog,
            &call(action("demo.tax.set"), id, input, "config:reviewer"),
            &Policy,
        )
        .expect("ordinary setting action");
    }
    commit_action(
        &mut cell,
        &catalog,
        &call(
            action("demo.tax.configure"),
            "configure",
            values(r#"{"id":"tax","june":"june","july":"july"}"#),
            "config:reviewer",
        ),
        &Policy,
    )
    .expect("governed link membership");
    let mut originals = Vec::new();
    for (id, rate, at, tax, gross, setting) in [
        (
            "june-invoice",
            "0.10",
            "2026-06-30T23:59:59Z",
            10,
            110,
            "june",
        ),
        (
            "july-invoice",
            "0.12",
            "2026-07-01T00:00:00Z",
            12,
            112,
            "july",
        ),
    ] {
        let invocation = call(
            action("demo.tax.post"),
            id,
            invoice(id, rate, at),
            "invoice:clerk",
        );
        let ActionOutcome::Committed { record, .. } =
            commit_action(&mut cell, &catalog, &invocation, &Policy)
                .expect("real evaluator and commit")
        else {
            panic!("new invoice")
        };
        let effective = record.effective_input.as_ref().expect("derived input");
        assert_eq!(effective.get("net"), Some(&RuntimeValue::from(100)));
        assert_eq!(effective.get("tax"), Some(&RuntimeValue::from(tax)));
        assert_eq!(effective.get("gross"), Some(&RuntimeValue::from(gross)));
        assert_eq!(effective.get("currency"), Some(&RuntimeValue::from("USD")));
        assert_eq!(record.parameter_reads.len(), 1);
        assert_eq!(
            record.parameter_reads[0].key,
            FactKey::new("demo.NumericSetting", setting)
        );
        assert_eq!(
            record.parameter_reads[0].expected,
            cell.load(&record.parameter_reads[0].key)
                .expect("selected version")
                .version
        );
        assert_eq!(recorded(&cell, id), record);
        originals.push((id, invocation.input, record));
    }
    for (id, mut input) in [("edit-june", june), ("edit-july", july)] {
        input.insert(
            "value".into(),
            RuntimeValue::from_json_text("0.20").expect("exact"),
        );
        commit_action(
            &mut cell,
            &catalog,
            &call(action("demo.tax.set"), id, input, "config:reviewer"),
            &Policy,
        )
        .expect("later governed edit");
    }
    drop(cell);
    let mut cell = open();
    for (id, input, original) in originals {
        assert_eq!(recorded(&cell, id), original, "durable original receipt");
        let ActionOutcome::Replayed { record } = commit_action(
            &mut cell,
            &catalog,
            &call(action("demo.tax.post"), id, input, "invoice:clerk"),
            &Policy,
        )
        .expect("replay after reopen") else {
            panic!("original replay")
        };
        assert_eq!(record, original);
        let posted = cell
            .load(&original.object)
            .expect("posted invoice")
            .values
            .expect("posted values");
        for field in ["net", "tax", "gross"] {
            assert_eq!(
                posted.get(field),
                original
                    .effective_input
                    .as_ref()
                    .expect("derived")
                    .get(field)
            );
        }
    }
    let wrong_rate = call(
        action("demo.tax.post"),
        "wrong",
        invoice("wrong", "0.10", "2026-07-01T00:00:00Z"),
        "invoice:clerk",
    );
    assert_eq!(
        commit_action(&mut cell, &catalog, &wrong_rate, &Policy)
            .expect_err("cannot override selected rate")
            .code(),
        "action.precondition_failed"
    );
    assert!(
        !cell
            .load(&FactKey::new("demo.TaxInvoice", "wrong"))
            .expect("no partial invoice")
            .exists()
    );
}
