#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_ir::ActionRoot;
use norn_projections::mcp::{McpProjectionError, action_tool};
use serde_json::{Value, json};

fn action_from_fixture(source: &str) -> ActionRoot {
    let envelope: Value = serde_json::from_str(source).expect("fixture should parse");
    serde_json::from_value(envelope["payload"].clone()).expect("payload should be an ActionRoot")
}

fn fixture_action() -> ActionRoot {
    action_from_fixture(include_str!(
        "../../../fixtures/ir/retail-reorder.action.json"
    ))
}

#[test]
fn derived_defaults_are_optional_but_normalized_caller_inputs_remain_required() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object", "additionalProperties": false,
        "properties": {"code": {"type": "string"}, "brand": {"type": "string"}},
        "required": ["code", "brand"]
    }))
    .expect("schema decodes");
    action.derive = serde_json::from_value(json!([
        {"field": "code", "expression": "trim(input.code)"},
        {"field": "code", "expression": "upper(input.code)"},
        {"field": "brand", "expression": "input.brand ?? 'NORN'", "inputRequired": false}
    ]))
    .expect("derivations decode");
    let hints = action_tool(&fixture_action())
        .expect("fixture projects")
        .annotations;
    let projected = action_tool(&action).expect("action projects");
    assert_eq!(
        projected.annotations, hints,
        "derivation leaves UI effects unchanged"
    );
    let tool = serde_json::to_value(projected).expect("tool encodes");
    assert_eq!(tool["inputSchema"]["required"], json!(["code"]));
    assert_eq!(
        tool["inputSchema"]["properties"]["brand"]["anyOf"],
        json!([{"type": "string"}, {"type": "null"}])
    );
    assert_eq!(
        tool["_meta"]["norn/inputDerivations"],
        json!([
            {"field": "code", "expression": "trim(input.code)", "inputRequired": true},
            {"field": "code", "expression": "upper(input.code)", "inputRequired": true},
            {"field": "brand", "expression": "input.brand ?? 'NORN'", "inputRequired": false}
        ])
    );
    assert_eq!(
        action
            .input_schema
            .get("required")
            .and_then(norn_ir::JsonValue::as_array)
            .map(<[norn_ir::JsonValue]>::len),
        Some(2)
    );
}

#[test]
fn required_derivation_sources_are_required_even_when_final_schema_is_optional() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object", "additionalProperties": false,
        "properties": {"code": {"type": "string"}}
    }))
    .expect("optional final schema");
    action.derive = serde_json::from_value(json!([
        {"field": "code", "expression": "trim(input.code)"},
        {"field": "code", "expression": "upper(input.code)"}
    ]))
    .expect("ordered normalization");
    let projected = serde_json::to_value(action_tool(&action).expect("projects")).expect("encodes");
    assert_eq!(projected["inputSchema"]["required"], json!(["code"]));
    assert!(!action.input_schema.contains_key("required"));
}

/// The destructive predicate from `origin/main` before outbound-command risks
/// were projected.
fn legacy_destructive(action: &ActionRoot) -> bool {
    let legacy_read_only =
        !action.ledgered && action.effect_program.is_none() && action.writes.is_empty();
    let legacy_additive = action.effect_program.as_ref().map_or_else(
        || {
            !action.writes.is_empty()
                && action
                    .writes
                    .iter()
                    .all(|write| write.change == norn_ir::WriteChange::Created)
        },
        |program| !program.changes().contains(&norn_ir::WriteChange::Modified),
    );
    !legacy_read_only && !legacy_additive
}

fn assert_risk_hints(
    action: &ActionRoot,
    label: &str,
    expected_legacy_destructive: bool,
    expected_destructive: bool,
    expected_open_world: bool,
) -> norn_ir::McpToolAnnotations {
    let actual_legacy_destructive = legacy_destructive(action);
    assert_eq!(
        actual_legacy_destructive, expected_legacy_destructive,
        "{label}: legacy destructive predicate"
    );

    let annotations = action_tool(action)
        .expect("action projects")
        .annotations
        .expect("annotations are always derived");
    assert_eq!(
        annotations.destructive_hint,
        Some(expected_destructive),
        "{label}: destructiveHint"
    );
    assert_eq!(
        annotations.open_world_hint,
        Some(expected_open_world),
        "{label}: openWorldHint"
    );
    assert!(
        !actual_legacy_destructive || expected_destructive,
        "{label}: the new hint must preserve legacy destructive risk"
    );
    assert_eq!(
        annotations.idempotent_hint,
        Some(action.idempotent),
        "{label}: idempotentHint must follow the declared action flag"
    );
    annotations
}

