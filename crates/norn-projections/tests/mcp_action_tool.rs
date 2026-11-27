#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeSet;

use norn_ir::ActionRoot;
use norn_projections::mcp::{McpProjectionError, action_tool, action_tool_with_server_filled};
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
        "properties": {"code": {"type": "string"}, "brand": {"type": "string"}, "amount": {"type": "number", "nornType": {"kind":"decimal", "scale":2}}},
        "required": ["code", "brand", "amount"]
    }))
    .expect("schema decodes");
    action.derive = serde_json::from_value(json!([
        {"field": "code", "expression": "trim(input.code)"},
        {"field": "code", "expression": "upper(input.code)"},
        {"field": "brand", "expression": "input.brand ?? 'NORN'", "inputRequired": false},
        {"field": "amount", "expression": "input.amount ?? 0", "inputRequired": false}
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
        tool["inputSchema"]["properties"]["amount"]["anyOf"],
        json!([{"type":"string", "nornType":{"kind":"decimal","scale":2.0},
            "description":"Exact decimal text as a JSON string, for example \"125\". Use a dot decimal separator; do not send a JSON number."},{"type":"null"}])
    );
    assert_eq!(
        tool["_meta"]["norn/inputDerivations"],
        json!([
            {"field": "code", "expression": "trim(input.code)", "inputRequired": true},
            {"field": "code", "expression": "upper(input.code)", "inputRequired": true},
            {"field": "brand", "expression": "input.brand ?? 'NORN'", "inputRequired": false},
            {"field": "amount", "expression": "input.amount ?? 0", "inputRequired": false}
        ])
    );
    assert_eq!(
        action
            .input_schema
            .get("required")
            .and_then(norn_ir::JsonValue::as_array)
            .map(<[norn_ir::JsonValue]>::len),
        Some(3)
    );
    let mut schema = serde_json::to_value(&action.input_schema).expect("schema");
    schema["properties"]["amount"]["description"] = json!("Author supplied decimal guidance");
    action.input_schema = serde_json::from_value(schema).expect("typed schema");
    let described =
        serde_json::to_value(action_tool(&action).expect("described tool")).expect("tool");
    assert_eq!(
        described["inputSchema"]["properties"]["amount"]["anyOf"][0]["description"],
        "Author supplied decimal guidance"
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
    let annotations = assert_risk_hints(&create_only, "create-only command", true, true, true);
    assert_eq!(annotations.read_only_hint, Some(false));

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
        assert_eq!(annotations.idempotent_hint, Some(idempotent));
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
    let action = action_from_fixture(include_str!(
        "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
    ));
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
    let mut bound = serde_json::to_value(&action).expect("action encodes");
    bound["effectProgram"]["acceptReceipt"] = json!({
        "referenceField":"receipt", "manifestDigest":format!("sha256:{}", "a".repeat(64)),
        "digestProperty":"digest", "resultBindings":{"score":"score"}
    });
    let bound: ActionRoot = serde_json::from_value(bound.clone()).expect("receipt effect decodes");
    let projected = serde_json::to_value(action_tool(&bound).expect("projects")).expect("encodes");
    assert_eq!(
        projected["_meta"]["norn/transition"]["acceptReceipt"],
        serde_json::to_value(bound).expect("encodes")["effectProgram"]["acceptReceipt"]
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

#[test]
fn server_filled_fields_leave_the_caller_schema() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object", "additionalProperties": false,
        "properties": {"id": {"type": "string"}, "corrected_by": {"type": "string"}, "brand": {"type": "string"}},
        "required": ["id", "corrected_by", "brand"]
    }))
    .expect("schema decodes");
    action.derive = serde_json::from_value(json!([
        {"field": "corrected_by", "expression": "actor.subjectId", "inputRequired": false},
        {"field": "brand", "expression": "input.brand ?? 'NORN'", "inputRequired": false}
    ]))
    .expect("derivations decode");
    let tool = serde_json::to_value(action_tool(&action).expect("projects")).expect("encodes");
    let schema = &tool["inputSchema"];
    assert!(
        schema["properties"].get("corrected_by").is_none(),
        "the actor is the server's to fill"
    );
    assert_eq!(schema["required"], json!(["id"]));
    assert!(
        schema["properties"]["brand"]["anyOf"].is_array(),
        "a default over input stays the caller's"
    );
    assert_eq!(
        tool["_meta"]["norn/inputDerivations"][0]["field"],
        "corrected_by"
    );
}

