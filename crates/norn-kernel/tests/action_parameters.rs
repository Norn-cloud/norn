#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test fixture and assertion helpers"
)]

//! End-to-end `SliceKey::Parameter` execution tests in `norn-kernel`:
//! - D07 / PARAM-EXACT-1 / PARAM-BOUND-1 resolution (half-open effective windows,
//!   scope precedence fallthrough, supersession, explicit default, missing/gap
//!   refusal, equal-precedence & shadowed overlap refusal, wrong scope, malformed
//!   setting fact, read authorization concealment).
//! - COMPLETE-TAX receipt pin retention across later setting revisions and exact
//!   idempotent replay vs conflicting `effective_at` replay.
//! - Concurrent candidate edit (winning AND non-winning) and link-membership
//!   phantom mutations refused as stale precondition reads.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_checker::parameter::ParameterError;
use norn_ir::{ActionRoot, Decimal, ObjectTypeDef, RuleRoot, RuntimeValue};
use norn_kernel::cell_host::{BoundedFact, JsonObject};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionCallRecord,
    ActionDenial, ActionOutcome, ActionParameterPin, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost, Command, CommitError,
    CommitReceipt, DecisionBinding, DecisionTurn, DueTimer, FactEvent, FactKey, FactMutation,
    FactState, FactWrite, LinkKey, LinkMutation, LinkReadTarget, LinkState, LinkWrite, LogicalTime,
    Observed, Performed, SynchronousRule, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

#[derive(Clone, Copy)]
struct SettingFixture<'a> {
    id: &'a str,
    setting_key: &'a str,
    value: &'a str,
    effective_from: &'a str,
    effective_to: Option<&'a str>,
    review_state: &'a str,
    org_id: Option<&'a str>,
    environment_id: Option<&'a str>,
}

fn new_cell() -> Cell {
    Cell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(types())
}

fn object(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("exact fixture object")
}

fn decimal(text: &str) -> RuntimeValue {
    RuntimeValue::Number(Decimal::parse(text).expect("exact decimal"))
}

fn types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([
        {
            "name": "billing.NumericSetting",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "setting_key", "type": {"kind": "text"}},
                {"id": "value", "type": {"kind": "decimal", "scale": 4}},
                {"id": "effective_from", "type": {"kind": "timestamp", "timezone": "utc"}},
                {"id": "effective_to", "type": {"kind": "timestamp", "timezone": "utc"}, "constraints": {"nullable": true}},
                {"id": "review_state", "type": {"kind": "text"}},
                {"id": "org_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                {"id": "tenant_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                {"id": "workspace_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                {"id": "environment_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                {"id": "business_unit_id", "type": {"kind": "text"}, "constraints": {"nullable": true}}
            ],
            "identity": [{"name": "primary", "properties": ["id"]}]
        },
        {
            "name": "billing.SettingRegistry",
            "properties": [{"id": "id", "type": {"kind": "text"}}],
            "identity": [{"name": "primary", "properties": ["id"]}],
            "links": [
                {"id": "settings", "target": "billing.NumericSetting", "cardinality": {"to": "many", "from": "many"}}
            ]
        },
        {
            "name": "billing.Invoice",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [{"value": "draft"}, {"value": "posted"}]
                    }
                },
                {"id": "net", "type": {"kind": "decimal", "scale": 2}},
                {"id": "tax", "type": {"kind": "decimal", "scale": 2}},
                {"id": "gross", "type": {"kind": "decimal", "scale": 2}}
            ],
            "identity": [{"name": "primary", "properties": ["id"]}],
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["posted"],
                "transitions": [{"id": "post", "from": ["draft"], "to": "posted"}]
            }]
        }
    ]))
    .expect("typed entities")
}

fn setting_values(fixture: SettingFixture<'_>) -> JsonObject {
    let opt = |value: Option<&str>| match value {
        Some(text) => RuntimeValue::String(text.to_owned()),
        None => RuntimeValue::Null,
    };
    JsonObject::from([
        ("id".to_owned(), RuntimeValue::String(fixture.id.to_owned())),
        (
            "setting_key".to_owned(),
            RuntimeValue::String(fixture.setting_key.to_owned()),
        ),
        ("value".to_owned(), decimal(fixture.value)),
        (
            "effective_from".to_owned(),
            RuntimeValue::String(fixture.effective_from.to_owned()),
        ),
        ("effective_to".to_owned(), opt(fixture.effective_to)),
        (
            "review_state".to_owned(),
            RuntimeValue::String(fixture.review_state.to_owned()),
        ),
        ("org_id".to_owned(), opt(fixture.org_id)),
        ("tenant_id".to_owned(), RuntimeValue::Null),
        ("workspace_id".to_owned(), RuntimeValue::Null),
        ("environment_id".to_owned(), opt(fixture.environment_id)),
        ("business_unit_id".to_owned(), RuntimeValue::Null),
    ])
}