fn command(operation: &str) -> norn_ir::ActionCommand {
    let mut value = json!({
        "connector": "erpnext",
        "operation": operation,
        "doctype": "Purchase Request"
    });
    if operation != "create" {
        value["name"] = json!("input.requestId");
    }
    serde_json::from_value(value).expect("command should decode")
}

#[test]
fn action_projects_to_an_mcp_2026_07_28_tool() {
    let action = fixture_action();
    let tool = serde_json::to_value(action_tool(&action).expect("fixture projects"))
        .expect("tool should encode");
    assert_eq!(tool["name"], "retail.sku.reorder");
    assert_eq!(tool["title"], "Reorder SKU");
    assert_eq!(
        tool["description"],
        "Create a purchase request that replenishes one SKU to its target stock level."
    );
    assert_eq!(tool["inputSchema"]["type"], "object");
    assert_eq!(tool["outputSchema"]["required"], json!(["requestId"]));
    assert_eq!(
        tool["annotations"],
        json!({
            "title": "Reorder SKU",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        })
    );
    // Only MCP `Tool` members; no Norn-only field leaks into the projection.
    // Sorted: `serde_json::Map` keeps insertion order when `preserve_order` is
    // feature-unified in (cedar-policy via norn-authz, #2882).
    let mut keys: Vec<_> = tool
        .as_object()
        .expect("tool is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "annotations",
            "description",
            "inputSchema",
            "name",
            "outputSchema",
            "title"
        ]
    );
}

#[test]
fn annotations_follow_the_declared_effects() {
    let mut action = fixture_action();
    action.ledgered = false;
    action.writes.clear();
    let tool = action_tool(&action).expect("fixture projects");
    let annotations = tool.annotations.expect("annotations are always derived");
    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.destructive_hint, Some(false));
    assert_eq!(annotations.idempotent_hint, Some(true));

    let mut action = fixture_action();
    action.writes[0].change = norn_ir::WriteChange::Modified;
    let annotations = action_tool(&action)
        .expect("fixture projects")
        .annotations
        .expect("derived");
    assert_eq!(annotations.destructive_hint, Some(true));
}

#[test]
fn commands_drive_open_world_and_conservative_mutation_hints() {
    let mut create_only = fixture_action();
    create_only.ledgered = false;
    create_only.writes.clear();
    create_only.commands = vec![
        serde_json::from_value(json!({
            "connector": "erpnext",
            "operation": "create",
            "doctype": "Purchase Request"
        }))
        .expect("create command should decode"),
    ];
    let annotations = action_tool(&create_only)
        .expect("command action projects")
        .annotations
        .expect("derived annotations");
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(false));
    assert_eq!(annotations.idempotent_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(true));

    let mut update = fixture_action();
    update.commands = vec![
        serde_json::from_value(json!({
            "connector": "erpnext",
            "operation": "update",
            "doctype": "Purchase Request",
            "name": "input.requestId"
        }))
        .expect("update command should decode"),
    ];
    let annotations = action_tool(&update)
        .expect("command action projects")
        .annotations
        .expect("derived annotations");
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(true));
}

#[test]
fn ledgered_create_command_without_local_writes_keeps_legacy_destructive_risk() {
    let mut action = fixture_action();
    action.writes.clear();
    action.commands = vec![command("create")];

    let annotations = assert_risk_hints(&action, "ledgered create-only command", true, true, true);
    assert_eq!(annotations.read_only_hint, Some(false));
}

#[test]
fn action_fixture_risks_match_the_legacy_baseline_or_increase() {
    // These are all checked-in `*.action.json` fixtures under fixtures/ir.
    let fixtures = [
        (
            "retail reorder",
            include_str!("../../../fixtures/ir/retail-reorder.action.json"),
            false,
            false,
            false,
        ),
        (
            "invoice transition",
            include_str!("../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"),
            true,
            true,
            false,
        ),
        (
            "invalid write mode",
            include_str!("../../../fixtures/ir/artifact-set/action-invalid-write-mode.action.json"),
            true,
            true,
            false,
        ),
    ];

    for (label, source, old_destructive, destructive, open_world) in fixtures {
        let action = action_from_fixture(source);
        assert_risk_hints(&action, label, old_destructive, destructive, open_world);
    }
}

