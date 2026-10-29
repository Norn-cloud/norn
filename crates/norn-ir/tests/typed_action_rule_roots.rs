#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-IR-1c: typed Action and Rule roots.
//!
//! Every Action/Rule fixture decodes through the lenient envelope root *and*
//! through the strict typed root (`ActionRoot` / `RuleRoot`), and both
//! re-encode to the same canonical JSON. The two new envelope fixtures pin
//! their Norn1 canonical hashes.

use norn_canonical::HashProfile;
use norn_ir::jdm::{JdmHitPolicy, JdmNode, JdmNodeType};
use norn_ir::{
    ActionCommandOperation, ActionEffectProgram, ActionRoot, ActionUiEffect, ArtifactEnvelope,
    ArtifactKind, ArtifactPayload, DECIDE_INPUT_MEMBERS, DECIDE_OUTPUT_MEMBERS, RULE_IR_V1_VERSION,
    RULE_IR_V2_VERSION, RuleContractV2, RuleDecisionV1, RuleRoot, WriteChange,
};

const ACTION_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-reorder.action.json");
const RULE_V2_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-reorder-routing.rule.json");
const RULE_V1_CONFORMANCE: &str =
    include_str!("../../../verification/archive/rules-dmn-v1/fixtures/conformance.json");

fn canonical_hash(bytes: &[u8]) -> String {
    HashProfile::Norn1
        .hash_json(bytes)
        .expect("JSON should canonicalize")
}

fn payload_json(source: &str) -> serde_json::Value {
    let envelope: serde_json::Value = serde_json::from_str(source).expect("fixture should parse");
    envelope
        .get("payload")
        .cloned()
        .expect("fixture should contain a payload")
}

fn v1_conformance_payload() -> serde_json::Value {
    let conformance: serde_json::Value =
        serde_json::from_str(RULE_V1_CONFORMANCE).expect("conformance fixture should parse");
    conformance["artifact"]["payload"].clone()
}

/// Decode a payload into its typed root and assert the canonical JSON is
/// unchanged by the typed round-trip.
fn assert_typed_roundtrip<T>(name: &str, payload: &serde_json::Value) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let typed: T = serde_json::from_value(payload.clone())
        .unwrap_or_else(|error| panic!("{name} should decode into its typed root: {error}"));
    let reencoded = serde_json::to_vec(&typed).expect("typed root should encode");
    let source = serde_json::to_vec(payload).expect("payload should encode");
    assert_eq!(
        canonical_hash(&reencoded),
        canonical_hash(&source),
        "{name} canonical JSON changed after typed round-trip"
    );
    typed
}

#[test]
fn new_envelope_fixtures_roundtrip_with_pinned_hashes() {
    for (name, source, kind, pinned) in [
        (
            "fixtures/ir/retail-reorder.action.json",
            ACTION_FIXTURE,
            ArtifactKind::Action,
            "a379cf3f4c0276492c10e543c1f52192c0b8648464046e7a89a0a6698c7777cd",
        ),
        (
            "fixtures/ir/retail-reorder-routing.rule.json",
            RULE_V2_FIXTURE,
            ArtifactKind::Rule,
            "2daa37428482aaad6d84c3d0ada0cd35c055622e561a08aa6f9c23b9e82f0638",
        ),
    ] {
        let envelope: ArtifactEnvelope = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(envelope.kind(), kind, "{name} kind");
        assert!(
            envelope.payload.unknown_fields().is_empty(),
            "{name} has undeclared members: {:?}",
            envelope.payload.unknown_fields()
        );
        let reserialized = serde_json::to_vec(&envelope).expect("envelope should serialize");
        let source_hash = canonical_hash(source.as_bytes());
        assert_eq!(
            canonical_hash(&reserialized),
            source_hash,
            "{name} canonical JSON changed after envelope round-trip"
        );
        assert_eq!(source_hash, pinned, "{name} canonical hash drifted");
    }
}