#[test]
fn decimal_wire_schema_rewrites_nested_arrays_and_server_filled_ignores_string_and_member_input() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "amount": {
                "anyOf": [
                    {"type": "number", "nornType": {"kind": "decimal"}},
                    {"type": "null"}
                ]
            },
            "note": {"type": "string"},
            "actor_input": {"type": "string"},
            "already_nullable": {"anyOf": [{"type": "string"}, {"type": "null"}]}
        },
        "required": ["amount", "note", "actor_input", "already_nullable"]
    }))
    .expect("schema decodes");
    action.derive = serde_json::from_value(json!([
        {"field": "note", "expression": "'user input'", "inputRequired": false},
        {"field": "actor_input", "expression": "actor.input", "inputRequired": false},
        {"field": "already_nullable", "expression": "input.already_nullable ?? 'x'", "inputRequired": false}
    ]))
    .expect("derivations decode");
    let tool = serde_json::to_value(action_tool(&action).expect("projects")).expect("encodes");
    let props = &tool["inputSchema"]["properties"];
    assert_eq!(props["amount"]["anyOf"][0]["type"], "string");
    assert!(props.get("note").is_none());
    assert!(props.get("actor_input").is_none());
    // Already-nullable property is not wrapped in nested anyOf.
    assert_eq!(props["already_nullable"]["anyOf"][0]["type"], "string");
    assert_eq!(props["already_nullable"]["anyOf"][1]["type"], "null");
}

#[test]
fn nullable_decimal_type_arrays_rewrite_number_to_string() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "price": {"type": ["number", "null"], "nornType": {"kind": "decimal", "scale": 2}},
            "discount": {"type": ["null", "number"], "nornType": {"kind": "decimal", "scale": 4}},
            "plain_num": {"type": ["number", "null"]}
        },
        "required": ["price"]
    }))
    .expect("schema decodes");
    let tool = serde_json::to_value(action_tool(&action).expect("projects")).expect("encodes");
    let props = &tool["inputSchema"]["properties"];
    assert_eq!(props["price"]["type"], json!(["string", "null"]));
    assert!(
        props["price"]["description"]
            .as_str()
            .is_some_and(|desc| desc.contains("Exact decimal text"))
    );
    assert_eq!(props["discount"]["type"], json!(["null", "string"]));
    assert!(
        props["discount"]["description"]
            .as_str()
            .is_some_and(|desc| desc.contains("Exact decimal text"))
    );
    assert_eq!(props["plain_num"]["type"], json!(["number", "null"]));
}

