#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "PARAM-TAX integration fixtures"
)]
//! PARAM-TAX / PARAM-PIN-1: governed settings, exact evaluation and durable receipts.
use norn_cell::{SqliteCell, StoreOptions};
use norn_ir::{
    ActionRoot, ArtifactEnvelope, InterfaceDef, ObjectTypeDef, Property, PropertyId, PropertyType,
    RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionCallRecord,
    ActionDenial, ActionOutcome, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellHost,
    DecisionTurn, FactEvent, FactKey, LogicalTime, TypeCatalog, World, commit_action,
};
use std::{collections::BTreeMap, path::Path, process::Command};
const JUN30: &str = "2026-06-30T23:59:59Z";
const JUL01: &str = "2026-07-01T00:00:00Z";
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
type Definitions = (Vec<ObjectTypeDef>, Vec<ActionRoot>, Vec<InterfaceDef>);
#[allow(
    clippy::disallowed_methods,
    reason = "test invokes the typed fixture compiler"
)]
fn definitions() -> Definitions {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let compiled = Command::new("bun")
        .current_dir(&root)
        .args("tools/norn-ts/src/cli.ts compile tools/norn-ts/fixtures/parameters --org org_demo --env dev".split_whitespace())
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
    let diagnostics = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let mut types = Vec::<ObjectTypeDef>::new();
    let mut interfaces = Vec::new();
    let mut actions = Vec::new();
    for artifact in artifacts {
        let payload = serde_json::to_value(artifact.payload.to_json_value()).expect("bare payload");
        if let Some(object) = payload.get("objectType") {
            types.push(serde_json::from_value(object.clone()).expect("object type"));
        }
        if let Some(interface) = payload.get("interface") {
            interfaces.push(serde_json::from_value(interface.clone()).expect("interface"));
        }
        if payload.get("actionId").is_some() {
            actions.push(serde_json::from_value(payload).expect("action"));
        }
    }
    let mut receipt = types
        .iter()
        .find(|ty| ty.name.as_str() == "demo.SettingRegistry")
        .expect("registry template")
        .clone();
    receipt.name = norn_ir::ObjectTypeName(ACTION_CALL_TYPE.into());
    receipt.links.clear();
    receipt.identity.clear();
    receipt.properties = "actionId actionArtifactId actionVersion effectKind object input effectiveInput derivedValues preconditionOutcomes parameterReads actor turn transition from to rowOperations"
        .split_whitespace()
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
        .collect();
    types.push(receipt);
    (types, actions, interfaces)
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
        r#"{{"id":"{id}","setting_key":"tax.rate","value":{value},"effective_from":"{from}","effective_to":"{to}","review_state":"approved","org_id":"org_demo","environment_id":"dev","tenant_id":null,"workspace_id":null,"business_unit_id":null}}"#
    ))
}
fn invoice(id: &str, rate: &str, at: &str) -> JsonObject {
    values(&format!(
        r#"{{"id":"{id}","registry":"tax","currency":"USD","net":100,"rate":{rate},"effective_at":"{at}"}}"#
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
    let (types, actions, interfaces) = definitions();
    let catalog = TypeCatalog::new(
        types
            .iter()
            .filter(|ty| ty.name.as_str() != ACTION_CALL_TYPE),
    )
    .with_interfaces(&interfaces);
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
            norn_cell::Model::from_definitions_with_interfaces(&types, &[], &interfaces)
                .expect("projected model"),
            StoreOptions::default(),
            "org_demo/dev",
            |command| command.payload.clone(),
        )
        .expect("native cell")
    };
    let mut cell = open();
    let june = rate("june", "0.10", "2026-01-01T00:00:00Z", JUL01);
    let july = rate("july", "0.12", JUL01, "2099-01-01T00:00:00Z");
    let mut org_rate = rate(
        "org",
        "0.07",
        "2025-07-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
    );
    org_rate.remove("environment_id");
    let mut global_rate = rate(
        "global",
        "0.05",
        "2025-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
    );
    global_rate.remove("org_id");
    global_rate.remove("environment_id");
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
    for (id, input) in [
        ("june", june.clone()),
        ("july", july.clone()),
        ("org", org_rate.clone()),
        ("global", global_rate.clone()),
    ] {
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
            values(r#"{"id":"tax"}"#),
            "config:reviewer",
        ),
        &Policy,
    )
    .expect("registry action");
    let mut membership = norn_kernel::CellCommit::empty();
    for id in ["june", "july", "org", "global"] {
        membership.links.push(norn_kernel::LinkWrite {
            key: norn_kernel::LinkKey::new(
                FactKey::new("demo.SettingRegistry", "tax"),
                "settings",
                FactKey::new("demo.TaxRate", id),
            ),
            expected: norn_kernel::FactVersion(0),
            mutation: norn_kernel::LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    cell.commit(membership)
        .expect("installed setting membership");
    let mut originals = Vec::new();
    for (id, rate, at, tax, gross, setting) in [
        ("jun30", "0.10", JUN30, 10, 110, "june"),
        ("jul01", "0.12", JUL01, 12, 112, "july"),
        (
            "global-fallback",
            "0.05",
            "2025-06-30T00:00:00Z",
            5,
            105,
            "global",
        ),
        (
            "org-fallback",
            "0.07",
            "2025-07-01T00:00:00Z",
            7,
            107,
            "org",
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
        let pin = record
            .parameter_reads
            .first()
            .expect("selected setting pin");
        assert_eq!(pin.key, FactKey::new("demo.TaxRate", setting));
        assert_eq!(
            pin.expected,
            cell.load(&pin.key).expect("selected version").version
        );
        assert_eq!(recorded(&cell, id), record);
        originals.push((id, invocation.input, record));
    }
    for (id, mut input) in [
        ("edit-june", june),
        ("edit-july", july),
        ("edit-org", org_rate),
        ("edit-global", global_rate),
    ] {
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
        for pin in &original.parameter_reads {
            assert!(cell.load(&pin.key).expect("edited rate").version > pin.expected);
        }
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
        let derived = original.effective_input.as_ref().expect("derived input");
        for field in ["net", "tax", "gross"] {
            assert_eq!(posted.get(field), derived.get(field));
        }
    }
}