#[test]
fn action_fixture_decodes_into_the_typed_root() {
    let action: ActionRoot =
        assert_typed_roundtrip("retail-reorder.action.json", &payload_json(ACTION_FIXTURE));
    assert_eq!(action.action_id, "retail.sku.reorder");
    assert_eq!(action.preconditions.len(), 2);
    assert_eq!(action.writes[0].change, WriteChange::Created);
    assert_eq!(
        action
            .compensation
            .map(|compensation| compensation.action_id),
        Some("retail.sku.cancel_reorder".to_string())
    );
}

#[test]
fn action_commands_are_typed_and_empty_commands_are_omitted() {
    let mut payload = payload_json(ACTION_FIXTURE);
    let action: ActionRoot =
        serde_json::from_value(payload.clone()).expect("legacy action has no commands");
    assert!(action.commands.is_empty());
    assert!(
        serde_json::to_value(action)
            .expect("action should serialize")
            .get("commands")
            .is_none()
    );

    payload["commands"] = serde_json::json!([
        {
            "connector": "erpnext",
            "operation": "submit",
            "doctype": "Sales Order",
            "name": "input.so_number"
        }
    ]);
    let action: ActionRoot = serde_json::from_value(payload).expect("command should decode");
    assert_eq!(action.commands.len(), 1);
    assert_eq!(action.commands[0].connector, "erpnext");
    assert_eq!(action.commands[0].operation, ActionCommandOperation::Submit);
    assert_eq!(
        action.commands[0]
            .name
            .as_ref()
            .map(norn_ir::ZenExpression::as_str),
        Some("input.so_number")
    );
}

#[test]
fn action_ui_effects_are_versioned_and_effects_are_derived_from_commands() {
    let mut payload = payload_json(ACTION_FIXTURE);
    payload["commands"] = serde_json::json!([{
        "connector": "erpnext",
        "operation": "create",
        "doctype": "Purchase Request"
    }]);
    payload["uiEffects"] = serde_json::json!({
        "effectsVersion": "0.1",
        "irreversible": true,
        "destructive": true,
        "money": ["currency.USD"],
        "errorCodes": ["payment.declined"],
        "resultCodes": ["payment.posted"]
    });
    let action: ActionRoot = assert_typed_roundtrip("action UI effects", &payload);
    let effects = action.effective_ui_effects();
    assert_eq!(
        effects,
        std::collections::BTreeSet::from([
            ActionUiEffect::External,
            ActionUiEffect::Async,
            ActionUiEffect::Idempotent,
            ActionUiEffect::Irreversible,
            ActionUiEffect::Destructive,
            ActionUiEffect::Money {
                currency_ref: "currency.USD".to_owned()
            },
            ActionUiEffect::Error {
                code: "OutcomeUnknown".to_owned()
            },
            ActionUiEffect::Error {
                code: "payment.declined".to_owned()
            },
            ActionUiEffect::Result {
                code: "payment.posted".to_owned()
            },
        ])
    );

    let mut deleted_action = action;
    deleted_action.writes.push(norn_ir::ActionWrite {
        entity_kind: "retail.PurchaseRequest".to_owned(),
        change: WriteChange::Deleted,
    });
    assert!(
        deleted_action
            .effective_ui_effects()
            .contains(&ActionUiEffect::Destructive)
    );
}

/// T3-LIFE-3 (#2867): a `fire_transition` effect names the transition it
/// fires; its target and write-set changes come from the closed enum.
#[test]
fn transition_action_fixture_decodes_into_the_typed_root() {
    let action: ActionRoot = assert_typed_roundtrip(
        "commerce-sales-order-submit-invoice.action.json",
        &payload_json(include_str!(
            "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
        )),
    );
    let program = action
        .effect_program
        .expect("the action fires a transition");
    let ActionEffectProgram::FireTransition {
        lifecycle,
        transition,
        ..
    } = &program
    else {
        panic!("expected fire_transition, got {program:?}");
    };
    assert_eq!(
        (lifecycle.as_str(), transition.as_str()),
        ("status", "invoice")
    );
    assert_eq!(program.target().object_kind, "commerce.SalesOrder");
    assert_eq!(program.target().object_id_field, "so_number");
    assert_eq!(program.changes(), [WriteChange::Modified]);
}