fn post_tax_action(default_from: Option<&str>, precedence: &serde_json::Value) -> ActionRoot {
    let mut slice = vec![
        json!({"name": "registry", "objectType": "billing.SettingRegistry", "key": {"property": {"from": "input", "property": "registry_id"}}}),
        json!({"name": "settings", "objectType": "billing.NumericSetting", "key": {"link": {"from": "registry", "link": "settings"}}}),
        json!({"name": "invoice", "objectType": "billing.Invoice", "key": {"property": {"from": "input", "property": "id"}}}),
    ];
    if default_from.is_some() {
        slice.push(json!({
            "name": "fallback",
            "objectType": "billing.NumericSetting",
            "key": {"property": {"from": "input", "property": "default_setting_id"}},
            "optional": true
        }));
    }
    let mut parameter = json!({
        "from": "settings",
        "settingKey": "billing.vat_rate",
        "effectiveAt": "effective_at",
        "precedence": precedence
    });
    if let Some(default_name) = default_from {
        parameter["defaultFrom"] = json!(default_name);
    }
    slice.push(json!({"name": "tax", "objectType": "billing.NumericSetting", "key": {"parameter": parameter}}));

    let mut properties = json!({
        "id": {"type": "string"},
        "registry_id": {"type": "string"},
        "effective_at": {"type": "string"}
    });
    if default_from.is_some() {
        properties["default_setting_id"] = json!({"type": ["string", "null"]});
    }

    serde_json::from_value(json!({
        "actionId": "billing.invoice.post",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": ["id", "registry_id", "effective_at"]
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "slice": slice,
        "preconditions": ["slice.tax.value >= 0"],
        "synchronousRule": {"artifactId": "billing.invoice.compute_tax.rule", "version": "1.0.0"},
        "writes": [{"entityKind": "billing.Invoice", "change": "modified"}],
        "effectProgram": {
            "kind": "fire_transition",
            "objectKind": "billing.Invoice",
            "objectIdField": "id",
            "lifecycle": "status",
            "transition": "post",
            "entityArtifactId": "billing.Invoice.entity",
            "entityArtifactVersion": "1.0.0"
        }
    }))
    .expect("post tax action")
}

fn setting_object_schema() -> serde_json::Value {
    let nullable_str = json!({"type": ["string", "null"]});
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": {"type": "string"},
            "setting_key": {"type": "string"},
            "value": {"type": "number"},
            "effective_from": {"type": "string"},
            "effective_to": nullable_str,
            "review_state": {"type": "string"},
            "org_id": nullable_str,
            "tenant_id": nullable_str,
            "workspace_id": nullable_str,
            "environment_id": nullable_str,
            "business_unit_id": nullable_str
        },
        "required": [
            "id", "setting_key", "value", "effective_from", "effective_to",
            "review_state", "org_id", "tenant_id", "workspace_id",
            "environment_id", "business_unit_id"
        ]
    })
}

fn tax_rule_input_schema() -> serde_json::Value {
    let setting = setting_object_schema();
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "event": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "actor": {"type": "string"},
                    "input": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string"},
                            "registry_id": {"type": "string"},
                            "effective_at": {"type": "string"},
                            "default_setting_id": {"type": ["string", "null"]}
                        },
                        "required": ["id", "registry_id", "effective_at"]
                    },
                    "operationId": {"type": "string"}
                },
                "required": ["input", "actor", "operationId"]
            },
            "slice": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "registry": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"id": {"type": "string"}},
                        "required": ["id"]
                    },
                    "settings": {"type": "array", "items": setting},
                    "invoice": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string"},
                            "status": {"type": "string"},
                            "net": {"type": "number"},
                            "tax": {"type": "number"},
                            "gross": {"type": "number"}
                        },
                        "required": ["id", "status", "net", "tax", "gross"]
                    },
                    "fallback": setting,
                    "tax": setting
                },
                "required": ["registry", "settings", "invoice", "tax"]
            }
        },
        "required": ["slice", "event"]
    })
}