#[test]
fn mixed_required_and_optional_derivations_keep_field_required_and_actor_derived_omitted() {
    let mut action = fixture_action();
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "code": {"type": "string"},
            "owner": {"type": "string"},
            "root_actor": {"type": "string"},
            "root_input": {"type": "string"},
            "greeting": {"type": "string"},
            "actor_greeting": {"type": "string"},
            "escaped_template_actor": {"type": "string"},
            "escaped_quote_actor": {"type": "string"},
            "extra_secret": {"type": "string"}
        },
        "required": [
            "code",
            "owner",
            "root_actor",
            "root_input",
            "greeting",
            "actor_greeting",
            "escaped_template_actor",
            "escaped_quote_actor",
            "extra_secret"
        ]
    }))
    .expect("schema decodes");
    action.derive = serde_json::from_value(json!([
        {"field": "code", "expression": "trim(input.code)", "inputRequired": true},
        {"field": "code", "expression": "input.code ?? 'FALLBACK'", "inputRequired": false},
        {"field": "owner", "expression": "input.owner ?? actor.subjectId", "inputRequired": false},
        {"field": "root_actor", "expression": "$.actor.subjectId", "inputRequired": false},
        {"field": "root_input", "expression": "$.input.root_input ?? 'DEFAULT'", "inputRequired": false},
        {"field": "greeting", "expression": "`Hello actor $5 ${input.greeting}`", "inputRequired": false},
        {"field": "actor_greeting", "expression": "`Hello ${actor.subjectId}`", "inputRequired": false},
        {"field": "escaped_template_actor", "expression": "`Hello \\${actor.subjectId}`", "inputRequired": false},
        {"field": "escaped_quote_actor", "expression": "'x\\' + actor.subjectId", "inputRequired": false}
    ]))
    .expect("derivations decode");

    let extra = BTreeSet::from(["extra_secret".to_owned()]);
    let tool =
        serde_json::to_value(action_tool_with_server_filled(&action, &extra).expect("projects"))
            .expect("encodes");
    let schema = &tool["inputSchema"];
    assert_eq!(schema["required"], json!(["code"]));
    assert_eq!(schema["properties"]["code"], json!({"type": "string"}));
    assert!(
        schema["properties"].get("owner").is_none(),
        "actor-reading optional derivation is server-filled even when also referencing input"
    );
    assert!(
        schema["properties"].get("root_actor").is_none(),
        "$.actor optional derivation is server-filled"
    );
    assert!(
        schema["properties"].get("root_input").is_none(),
        "$ whole-environment root fails closed as server-filled, matching kernel actor_derived_fields"
    );
    assert!(
        schema["properties"]["greeting"]["anyOf"].is_array(),
        "template interpolation `${input.greeting}` must not be misread as whole-environment `$`"
    );
    assert!(
        schema["properties"].get("actor_greeting").is_none(),
        "template interpolation `${actor.subjectId}` is still detected as an actor read"
    );
    assert!(
        schema["properties"].get("escaped_template_actor").is_none(),
        "ZEN template_string has no backslash escapes, so `\\${actor.subjectId}` still reads actor"
    );
    assert!(
        schema["properties"].get("escaped_quote_actor").is_none(),
        "ZEN string literals have no backslash escapes, so `'x\\' + actor.subjectId` still reads actor"
    );
    assert!(
        schema["properties"].get("extra_secret").is_none(),
        "extra_server_filled fields leave the caller schema"
    );
}

#[test]
fn destructive_hint_covers_deleted_writes_and_writes_alongside_create_only_effect() {
    let mut parent_delete = fixture_action();
    parent_delete.writes.clear();
    parent_delete.effect_program = Some(norn_ir::ActionEffectProgram::MutateParentWithRows {
        object_id_field: "orderId".to_owned(),
        object_kind: "commerce.SalesOrder".to_owned(),
        entity_artifact_id: "commerce.SalesOrder.entity".to_owned(),
        entity_artifact_version: "0.1.0".to_owned(),
        parent_values_field: "parent".to_owned(),
        row_ops_field: "rows".to_owned(),
        parent_version_field: "version".to_owned(),
        parent_delete_field: Some("deleteParent".to_owned()),
        collections: Vec::new(),
    });
    let annotations = action_tool(&parent_delete)
        .expect("projects")
        .annotations
        .expect("annotations");
    assert_eq!(annotations.destructive_hint, Some(true));

    for change in [
        norn_ir::WriteChange::Modified,
        norn_ir::WriteChange::Deleted,
    ] {
        let mut create_effect_with_destructive_write = fixture_action();
        create_effect_with_destructive_write.writes[0].change = change;
        create_effect_with_destructive_write.effect_program =
            Some(norn_ir::ActionEffectProgram::UpsertInputObject {
                object_id_field: "requestId".to_owned(),
                object_kind: "retail.PurchaseRequest".to_owned(),
                entity_artifact_id: "retail.PurchaseRequest.entity".to_owned(),
                entity_artifact_version: "0.1.0".to_owned(),
                write_mode: norn_ir::UpsertWriteMode::CreateOnly,
            });
        let annotations = action_tool(&create_effect_with_destructive_write)
            .expect("projects")
            .annotations
            .expect("annotations");
        assert_eq!(
            annotations.destructive_hint,
            Some(true),
            "CreateOnly effect program must not mask {change:?} in action.writes"
        );
    }
}