#[test]
fn legacy_action_payload_decodes_into_the_typed_root() {
    let payload = serde_json::json!({
        "actionId": "commerce.product.upsert",
        "scopeRequired": ["orgId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "id",
            "objectKind": "commerce.Product",
            "entityArtifactId": "commerce.entity.product",
            "entityArtifactVersion": "0.1.0"
        }
    });
    let action: ActionRoot = assert_typed_roundtrip("legacy action", &payload);
    assert!(action.effect_program.is_some());
    assert!(action.writes.is_empty());
}

#[test]
fn action_derivations_default_to_required_and_preserve_explicit_optional_input() {
    let payload = serde_json::json!({
        "actionId": "commerce.product.normalize",
        "scopeRequired": ["orgId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "properties": { "code": { "type": "string" }, "brand": { "type": "string" } },
            "required": ["code"],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "derive": [
            { "field": "code", "expression": "trim(input.code)" },
            { "field": "brand", "expression": "input.brand ?? \"NORN\"", "inputRequired": false }
        ]
    });
    let action: ActionRoot =
        serde_json::from_value(payload).expect("derivation payload should decode");
    let encoded = serde_json::to_value(action).expect("typed action should encode");
    assert_eq!(
        encoded["derive"],
        serde_json::json!([
            { "field": "code", "expression": "trim(input.code)" },
            { "field": "brand", "expression": "input.brand ?? \"NORN\"", "inputRequired": false }
        ])
    );
}

#[test]
fn rule_v1_conformance_payload_decodes_into_the_typed_root() {
    let rule: RuleRoot =
        assert_typed_roundtrip("rules-dmn-v1 conformance", &v1_conformance_payload());
    assert_eq!(rule.rule_version, RULE_IR_V1_VERSION);
    assert!(rule.graph.is_none());
    let Some(RuleDecisionV1::DecisionTable { rows, .. }) = rule.decision else {
        panic!("conformance rule should be a v1 decision table");
    };
    let ids: Vec<_> = rows.iter().map(|row| row.row_id.as_str()).collect();
    assert_eq!(ids, ["vip_large", "large", "default"]);
}

#[test]
fn rule_v2_fixture_decodes_into_the_typed_jdm_root() {
    let rule: RuleRoot = assert_typed_roundtrip(
        "retail-reorder-routing.rule.json",
        &payload_json(RULE_V2_FIXTURE),
    );
    assert_eq!(rule.rule_version, RULE_IR_V2_VERSION);
    assert!(rule.decision.is_none());
    let graph = rule.graph.expect("v2 rule should carry a graph");
    let types: Vec<_> = graph.nodes.iter().map(JdmNode::node_type).collect();
    assert_eq!(
        types,
        [
            JdmNodeType::InputNode,
            JdmNodeType::ExpressionNode,
            JdmNodeType::SwitchNode,
            JdmNodeType::DecisionTableNode,
            JdmNodeType::DecisionTableNode,
            JdmNodeType::OutputNode,
        ]
    );
    let JdmNode::DecisionTableNode { content, .. } = &graph.nodes[3] else {
        panic!("node 3 should be a decision table");
    };
    assert_eq!(content.hit_policy, JdmHitPolicy::First);
    assert_eq!(content.rules[0].id, "large");
    assert_eq!(content.rules[0].cells["c_total"].as_str(), "> 10000");
    let JdmNode::DecisionTableNode { content, .. } = &graph.nodes[4] else {
        panic!("node 4 should be a decision table");
    };
    assert_eq!(content.hit_policy, JdmHitPolicy::Collect);
}

#[test]
fn every_jdm_node_type_decodes_including_rejected_ones() {
    let nodes = serde_json::json!([
        { "id": "sub", "name": "sub", "type": "decisionNode", "content": { "key": "retail/pricing.json" } },
        { "id": "fn", "name": "fn", "type": "functionNode", "content": { "source": "export const handler = () => ({})" } },
        { "id": "fn1", "name": "fn v1", "type": "functionNode", "content": "module.exports = () => ({})" },
        { "id": "custom", "name": "custom", "type": "customNode", "content": { "kind": "wasm", "config": { "module": "m" } } }
    ]);
    let nodes: Vec<JdmNode> = serde_json::from_value(nodes).expect("nodes should decode");
    let types: Vec<_> = nodes
        .iter()
        .map(|node| (node.id(), node.node_type()))
        .collect();
    assert_eq!(
        types,
        [
            ("sub", JdmNodeType::DecisionNode),
            ("fn", JdmNodeType::FunctionNode),
            ("fn1", JdmNodeType::FunctionNode),
            ("custom", JdmNodeType::CustomNode)
        ]
    );
    for (_, node_type) in types {
        assert_eq!(JdmNodeType::from_wire(node_type.as_str()), Some(node_type));
    }
    assert_eq!(JdmNodeType::from_wire("scriptNode"), None);
}

#[test]
fn typed_roots_are_closed() {
    let unknown_node_member = serde_json::json!({
        "id": "in", "name": "in", "type": "inputNode", "unexpected": true
    });
    assert!(serde_json::from_value::<JdmNode>(unknown_node_member).is_err());

    let unknown_node_type = serde_json::json!({ "id": "x", "name": "x", "type": "scriptNode" });
    assert!(serde_json::from_value::<JdmNode>(unknown_node_type).is_err());

    let mut action = payload_json(ACTION_FIXTURE);
    action["sideChannel"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ActionRoot>(action).is_err());
}

/// Like the #2821 roots, `extensions` may be omitted or an object map but
/// not explicit `null` (#2823).
#[test]
fn null_extensions_are_rejected_by_the_action_and_rule_roots() {
    type Decode = fn(serde_json::Value) -> bool;
    fn decodes<T: serde::de::DeserializeOwned>(payload: serde_json::Value) -> bool {
        serde_json::from_value::<T>(payload).is_ok()
    }
    let roots: [(&str, serde_json::Value, Decode); 3] = [
        (
            "action",
            payload_json(ACTION_FIXTURE),
            decodes::<ActionRoot>,
        ),
        (
            "rule v2",
            payload_json(RULE_V2_FIXTURE),
            decodes::<RuleRoot>,
        ),
        ("rule v1", v1_conformance_payload(), decodes::<RuleRoot>),
    ];
    for (name, mut payload, decode) in roots {
        payload
            .as_object_mut()
            .expect("payload object")
            .remove("extensions");
        assert!(decode(payload.clone()), "{name}: omitted extensions");
        payload["extensions"] =
            serde_json::json!({ "example.com/audit/1.0.0": { "enabled": true } });
        assert!(decode(payload.clone()), "{name}: object extensions");
        payload["extensions"] = serde_json::Value::Null;
        assert!(
            !decode(payload),
            "{name}: extensions: null must be rejected"
        );
    }

    let mut action = payload_json(ACTION_FIXTURE);
    action["uiEffects"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<ActionRoot>(action).is_err());
}

#[test]
fn lenient_envelope_root_keeps_malformed_typed_members_for_the_checker() {
    let mut envelope: serde_json::Value =
        serde_json::from_str(RULE_V2_FIXTURE).expect("fixture should parse");
    envelope["payload"]["graph"]["nodes"][0]["type"] = serde_json::json!("functionNode");
    envelope["payload"]["writes"] = serde_json::json!("not-a-list");
    let decoded: ArtifactEnvelope =
        serde_json::from_value(envelope).expect("lenient root should still decode");
    let ArtifactPayload::Rule(rule) = &decoded.payload else {
        panic!("rule envelope should select the rule root");
    };
    assert!(rule.graph.as_value().is_some());
    assert!(decoded.payload.unknown_fields().contains("writes"));
}

#[test]
fn jdm_editor_metadata_is_modelled() {
    // Shapes emitted by jdm-editor (and present in the pinned zen corpus):
    // table input column `type`, switch `isDefault`, graph `settings`.
    let graph = serde_json::json!({
        "contentType": "application/vnd.gorules.decision",
        "settings": { "validation": { "inputSchema": null } },
        "nodes": [
            {
                "id": "t", "name": "t", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i", "name": "In", "field": "x", "type": "expression" }],
                    "outputs": [{ "id": "o", "name": "Out", "field": "y", "type": "expression" }],
                    "rules": [{ "_id": "r", "i": "> 1", "o": "1" }]
                }
            },
            {
                "id": "s", "name": "s", "type": "switchNode",
                "content": { "statements": [{ "id": "d", "condition": "", "isDefault": true }] }
            }
        ],
        "edges": []
    });
    let decoded: norn_ir::JdmGraph =
        serde_json::from_value(graph.clone()).expect("editor graph should decode");
    let reencoded = serde_json::to_value(&decoded).expect("graph should encode");
    assert_eq!(
        canonical_hash(&serde_json::to_vec(&reencoded).expect("encode")),
        canonical_hash(&serde_json::to_vec(&graph).expect("encode"))
    );
}