fn tax_rule() -> SynchronousRule {
    let root: RuleRoot = serde_json::from_value(json!({
        "ruleVersion": "2.0",
        "ruleId": "billing.invoice.compute_tax",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": tax_rule_input_schema(),
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "commands": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}},
                "facts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "op": {"type": "string", "enum": ["set"]},
                            "objectType": {"type": "string", "enum": ["billing.Invoice"]},
                            "key": {"type": "string"},
                            "property": {"type": "string", "enum": ["tax", "gross"]},
                            "value": {"type": "number"}
                        },
                        "required": ["op", "objectType", "key", "property", "value"]
                    }
                },
                "timers": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}}
            },
            "required": ["facts", "commands", "timers"]
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "type": "inputNode", "name": "input"},
                {
                    "id": "decision",
                    "type": "decisionTableNode",
                    "name": "decision",
                    "content": {
                        "hitPolicy": "first",
                        "inputs": [{"id": "condition", "name": "condition", "type": "expression"}],
                        "outputs": [
                            {"field": "facts", "id": "out_facts", "name": "facts"},
                            {"field": "commands", "id": "out_commands", "name": "commands"},
                            {"field": "timers", "id": "out_timers", "name": "timers"}
                        ],
                        "rules": [{
                            "_id": "row_0",
                            "condition": "",
                            "out_facts": "[{op: 'set', objectType: 'billing.Invoice', key: slice.invoice.id, property: 'tax', value: slice.invoice.net * slice.tax.value}, {op: 'set', objectType: 'billing.Invoice', key: slice.invoice.id, property: 'gross', value: slice.invoice.net + (slice.invoice.net * slice.tax.value)}]",
                            "out_commands": "[]",
                            "out_timers": "[]"
                        }]
                    }
                },
                {"id": "out", "type": "outputNode", "name": "output"}
            ],
            "edges": [
                {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
            ]
        }
    }))
    .expect("tax rule root");
    SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: "billing.invoice.compute_tax.rule".to_owned(),
            version: "1.0.0".to_owned(),
        },
        &root,
        &BTreeMap::new(),
        DecisionBinding {
            profile_id: "norn-zen/6".to_owned(),
            environment_hash: "test-env".to_owned(),
        },
    )
    .expect("admitted tax rule")
}

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

struct DenySettingKey(String);
impl ActionAuthorizer for DenySettingKey {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.target.key == self.0 {
            Err(ActionDenial::Invisible)
        } else {
            Ok(())
        }
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

struct DenySettingType;
impl ActionAuthorizer for DenySettingType {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if request.object_type.as_str() == "billing.NumericSetting" {
            Err(ActionDenial::Invisible)
        } else {
            Ok(())
        }
    }
}