#[test]
fn synthetic_write_command_and_declared_effect_risks_are_monotonic() {
    let mut create_only = fixture_action();
    create_only.writes.clear();
    create_only.commands = vec![command("create")];
    assert_risk_hints(&create_only, "create-only command", true, true, true);

    let mut update_only = fixture_action();
    update_only.writes.clear();
    update_only.commands = vec![command("update")];
    assert_risk_hints(&update_only, "update-only command", true, true, true);

    let mut created_write = fixture_action();
    assert_risk_hints(&created_write, "created write", false, false, false);

    created_write.writes[0].change = norn_ir::WriteChange::Modified;
    assert_risk_hints(&created_write, "modified write", true, true, false);

    let mut create_only_effect = fixture_action();
    create_only_effect.writes.clear();
    create_only_effect.effect_program = Some(norn_ir::ActionEffectProgram::UpsertInputObject {
        object_id_field: "requestId".to_owned(),
        object_kind: "retail.PurchaseRequest".to_owned(),
        entity_artifact_id: "retail.PurchaseRequest.entity".to_owned(),
        entity_artifact_version: "0.1.0".to_owned(),
        write_mode: norn_ir::UpsertWriteMode::CreateOnly,
    });
    let annotations = assert_risk_hints(
        &create_only_effect,
        "create-only effect program",
        false,
        false,
        false,
    );
    assert_eq!(annotations.read_only_hint, Some(false));

    for (idempotent, label) in [
        (false, "non-idempotent read-only"),
        (true, "idempotent read-only"),
    ] {
        let mut read_only = fixture_action();
        read_only.ledgered = false;
        read_only.writes.clear();
        read_only.idempotent = idempotent;
        let annotations = assert_risk_hints(&read_only, label, false, false, false);
        assert_eq!(annotations.read_only_hint, Some(true));
    }

    for (irreversible, destructive, label) in [
        (true, false, "irreversible UI effect"),
        (false, true, "destructive UI effect"),
    ] {
        let mut action = fixture_action();
        action.ledgered = false;
        action.writes.clear();
        action.idempotent = false;
        action.ui_effects = Some(norn_ir::ActionUiEffects {
            effects_version: norn_ir::IrVersion01::V0_1,
            irreversible,
            destructive,
            money: Vec::new(),
            error_codes: Vec::new(),
            result_codes: Vec::new(),
        });
        let annotations = assert_risk_hints(&action, label, false, true, false);
        assert_eq!(annotations.read_only_hint, Some(false));
    }
}

#[test]
fn declared_irreversible_ui_effect_projects_as_destructive() {
    let mut action = fixture_action();
    action.ui_effects = Some(norn_ir::ActionUiEffects {
        effects_version: norn_ir::IrVersion01::V0_1,
        irreversible: true,
        destructive: false,
        money: vec!["currency.USD".to_owned()],
        error_codes: Vec::new(),
        result_codes: Vec::new(),
    });
    let annotations = action_tool(&action)
        .expect("declared effects project")
        .annotations
        .expect("derived annotations");
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(true));
    assert_eq!(annotations.idempotent_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(false));
}

#[test]
fn non_object_input_schema_does_not_project() {
    let mut action = fixture_action();
    action.input_schema =
        serde_json::from_value(json!({ "type": "string" })).expect("schema should decode");
    assert_eq!(
        action_tool(&action),
        Err(McpProjectionError::InputSchemaNotObject)
    );
}

/// T3-LIFE-3 (#2867): a `fire_transition` action projects to a destructive
/// tool whose `_meta` names the declared transition it fires.
#[test]
fn a_transition_action_names_its_transition_in_meta() {
    let envelope: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
    ))
    .expect("fixture should parse");
    let action: ActionRoot =
        serde_json::from_value(envelope["payload"].clone()).expect("payload is an ActionRoot");
    let tool = serde_json::to_value(action_tool(&action).expect("fixture projects"))
        .expect("tool should encode");
    assert_eq!(tool["name"], "commerce.sales_order.submit_invoice");
    assert_eq!(
        tool["annotations"],
        json!({
            "title": "Submit invoice",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        })
    );
    assert_eq!(
        tool["_meta"],
        json!({
            "norn/transition": {
                "objectKind": "commerce.SalesOrder",
                "lifecycle": "status",
                "transition": "invoice"
            }
        })
    );
    // An action without a transition effect has no `_meta`.
    assert_eq!(
        action_tool(&fixture_action())
            .expect("fixture projects")
            .meta,
        None
    );
}

#[test]
fn input_derivation_metadata_preserves_transition_metadata() {
    let mut action = action_from_fixture(include_str!(
        "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
    ));
    let original = serde_json::to_value(action_tool(&action).expect("fixture projects"))
        .expect("tool encodes");
    action.derive = serde_json::from_value(json!([
        {"field": "so_number", "expression": "trim(input.so_number)"}
    ]))
    .expect("derivation decodes");
    let tool =
        serde_json::to_value(action_tool(&action).expect("action projects")).expect("tool encodes");
    assert_eq!(
        tool["_meta"]["norn/transition"],
        original["_meta"]["norn/transition"]
    );
    assert_eq!(tool["annotations"], original["annotations"]);
    assert_eq!(
        tool["inputSchema"]["required"],
        original["inputSchema"]["required"]
    );
    assert_eq!(
        tool["_meta"]["norn/inputDerivations"][0]["inputRequired"],
        true
    );
}