/// Every `.rule.json` file under `dir` (relative to the repo root).
fn rule_corpus(dir: &str) -> Vec<(String, serde_json::Value)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files: Vec<_> = std::fs::read_dir(root.join(dir))
        .unwrap_or_else(|error| panic!("{dir} should be readable: {error}"))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.to_string_lossy().ends_with(".rule.json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("fixture should be readable");
            let value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()));
            (path.display().to_string(), value)
        })
        .collect()
}

/// T3-DEC-1: every valid `RuleIR` v2 corpus rule decodes into the typed JDM
/// root with its contract, round-trips unchanged, and a decide graph's schemas
/// have exactly the decision contract members.
#[test]
fn rule_ir_v2_corpus_decodes_into_the_typed_root() {
    let mut corpus = rule_corpus("fixtures/rule-ir-v2/valid");
    corpus.extend(
        rule_corpus("fixtures/rule-ir-v2/sets")
            .into_iter()
            .filter(|(_, envelope)| envelope["payload"].get("graph").is_some()),
    );
    assert!(
        corpus.len() >= 7,
        "the RuleIR v2 corpus is missing fixtures"
    );
    let mut contracts = Vec::new();
    for (name, envelope) in &corpus {
        let rule: RuleRoot = assert_typed_roundtrip(name, &envelope["payload"]);
        assert_eq!(rule.rule_version, RULE_IR_V2_VERSION, "{name}");
        assert!(rule.graph.is_some() && rule.decision.is_none(), "{name}");
        let contract = rule
            .contract
            .unwrap_or_else(|| panic!("{name} has no contract"));
        if contract == RuleContractV2::Decide {
            for (schema, members) in [
                (&rule.input_schema, &DECIDE_INPUT_MEMBERS[..]),
                (&rule.output_schema, &DECIDE_OUTPUT_MEMBERS[..]),
            ] {
                let declared: Vec<&str> = schema["properties"]
                    .as_object()
                    .expect("closed record")
                    .keys()
                    .map(String::as_str)
                    .collect();
                let mut expected = members.to_vec();
                expected.sort_unstable();
                assert_eq!(declared, expected, "{name}");
            }
        }
        contracts.push(contract);
    }
    assert!(contracts.contains(&RuleContractV2::Decide));
    assert!(contracts.contains(&RuleContractV2::SubDecision));
}

/// T3-DEC-1: the rejection corpus stays decodable as wire JSON (the checker,
/// not serde, owns the Norn-profile verdict), and `contract` is a closed enum.
#[test]
fn rule_ir_v2_contract_is_closed_and_rejections_are_checker_findings() {
    for contract in ["decide", "sub_decision"] {
        let decoded: RuleContractV2 =
            serde_json::from_value(serde_json::json!(contract)).expect("admitted contract");
        assert_eq!(
            serde_json::to_value(decoded).expect("contract should encode"),
            serde_json::json!(contract)
        );
    }
    assert!(serde_json::from_value::<RuleContractV2>(serde_json::json!("process")).is_err());

    for (name, envelope) in rule_corpus("fixtures/rule-ir-v2/invalid") {
        let decoded: Result<ArtifactEnvelope, _> = serde_json::from_value(envelope);
        let envelope = decoded.unwrap_or_else(|error| panic!("{name} should decode: {error}"));
        assert_eq!(envelope.kind(), ArtifactKind::Rule, "{name}");
        let ArtifactPayload::Rule(_) = envelope.payload else {
            panic!("{name} should carry a rule payload");
        };
    }
}