fn put(store: &mut Cell, key: FactKey, values: JsonObject) {
    let expected = store.load(&key).expect("load").version;
    store
        .commit(CellCommit {
            writes: vec![FactWrite {
                key,
                expected,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        })
        .expect("seed fact");
}

fn link(store: &mut Cell, source: FactKey, link_name: &str, target: FactKey) {
    let key = LinkKey::new(source, link_name, target);
    let expected = store.load_link(&key).expect("load link").version;
    store
        .commit(CellCommit {
            links: vec![LinkWrite {
                key,
                expected,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect("seed link");
}

fn invocation_for<'a>(
    action: &'a ActionRoot,
    invoice_id: &str,
    effective_at: &str,
    idempotency_key: &str,
    default_setting_id: Option<&str>,
) -> ActionCall<'a> {
    let mut input = JsonObject::from([
        ("id".to_owned(), RuntimeValue::String(invoice_id.to_owned())),
        (
            "registry_id".to_owned(),
            RuntimeValue::String("reg-main".to_owned()),
        ),
        (
            "effective_at".to_owned(),
            RuntimeValue::String(effective_at.to_owned()),
        ),
    ]);
    if let Some(default_id) = default_setting_id {
        input.insert(
            "default_setting_id".to_owned(),
            RuntimeValue::String(default_id.to_owned()),
        );
    }
    ActionCall {
        action,
        action_artifact_id: "billing.invoice.post.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "billing:clerk".to_owned(),
        approved_intent: None,
        input,
        idempotency_key: idempotency_key.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{idempotency_key}"),
            now: LogicalTime::from_millis(10_000),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

fn seed_invoice(store: &mut Cell, id: &str, net: &str) {
    put(
        store,
        FactKey::new("billing.Invoice", id),
        JsonObject::from([
            ("id".to_owned(), RuntimeValue::String(id.to_owned())),
            (
                "status".to_owned(),
                RuntimeValue::String("draft".to_owned()),
            ),
            ("net".to_owned(), decimal(net)),
            ("tax".to_owned(), decimal("0")),
            ("gross".to_owned(), decimal("0")),
        ]),
    );
}

fn seed_june_july_settings(store: &mut Cell) -> (FactKey, FactKey) {
    let registry = FactKey::new("billing.SettingRegistry", "reg-main");
    put(store, registry.clone(), object(json!({"id": "reg-main"})));
    let june_key = FactKey::new("billing.NumericSetting", "vat-june");
    let july_key = FactKey::new("billing.NumericSetting", "vat-july");
    put(
        store,
        june_key.clone(),
        setting_values(SettingFixture {
            id: "vat-june",
            setting_key: "billing.vat_rate",
            value: "0.10",
            effective_from: "2026-06-01T00:00:00Z",
            effective_to: Some("2026-07-01T00:00:00Z"),
            review_state: "approved",
            org_id: Some("org_demo"),
            environment_id: Some("dev"),
        }),
    );
    put(
        store,
        july_key.clone(),
        setting_values(SettingFixture {
            id: "vat-july",
            setting_key: "billing.vat_rate",
            value: "0.12",
            effective_from: "2026-07-01T00:00:00Z",
            effective_to: None,
            review_state: "active",
            org_id: Some("org_demo"),
            environment_id: Some("dev"),
        }),
    );
    link(store, registry.clone(), "settings", june_key.clone());
    link(store, registry, "settings", july_key.clone());
    (june_key, july_key)
}

fn load_invoice(store: &Cell, id: &str) -> JsonObject {
    store
        .load(&FactKey::new("billing.Invoice", id))
        .expect("load invoice")
        .values
        .expect("invoice exists")
}

fn load_call_record(store: &Cell, key: &str) -> ActionCallRecord {
    ActionCallRecord::from_values(
        store
            .load(&FactKey::new(ACTION_CALL_TYPE, key))
            .expect("load call record")
            .values
            .as_ref()
            .expect("call record exists"),
    )
    .expect("decode call record")
}

fn expect_committed(outcome: ActionOutcome) -> ActionCallRecord {
    let ActionOutcome::Committed { record, .. } = outcome else {
        panic!("expected committed action outcome: {outcome:?}");
    };
    record
}

fn revise_june_july_settings(store: &mut Cell, june_key: FactKey, july_key: FactKey) {
    put(
        store,
        june_key,
        setting_values(SettingFixture {
            id: "vat-june",
            setting_key: "billing.vat_rate",
            value: "0.15",
            effective_from: "2026-06-01T00:00:00Z",
            effective_to: Some("2026-07-01T00:00:00Z"),
            review_state: "superseded",
            org_id: Some("org_demo"),
            environment_id: Some("dev"),
        }),
    );
    put(
        store,
        july_key,
        setting_values(SettingFixture {
            id: "vat-july",
            setting_key: "billing.vat_rate",
            value: "0.20",
            effective_from: "2026-07-01T00:00:00Z",
            effective_to: None,
            review_state: "active",
            org_id: Some("org_demo"),
            environment_id: Some("dev"),
        }),
    );
}

#[test]
fn parameter_slice_complete_tax_resolves_windows_pins_receipts_and_retains_pins_after_revision() {
    let types = types();
    let rule = tax_rule();
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let action = post_tax_action(
        None,
        &json!([{"orgId": "org_demo", "environmentId": "dev"}, {"orgId": "org_demo"}, {}]),
    );
    let mut store = new_cell();
    let (june_key, july_key) = seed_june_july_settings(&mut store);
    seed_invoice(&mut store, "inv-june", "100.00");
    seed_invoice(&mut store, "inv-july", "100.00");

    // June 30 boundary: selects 10% (tax = 10, gross = 110).
    let june_req = invocation_for(
        &action,
        "inv-june",
        "2026-06-30T23:59:59.999999999Z",
        "post-inv-june",
        None,
    );
    let june_record =
        expect_committed(commit_action(&mut store, &catalog, &june_req, &Allow).expect("June"));
    let inv_june = load_invoice(&store, "inv-june");
    assert_eq!(
        inv_june.get("status"),
        Some(&RuntimeValue::String("posted".to_owned()))
    );
    assert_eq!(inv_june.get("tax"), Some(&decimal("10")));
    assert_eq!(inv_june.get("gross"), Some(&decimal("110")));
    assert_eq!(june_record.parameter_pins.len(), 1);
    let june_pin = &june_record.parameter_pins[0];
    assert_eq!(june_pin.entry, "tax");
    assert_eq!(june_pin.setting_key, "billing.vat_rate");
    assert_eq!(june_pin.effective_at, "2026-06-30T23:59:59.999999999Z");
    assert_eq!(june_pin.setting, june_key);
    assert_eq!(june_pin.version, 1);
    assert!(june_pin.digest.starts_with("sha256:"));

    // July 1 boundary: selects 12% (tax = 12, gross = 112).
    let july_req = invocation_for(
        &action,
        "inv-july",
        "2026-07-01T00:00:00Z",
        "post-inv-july",
        None,
    );
    let july_record =
        expect_committed(commit_action(&mut store, &catalog, &july_req, &Allow).expect("July"));
    let inv_july = load_invoice(&store, "inv-july");
    assert_eq!(inv_july.get("tax"), Some(&decimal("12")));
    assert_eq!(inv_july.get("gross"), Some(&decimal("112")));
    let july_pin = &july_record.parameter_pins[0];
    assert_eq!(july_pin.setting, july_key);
    assert_ne!(june_pin.digest, july_pin.digest);

    // COMPLETE-TAX: revise both settings to new versions; stored receipts keep original pins.
    revise_june_july_settings(&mut store, june_key, july_key);
    assert_eq!(
        load_call_record(&store, "billing.invoice.post/post-inv-june").parameter_pins,
        vec![june_pin.clone()]
    );
    assert_eq!(
        load_call_record(&store, "billing.invoice.post/post-inv-july").parameter_pins,
        vec![july_pin.clone()]
    );

    // Exact idempotent replay returns original pins; changing effective_at refuses IdempotencyConflict.
    let replayed = commit_action(&mut store, &catalog, &june_req, &Allow).expect("replay");
    assert_eq!(
        replayed,
        ActionOutcome::Replayed {
            record: june_record
        }
    );
    let conflicting = invocation_for(
        &action,
        "inv-june",
        "2026-07-01T00:00:00Z",
        "post-inv-june",
        None,
    );
    assert!(matches!(
        commit_action(&mut store, &catalog, &conflicting, &Allow),
        Err(ActionRefusal::IdempotencyConflict { .. })
    ));
}

fn seed_precedence_settings(store: &mut Cell) -> (FactKey, FactKey) {
    let registry = FactKey::new("billing.SettingRegistry", "reg-main");
    put(store, registry.clone(), object(json!({"id": "reg-main"})));
    let superseded_env = FactKey::new("billing.NumericSetting", "vat-env-old");
    let org_fallback = FactKey::new("billing.NumericSetting", "vat-org");
    let explicit_default = FactKey::new("billing.NumericSetting", "vat-default");
    put(
        store,
        superseded_env.clone(),
        setting_values(SettingFixture {
            id: "vat-env-old",
            setting_key: "billing.vat_rate",
            value: "0.25",
            effective_from: "2026-06-01T00:00:00Z",
            effective_to: None,
            review_state: "superseded",
            org_id: Some("org_demo"),
            environment_id: Some("dev"),
        }),
    );
    put(
        store,
        org_fallback.clone(),
        setting_values(SettingFixture {
            id: "vat-org",
            setting_key: "billing.vat_rate",
            value: "0.08",
            effective_from: "2026-06-01T00:00:00Z",
            effective_to: Some("2026-07-01T00:00:00Z"),
            review_state: "approved",
            org_id: Some("org_demo"),
            environment_id: None,
        }),
    );
    put(
        store,
        explicit_default.clone(),
        setting_values(SettingFixture {
            id: "vat-default",
            setting_key: "billing.vat_rate",
            value: "0.05",
            effective_from: "2026-01-01T00:00:00Z",
            effective_to: None,
            review_state: "approved",
            org_id: None,
            environment_id: None,
        }),
    );
    link(store, registry.clone(), "settings", superseded_env);
    link(store, registry, "settings", org_fallback.clone());
    (org_fallback, explicit_default)
}

#[test]
fn parameter_slice_scope_precedence_fallthrough_supersession_and_default() {
    let types = types();
    let rule = tax_rule();
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let precedence =
        json!([{"orgId": "org_demo", "environmentId": "dev"}, {"orgId": "org_demo"}, {}]);
    let action_with_default = post_tax_action(Some("fallback"), &precedence);
    let action_without_default = post_tax_action(None, &precedence);
    let mut store = new_cell();
    let (org_fallback, explicit_default) = seed_precedence_settings(&mut store);

    // Higher precedence env setting is superseded -> falls through to org setting (8%).
    seed_invoice(&mut store, "inv-org", "100.00");
    let req_org = invocation_for(
        &action_with_default,
        "inv-org",
        "2026-06-15T00:00:00Z",
        "post-org",
        Some("vat-default"),
    );
    let record =
        expect_committed(commit_action(&mut store, &catalog, &req_org, &Allow).expect("org"));
    assert_eq!(record.parameter_pins[0].setting, org_fallback);
    assert_eq!(
        load_invoice(&store, "inv-org").get("tax"),
        Some(&decimal("8"))
    );

    // Gap on July 15 without default -> refuses core.composition.parameter_missing with zero writes.
    seed_invoice(&mut store, "inv-gap", "100.00");
    let req_gap = invocation_for(
        &action_without_default,
        "inv-gap",
        "2026-07-15T00:00:00Z",
        "post-gap-no-default",
        None,
    );
    let err = commit_action(&mut store, &catalog, &req_gap, &Allow).expect_err("missing");
    assert_eq!(err.code(), "core.composition.parameter_missing");
    assert!(matches!(
        err,
        ActionRefusal::ParameterRefused {
            error: ParameterError::Missing,
            ..
        }
    ));
    assert_eq!(
        load_invoice(&store, "inv-gap").get("status"),
        Some(&RuntimeValue::String("draft".to_owned()))
    );

    // Same gap WITH explicit defaultFrom -> uses explicit default (5%) and pins it.
    let req_default = invocation_for(
        &action_with_default,
        "inv-gap",
        "2026-07-15T00:00:00Z",
        "post-gap-default",
        Some("vat-default"),
    );
    let record = expect_committed(
        commit_action(&mut store, &catalog, &req_default, &Allow).expect("default"),
    );
    assert_eq!(record.parameter_pins[0].setting, explicit_default);
    let inv = load_invoice(&store, "inv-gap");
    assert_eq!(inv.get("tax"), Some(&decimal("5")));
    assert_eq!(inv.get("gross"), Some(&decimal("105")));
}

fn seed_ambiguous_settings(store: &mut Cell) {
    let registry = FactKey::new("billing.SettingRegistry", "reg-main");
    put(store, registry.clone(), object(json!({"id": "reg-main"})));
    for (id, value, env) in [
        ("vat-env", "0.10", Some("dev")),
        ("vat-org-a", "0.08", None),
        ("vat-org-b", "0.09", None),
    ] {
        let key = FactKey::new("billing.NumericSetting", id);
        put(
            store,
            key.clone(),
            setting_values(SettingFixture {
                id,
                setting_key: "billing.vat_rate",
                value,
                effective_from: "2026-06-01T00:00:00Z",
                effective_to: None,
                review_state: "approved",
                org_id: Some("org_demo"),
                environment_id: env,
            }),
        );
        link(store, registry.clone(), "settings", key);
    }
}

#[test]
fn parameter_slice_refuses_overlap_wrong_scope_malformed_and_unauthorized_candidates() {
    let types = types();
    let rule = tax_rule();
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let action = post_tax_action(
        None,
        &json!([{"orgId": "org_demo", "environmentId": "dev"}, {"orgId": "org_demo"}]),
    );
    let mut store = new_cell();
    seed_ambiguous_settings(&mut store);
    seed_invoice(&mut store, "inv-1", "100.00");

    // Shadowed equal-precedence overlap refuses Ambiguous even when higher precedence exists.
    let request = invocation_for(&action, "inv-1", "2026-06-15T00:00:00Z", "post-ambig", None);
    let err = commit_action(&mut store, &catalog, &request, &Allow).expect_err("ambiguous");
    assert_eq!(err.code(), "core.composition.parameter_ambiguous");
    assert!(matches!(
        err,
        ActionRefusal::ParameterRefused {
            error: ParameterError::Ambiguous,
            ..
        }
    ));

    // Read denial on any candidate fails closed as TargetNotFound before parameter resolution.
    let denied = commit_action(
        &mut store,
        &catalog,
        &request,
        &DenySettingKey("vat-org-b".to_owned()),
    )
    .expect_err("unauthorized candidate");
    assert!(matches!(denied, ActionRefusal::TargetNotFound));

    // Wrong scope in precedence (conflicts with caller's environmentId="dev") refuses Invalid.
    let wrong_scope_action = post_tax_action(
        None,
        &json!([{"orgId": "org_demo", "environmentId": "prod"}]),
    );
    let wrong_scope_req = invocation_for(
        &wrong_scope_action,
        "inv-1",
        "2026-06-15T00:00:00Z",
        "post-wrong-scope",
        None,
    );
    let wrong_scope_err =
        commit_action(&mut store, &catalog, &wrong_scope_req, &Allow).expect_err("wrong scope");
    assert_eq!(wrong_scope_err.code(), "setting.validation.invalid_payload");

    // Malformed candidate setting (blank scope dimension) refuses Invalid.
    let mut bad_values = setting_values(SettingFixture {
        id: "vat-org-b",
        setting_key: "billing.vat_rate",
        value: "0.09",
        effective_from: "2026-06-01T00:00:00Z",
        effective_to: None,
        review_state: "approved",
        org_id: Some("org_demo"),
        environment_id: None,
    });
    bad_values.insert(
        "business_unit_id".to_owned(),
        RuntimeValue::String("   ".to_owned()),
    );
    put(
        &mut store,
        FactKey::new("billing.NumericSetting", "vat-org-b"),
        bad_values,
    );
    let malformed_err =
        commit_action(&mut store, &catalog, &request, &Allow).expect_err("malformed candidate");
    assert_eq!(malformed_err.code(), "setting.validation.invalid_payload");
}

type RaceHook = Box<dyn FnOnce(&mut Cell)>;

struct RacingHost {
    inner: RefCell<Cell>,
    on_commit: RefCell<Option<RaceHook>>,
}

impl RacingHost {
    fn new(store: Cell, on_commit: impl FnOnce(&mut Cell) + 'static) -> Self {
        Self {
            inner: RefCell::new(store),
            on_commit: RefCell::new(Some(Box::new(on_commit))),
        }
    }
}

impl CellHost for RacingHost {
    type Error = Infallible;
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.inner.borrow().load(key)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        self.inner.borrow().load_bounded(key, max_bytes)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.inner.borrow().load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.inner
            .borrow()
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        if let Some(race) = self.on_commit.borrow_mut().take() {
            race(self.inner.get_mut());
        }
        self.inner.get_mut().commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        self.inner.get_mut().perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        self.inner.get_mut().observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        self.inner.get_mut().clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error> {
        self.inner.get_mut().publish(receipt)
    }
}

fn seeded_race_cell() -> Cell {
    let mut store = new_cell();
    let _ = seed_june_july_settings(&mut store);
    seed_invoice(&mut store, "inv-race", "100.00");
    store
}

#[test]
fn parameter_slice_concurrent_candidate_edit_and_link_mutation_refuse_stale_read() {
    let types = types();
    let rule = tax_rule();
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let action = post_tax_action(
        None,
        &json!([{"orgId": "org_demo", "environmentId": "dev"}]),
    );
    let request = invocation_for(
        &action,
        "inv-race",
        "2026-06-15T00:00:00Z",
        "post-race",
        None,
    );

    // 1. Concurrent edit of a NON-winning candidate setting still triggers conflict.
    let mut non_winner_race = RacingHost::new(seeded_race_cell(), |store| {
        put(
            store,
            FactKey::new("billing.NumericSetting", "vat-july"),
            setting_values(SettingFixture {
                id: "vat-july",
                setting_key: "billing.vat_rate",
                value: "0.14",
                effective_from: "2026-06-01T00:00:00Z",
                effective_to: None,
                review_state: "approved",
                org_id: Some("org_demo"),
                environment_id: Some("dev"),
            }),
        );
    });
    let err = commit_action(&mut non_winner_race, &catalog, &request, &Allow)
        .expect_err("concurrent candidate edit must refuse");
    assert!(matches!(
        err,
        ActionRefusal::StalePreconditionRead { ref key, link: None }
            if key == &FactKey::new("billing.NumericSetting", "vat-july")
    ));
    assert_eq!(
        load_invoice(&non_winner_race.inner.borrow(), "inv-race").get("status"),
        Some(&RuntimeValue::String("draft".to_owned()))
    );

    // 2. Concurrent link addition (phantom candidate setting) triggers LinkReadConflict.
    let mut phantom_link_race = RacingHost::new(seeded_race_cell(), |store| {
        let phantom = FactKey::new("billing.NumericSetting", "vat-phantom");
        put(
            store,
            phantom.clone(),
            setting_values(SettingFixture {
                id: "vat-phantom",
                setting_key: "billing.vat_rate",
                value: "0.11",
                effective_from: "2026-06-01T00:00:00Z",
                effective_to: None,
                review_state: "approved",
                org_id: Some("org_demo"),
                environment_id: Some("dev"),
            }),
        );
        link(
            store,
            FactKey::new("billing.SettingRegistry", "reg-main"),
            "settings",
            phantom,
        );
    });
    let err = commit_action(&mut phantom_link_race, &catalog, &request, &Allow)
        .expect_err("phantom candidate link must refuse");
    assert!(matches!(
        err,
        ActionRefusal::StalePreconditionRead {
            ref key,
            link: Some(ref link_name)
        } if key == &FactKey::new("billing.SettingRegistry", "reg-main") && link_name == "settings"
    ));
}

#[test]
fn action_call_record_parameter_pins_roundtrip_and_reject_corrupt_values() {
    let mut record = ActionCallRecord {
        action_id: "billing.invoice.post".to_owned(),
        action_artifact_id: Some("billing.invoice.post.action".to_owned()),
        action_version: Some("1.0.0".to_owned()),
        effect_kind: Some("fire_transition".to_owned()),
        rule_binding: None,
        object: FactKey::new("billing.Invoice", "inv-1"),
        input: JsonObject::new(),
        effective_input: None,
        derived_values: Vec::new(),
        precondition_outcomes: Vec::new(),
        parameter_pins: Vec::new(),
        actor: "billing:clerk".to_owned(),
        turn: "turn-1".to_owned(),
        transition: "post".to_owned(),
        from: "draft".to_owned(),
        to: "posted".to_owned(),
        row_operations: Vec::new(),
        trace: None,
    };
    let empty_values: JsonObject =
        serde_json::from_value(serde_json::to_value(&record).expect("serialize empty pins"))
            .expect("json object");
    assert!(!empty_values.contains_key("parameterPins"));

    record.parameter_pins = vec![ActionParameterPin {
        entry: "tax".to_owned(),
        setting_key: "billing.vat_rate".to_owned(),
        effective_at: "2026-06-30T23:59:59Z".to_owned(),
        setting: FactKey::new("billing.NumericSetting", "vat-june"),
        version: 1,
        digest: format!("sha256:{}", "a".repeat(64)),
    }];
    let json = serde_json::to_value(&record).expect("serialize with pins");
    let decoded: ActionCallRecord = serde_json::from_value(json).expect("deserialize");
    assert_eq!(decoded, record);

    // Corrupt pin (bad digest / zero version) must fail closed in from_values.
    let mut corrupt = object(serde_json::to_value(&record).expect("value"));
    corrupt.insert(
        "parameterPins".to_owned(),
        serde_json::from_value(json!([{
            "entry": "tax",
            "settingKey": "billing.vat_rate",
            "effectiveAt": "2026-06-30T23:59:59Z",
            "setting": {"objectType": "billing.NumericSetting", "key": "vat-june"},
            "version": 0,
            "digest": "not-a-sha256"
        }]))
        .expect("corrupt pins"),
    );
    assert!(ActionCallRecord::from_values(&corrupt).is_none());

    // Corrupt pin (invalid RFC 3339 effectiveAt) must fail closed in from_values.
    let mut bad_effective_at = object(serde_json::to_value(&record).expect("value"));
    bad_effective_at.insert(
        "parameterPins".to_owned(),
        serde_json::from_value(json!([{
            "entry": "tax",
            "settingKey": "billing.vat_rate",
            "effectiveAt": "not-a-timestamp",
            "setting": {"objectType": "billing.NumericSetting", "key": "vat-june"},
            "version": 1,
            "digest": format!("sha256:{}", "a".repeat(64))
        }]))
        .expect("bad effectiveAt pins"),
    );
    assert!(ActionCallRecord::from_values(&bad_effective_at).is_none());
}

#[test]
fn parameter_slice_enforces_type_read_authz_when_optional_parent_is_omitted() {
    let types = types();
    let rule = tax_rule();
    let catalog = TypeCatalog::new(&types).with_action_rules(std::slice::from_ref(&rule));
    let mut action_json = serde_json::to_value(post_tax_action(
        None,
        &json!([{"orgId": "org_demo", "environmentId": "dev"}]),
    ))
    .expect("action json");
    action_json["slice"][0]["optional"] = json!(true);
    action_json["inputSchema"]["properties"]["registry_id"] = json!({"type": ["string", "null"]});
    action_json["inputSchema"]["required"] = json!(["id", "effective_at"]);
    let action: ActionRoot = serde_json::from_value(action_json).expect("optional-parent action");

    let mut store = new_cell();
    seed_invoice(&mut store, "inv-omitted-parent", "100.00");

    let mut request = invocation_for(
        &action,
        "inv-omitted-parent",
        "2026-06-15T00:00:00Z",
        "post-omitted-parent-denied",
        None,
    );
    request.input.remove("registry_id");

    // Denied caller must receive TargetNotFound even though the optional registry parent is omitted.
    let denied = commit_action(&mut store, &catalog, &request, &DenySettingType)
        .expect_err("denied setting type must fail closed");
    assert!(matches!(denied, ActionRefusal::TargetNotFound));
}
