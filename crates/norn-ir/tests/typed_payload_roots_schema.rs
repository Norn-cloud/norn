#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use std::{collections::BTreeMap, fs, path::Path};

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, JsonValue, artifact_envelope_schema,
    artifact_payload_schema, data_quality_finding_schema, fact_change_schema, interface_def_schema,
    object_type_def_schema, object_type_extension_schema, property_type_def_schema,
};

const UPDATE_SCHEMAS_ENV: &str = "NORN_IR_UPDATE_PAYLOAD_SCHEMAS";

fn payload_root_cases() -> [(&'static str, ArtifactKind); 14] {
    [
        ("entity.schema.json", ArtifactKind::Entity),
        ("relation.schema.json", ArtifactKind::Relation),
        ("action.schema.json", ArtifactKind::Action),
        ("rule.schema.json", ArtifactKind::Rule),
        ("connector.schema.json", ArtifactKind::Connector),
        ("widget.schema.json", ArtifactKind::Widget),
        ("customization.schema.json", ArtifactKind::Customization),
        ("setting.schema.json", ArtifactKind::Setting),
        ("business_pack.schema.json", ArtifactKind::BusinessPack),
        ("policy.schema.json", ArtifactKind::Policy),
        ("derived.schema.json", ArtifactKind::Derived),
        ("source_schema.schema.json", ArtifactKind::SourceSchema),
        ("mapping.schema.json", ArtifactKind::Mapping),
        ("law.schema.json", ArtifactKind::Law),
    ]
}

#[test]
fn payload_root_schemas_are_checked_in_and_current() {
    let schema_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    let update = std::env::var_os(UPDATE_SCHEMAS_ENV).is_some();

    if update {
        fs::create_dir_all(&schema_dir).expect("schema directory should be creatable");
    }

    for (filename, kind) in payload_root_cases() {
        assert_schema_is_current(
            &schema_dir,
            filename,
            &artifact_payload_schema(&kind),
            update,
        );
    }

    assert_schema_is_current(
        &schema_dir,
        "artifact_envelope.schema.json",
        &artifact_envelope_schema(),
        update,
    );

    for (filename, schema) in [
        ("metric_ir.schema.json", norn_ir::metric_ir_schema()),
        ("question_ir.schema.json", norn_ir::question_ir_schema()),
    ] {
        assert_schema_is_current(&schema_dir, filename, &schema, update);
    }

    // EntityIR v2 vocabulary (T1-ONT-1a): not an envelope root yet.
    assert_schema_is_current(
        &schema_dir,
        "property_type_def.schema.json",
        &property_type_def_schema(),
        update,
    );

    // EntityIR v2 object types (T1-ONT-1b): not an envelope root yet.
    assert_schema_is_current(
        &schema_dir,
        "object_type_def.schema.json",
        &object_type_def_schema(),
        update,
    );

    // EntityIR v2 interfaces and extensions (T1-ONT-1c): not envelope roots
    // yet.
    assert_schema_is_current(
        &schema_dir,
        "interface_def.schema.json",
        &interface_def_schema(),
        update,
    );
    assert_schema_is_current(
        &schema_dir,
        "object_type_extension.schema.json",
        &object_type_extension_schema(),
        update,
    );

    // The canonical decide-graph fact change (T3-LIFE-2): the item contract
    // of a decide graph's `facts` output list.
    assert_schema_is_current(
        &schema_dir,
        "fact_change.schema.json",
        &fact_change_schema(),
        update,
    );

    // A versioned observation finding is a first-class typed payload.
    assert_schema_is_current(
        &schema_dir,
        "data_quality_finding.schema.json",
        &data_quality_finding_schema(),
        update,
    );
}

#[expect(
    clippy::print_stdout,
    reason = "CI schema output when artifact storage is full"
)]
fn assert_schema_is_current(
    schema_dir: &Path,
    filename: &str,
    schema: &schemars::Schema,
    update: bool,
) {
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&schema).expect("schema should serialize")
    );
    let path = schema_dir.join(filename);

    if update {
        if fs::read_to_string(&path).ok().as_deref() != Some(rendered.as_str()) {
            println!("---BEGIN {filename}---\n{rendered}---END {filename}---");
        }
        fs::write(&path, &rendered).expect("schema should write");
    }

    // Compare as JSON values: object member order depends on the
    // `serde_json` map backend, and `preserve_order` is feature-unified into
    // any build that includes `norn-authz` via `cedar-policy` (#2882). Map
    // equality is order-insensitive under both backends. A missing or stale
    // file fails with the rendered schema between markers, so a CI-only
    // workflow can check it in from the job log.
    let checked_in: Option<serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let current: serde_json::Value =
        serde_json::from_str(&rendered).expect("rendered schema should parse");
    assert!(
        checked_in.as_ref() == Some(&current),
        "payload schema {} is missing or stale; regenerate with {UPDATE_SCHEMAS_ENV}=1 cargo test -p norn-ir payload_root_schemas_are_checked_in_and_current\n---BEGIN {filename}---\n{rendered}---END {filename}---",
        path.display()
    );
}

#[test]
fn payload_root_schemas_mark_unconditionally_required_members() {
    // Entity, Policy, and BusinessPack have several closed forms (`anyOf`);
    // see `entity_payload_schema_is_the_v1_or_v2_typed_root` and
    // `policy_and_business_pack_schemas_are_their_typed_forms`.
    let expected = [
        (ArtifactKind::Relation, &["from", "to"][..]),
        (
            ArtifactKind::Action,
            &[
                "actionId",
                "scopeRequired",
                "idempotent",
                "ledgered",
                "inputSchema",
                "outputSchema",
            ][..],
        ),
        (ArtifactKind::Rule, &["ruleId"][..]),
        (
            ArtifactKind::Connector,
            &["connectorVersion", "auth", "resources", "mappings"][..],
        ),
        (
            ArtifactKind::Widget,
            &[
                "widgetVersion",
                "widgetId",
                "dataDependencies",
                "viewStates",
                "actionBindings",
                "permissions",
            ][..],
        ),
        (
            ArtifactKind::Customization,
            &[
                "customizationVersion",
                "customizationId",
                "intent",
                "targetArtifact",
                "overlay",
                "approval",
                "scopeRequired",
            ][..],
        ),
        (
            ArtifactKind::Setting,
            &[
                "settingVersion",
                "settingKey",
                "valueType",
                "value",
                "effectiveFrom",
                "review",
                "scopeRequired",
            ][..],
        ),
        (
            ArtifactKind::Derived,
            &[
                "viewVersion",
                "viewId",
                "inputs",
                "calc",
                "outputSchema",
                "maintenance",
            ][..],
        ),
        (
            ArtifactKind::SourceSchema,
            &["format", "source", "observedAt", "adapter", "rawMetadata"][..],
        ),
        (
            ArtifactKind::Mapping,
            &["format", "source", "interfaces", "maps"][..],
        ),
    ];

    for (kind, expected_required) in expected {
        let schema = serde_json::to_value(artifact_payload_schema(&kind))
            .expect("payload root schema should serialize");
        let required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .expect("payload root schema should list required fields")
            .iter()
            .map(|value| value.as_str().expect("required field should be a string"))
            .collect::<Vec<_>>();
        assert_eq!(
            required, expected_required,
            "{kind:?} required fields changed"
        );
        assert_eq!(
            schema["properties"]["extensions"]["type"], "object",
            "{kind:?} extensions must remain an object map when present"
        );
    }
}

#[test]
fn connector_missing_items_flag_schema_is_optional_but_non_null() {
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::Connector))
        .expect("connector payload schema should serialize");
    let variants = schema["$defs"]["ConnectorResource"]["oneOf"]
        .as_array()
        .expect("connector resource should expose its typed forms");
    let mut kinds = Vec::new();

    for variant in variants {
        let Some(property) = variant
            .get("properties")
            .and_then(|properties| properties.get("missingItemsIsEmpty"))
        else {
            continue;
        };
        let kind = variant["properties"]["kind"]["const"]
            .as_str()
            .expect("resource variant kind should be a string");
        kinds.push(kind);
        assert_eq!(property["type"], "boolean", "{kind} flag must reject null");
        assert!(
            !variant["required"]
                .as_array()
                .expect("resource variant should list required fields")
                .contains(&serde_json::json!("missingItemsIsEmpty")),
            "{kind} flag should remain optional"
        );
    }

    assert_eq!(kinds, vec!["paginated", "cursor", "delta"]);
}

#[test]
fn entity_payload_schema_is_the_v1_or_v2_typed_root() {
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::Entity))
        .expect("entity payload schema should serialize");
    let forms = schema["anyOf"]
        .as_array()
        .expect("entity payload schema should offer the v1 and v2 forms");
    let expected = [
        ("#/$defs/EntityRootV2", &["entityVersion", "objectType"][..]),
        (
            "#/$defs/EntityInterfaceRootV2",
            &["entityVersion", "interface"][..],
        ),
        (
            "#/$defs/EntityExtensionRootV2",
            &["entityVersion", "objectTypeExtension"][..],
        ),
        (
            "#/$defs/PropertyTypeCatalogRootV2",
            &["entityVersion", "propertyTypes"][..],
        ),
        (
            "#/$defs/MarkingCatalogRootV2",
            &["entityVersion", "markings"][..],
        ),
        ("#/$defs/EntityRootV1", &["objectKind", "fields"][..]),
    ];
    assert_eq!(forms.len(), expected.len());
    for (form, (reference, required)) in forms.iter().zip(expected) {
        assert_eq!(form["$ref"], reference);
        let name = reference.trim_start_matches("#/$defs/");
        let definition = &schema["$defs"][name];
        assert_eq!(
            definition["required"],
            serde_json::json!(required),
            "{name}"
        );
        assert_eq!(
            definition["additionalProperties"], false,
            "{name} is closed"
        );
        assert_eq!(
            definition["properties"]["extensions"]["type"], "object",
            "{name} extensions must remain an object map when present"
        );
    }
    assert_eq!(
        schema["$defs"]["EntityRootV2"]["properties"]["entityVersion"]["$ref"],
        "#/$defs/EntityIrVersion"
    );
    assert_eq!(
        schema["$defs"]["EntityRootV2"]["properties"]["objectType"]["$ref"],
        "#/$defs/ObjectTypeDef"
    );
}

/// Policy and `BusinessPack` publish their strict typed roots (T1-IR-2):
/// a `oneOf` with one closed branch per form, with the form's own required
/// set, and the branches are disjoint.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table of expected forms per kind"
)]
fn policy_and_business_pack_schemas_are_their_typed_forms() {
    let cases = [
        (
            ArtifactKind::Policy,
            vec![
                (
                    "ObligationPolicyRoot",
                    &["policyVersion", "policyId", "scopeRequired", "obligations"][..],
                ),
                (
                    "ApprovalPolicyRoot",
                    &[
                        "policyId",
                        "appliesTo",
                        "reviewRequiredWhen",
                        "approvalRoles",
                    ][..],
                ),
                ("AccessIr", &["accessVersion", "policyId", "rules"][..]),
            ],
        ),
        (
            ArtifactKind::BusinessPack,
            vec![
                (
                    "OntologyPackRoot",
                    &[
                        "packId",
                        "packVersion",
                        "packKind",
                        "sources",
                        "terms",
                        "mappings",
                        "shapes",
                    ][..],
                ),
                (
                    "OntologyLinkRoot",
                    &[
                        "packId",
                        "packVersion",
                        "packKind",
                        "fromPack",
                        "toPack",
                        "correspondences",
                    ][..],
                ),
                (
                    "CompositionPackRoot",
                    &["packId", "packVersion", "artifacts"][..],
                ),
            ],
        ),
    ];
    for (kind, expected) in cases {
        let schema = serde_json::to_value(artifact_payload_schema(&kind))
            .expect("payload root schema should serialize");
        assert!(
            schema.get("anyOf").is_none(),
            "{kind:?} forms are exclusive"
        );
        let forms = schema["oneOf"]
            .as_array()
            .expect("payload root schema should offer its forms as oneOf");
        assert_eq!(forms.len(), expected.len(), "{kind:?}");
        for (form, (name, required)) in forms.iter().zip(expected) {
            assert_eq!(form["$ref"], format!("#/$defs/{name}"));
            let definition = &schema["$defs"][name];
            assert_eq!(
                definition["required"],
                serde_json::json!(required),
                "{name}"
            );
            assert_eq!(
                definition["additionalProperties"], false,
                "{name} is closed"
            );
            assert_eq!(
                definition["properties"]["extensions"]["type"], "object",
                "{name} extensions must remain an object map when present"
            );
        }
    }
    // The pack forms are disjoint on `packKind`: the ontology form requires
    // "ontology"; the closed composition form admits only "composition" or no
    // `packKind` at all (legacy IR 0.1 packs).
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::BusinessPack))
        .expect("business pack schema should serialize");
    let defs = &schema["$defs"];
    assert_eq!(
        defs["OntologyPackRoot"]["properties"]["packKind"]["$ref"],
        "#/$defs/OntologyPackKind"
    );
    assert_eq!(
        defs["CompositionPackRoot"]["properties"]["packKind"]["$ref"],
        "#/$defs/CompositionPackKind"
    );
    let composition_required = defs["CompositionPackRoot"]["required"]
        .as_array()
        .expect("composition required set");
    assert!(!composition_required.contains(&serde_json::json!("packKind")));
    assert_eq!(
        defs["OntologyPackKind"]["enum"],
        serde_json::json!(["ontology"])
    );
    assert_eq!(
        defs["CompositionPackKind"]["enum"],
        serde_json::json!(["composition"])
    );

    // The policy forms are disjoint on `policyVersion`: required by the
    // obligations form, not admitted by the closed approval form.
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::Policy))
        .expect("policy schema should serialize");
    assert!(
        schema["$defs"]["ApprovalPolicyRoot"]["properties"]
            .get("policyVersion")
            .is_none()
    );
}

/// The setting schema types `value` by `valueType` (T1-IR-2): one `oneOf`
/// branch per value type.
#[test]
fn setting_schema_types_value_by_value_type() {
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::Setting))
        .expect("setting schema should serialize");
    let branches = schema["oneOf"]
        .as_array()
        .expect("setting schema should branch on valueType");
    // The branches are disjoint: `valueType` is required and each branch fixes
    // a different literal.
    assert!(schema.get("anyOf").is_none());
    assert!(
        schema["required"]
            .as_array()
            .expect("setting required set")
            .contains(&serde_json::json!("valueType"))
    );
    let pairs: Vec<_> = branches
        .iter()
        .map(|branch| {
            (
                branch["properties"]["valueType"]["const"].clone(),
                branch["properties"]["value"].clone(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            (
                serde_json::json!("text"),
                serde_json::json!({ "type": "string" })
            ),
            (
                serde_json::json!("number"),
                serde_json::json!({ "type": "number", "format": "double" })
            ),
            (
                serde_json::json!("boolean"),
                serde_json::json!({ "type": "boolean" })
            ),
            (
                serde_json::json!("json"),
                serde_json::json!({ "$ref": "#/$defs/JsonValue" })
            ),
        ]
    );
}

#[test]
fn envelope_schema_pairs_each_kind_with_its_payload_root() {
    let schema =
        serde_json::to_value(artifact_envelope_schema()).expect("envelope schema should serialize");
    let branches = schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("envelope schema should contain oneOf branches");
    assert_eq!(branches.len(), payload_root_cases().len());

    for (filename, kind) in payload_root_cases() {
        let wire_kind = serde_json::to_value(&kind)
            .expect("artifact kind should serialize")
            .as_str()
            .expect("artifact kind should be a string")
            .to_string();
        let root_name = filename.trim_end_matches(".schema.json");
        let expected_ref = format!("#/$defs/{}Payload", to_pascal_case(root_name));
        assert!(branches.iter().any(|branch| {
            branch["properties"]["kind"]["const"] == wire_kind
                && branch["properties"]["payload"]["$ref"] == expected_ref
        }));
    }
}

fn to_pascal_case(value: &str) -> String {
    value
        .split('_')
        .map(|segment| {
            let mut characters = segment.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            first.to_uppercase().collect::<String>() + characters.as_str()
        })
        .collect()
}

#[test]
fn envelope_kind_selects_its_closed_payload_root() {
    for (_, kind) in payload_root_cases() {
        let payload =
            ArtifactPayload::from_kind_json(&kind, JsonValue::Object(BTreeMap::default()))
                .expect("empty payload is an object");
        match kind {
            ArtifactKind::Entity => assert!(matches!(payload, ArtifactPayload::Entity(_))),
            ArtifactKind::Relation => assert!(matches!(payload, ArtifactPayload::Relation(_))),
            ArtifactKind::Action => assert!(matches!(payload, ArtifactPayload::Action(_))),
            ArtifactKind::Rule => assert!(matches!(payload, ArtifactPayload::Rule(_))),
            ArtifactKind::Connector => assert!(matches!(payload, ArtifactPayload::Connector(_))),
            ArtifactKind::Widget => assert!(matches!(payload, ArtifactPayload::Widget(_))),
            ArtifactKind::Customization => {
                assert!(matches!(payload, ArtifactPayload::Customization(_)));
            }
            ArtifactKind::Setting => assert!(matches!(payload, ArtifactPayload::Setting(_))),
            ArtifactKind::BusinessPack => {
                assert!(matches!(payload, ArtifactPayload::BusinessPack(_)));
            }
            ArtifactKind::Policy => assert!(matches!(payload, ArtifactPayload::Policy(_))),
            ArtifactKind::Derived => assert!(matches!(payload, ArtifactPayload::Derived(_))),
            ArtifactKind::SourceSchema => {
                assert!(matches!(payload, ArtifactPayload::SourceSchema(_)));
            }
            ArtifactKind::Mapping => assert!(matches!(payload, ArtifactPayload::Mapping(_))),
            ArtifactKind::Law => assert!(matches!(payload, ArtifactPayload::Law(_))),
        }
    }
}

#[test]
fn typed_payload_roots_use_legacy_json_value_wire_encoding() {
    for (_, kind) in payload_root_cases() {
        let mut extension_value = BTreeMap::default();
        extension_value.insert(
            "example.com/audit/1.0.0".to_string(),
            JsonValue::Object(BTreeMap::from([(
                "enabled".to_string(),
                JsonValue::Bool(true),
            )])),
        );
        let legacy = JsonValue::Object(BTreeMap::from([(
            "extensions".to_string(),
            JsonValue::Object(extension_value),
        )]));
        let typed = ArtifactPayload::from_kind_json(&kind, legacy.clone())
            .expect("extension payload is an object");

        assert_eq!(typed.kind(), kind, "{kind:?} selected the wrong root");
        assert_eq!(
            serde_json::to_vec(&typed.to_json_value()).expect("typed payload should serialize"),
            serde_json::to_vec(&legacy).expect("legacy JSON value should serialize"),
            "{kind:?} payload changed its legacy JSON wire encoding"
        );

        // Standalone, the closed root set is adjacently tagged exactly like the envelope:
        // `kind` is the tag and `payload` is the legacy root object.
        let tagged = serde_json::to_value(&typed).expect("tagged payload should serialize");
        assert_eq!(
            tagged,
            serde_json::json!({
                "kind": serde_json::to_value(&kind).expect("kind should serialize"),
                "payload": serde_json::to_value(&legacy).expect("legacy should serialize"),
            }),
            "{kind:?} tagged encoding"
        );
        let decoded: ArtifactPayload =
            serde_json::from_value(tagged).expect("tagged payload should deserialize");
        assert_eq!(decoded, typed, "{kind:?} tagged round-trip");
    }
}

#[test]
fn envelope_kind_is_the_payload_tag_and_mismatched_roots_cannot_be_encoded_by_kind() {
    let source = serde_json::json!({
        "irVersion": "0.1",
        "artifactId": "test.relation",
        "kind": "relation",
        "namespace": "test",
        "name": "Relation",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        // `actionId` would have steered the removed shape heuristic to Action; the envelope
        // tag is authoritative and the key is kept as an unknown field for the validator.
        "payload": { "from": "A", "to": "B", "actionId": "x" }
    });
    let envelope: ArtifactEnvelope =
        serde_json::from_value(source.clone()).expect("envelope should deserialize");
    assert_eq!(envelope.kind(), ArtifactKind::Relation);
    assert!(matches!(envelope.payload, ArtifactPayload::Relation(_)));
    // The unknown key is kept for the checker, which reports it as a diagnostic
    // (asserted in norn-checker's typed_payload_roots_compatibility tests).
    assert!(
        envelope.payload.unknown_fields().contains("actionId"),
        "unknown payload field stays a diagnostic"
    );

    let mut missing_kind = source.clone();
    missing_kind
        .as_object_mut()
        .expect("fixture is an object")
        .remove("kind");
    assert!(serde_json::from_value::<ArtifactEnvelope>(missing_kind).is_err());

    let mut unknown_kind = source;
    unknown_kind["kind"] = serde_json::json!("process");
    assert!(
        serde_json::from_value::<ArtifactEnvelope>(unknown_kind).is_err(),
        "retired kinds such as process are not payload roots"
    );
}

fn normalize_fixture_json(source: &str) -> String {
    let value: JsonValue =
        serde_json::from_str(source).expect("fixture JSON should decode into canonical JSON");
    serde_json::to_string(&value).expect("canonical fixture JSON should encode")
}

fn assert_fixture_roundtrip(name: &str, source: &str) {
    let envelope: ArtifactEnvelope = serde_json::from_str(source)
        .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
    let reserialized = serde_json::to_string(&envelope)
        .unwrap_or_else(|error| panic!("{name} should serialize: {error}"));

    assert_eq!(
        normalize_fixture_json(&reserialized),
        normalize_fixture_json(source),
        "{name} changed after ArtifactEnvelope wire round-trip"
    );
}

#[test]
fn existing_artifact_fixtures_roundtrip_with_identical_canonical_json() {
    let fixtures = [
        (
            "fixtures/ir/retail-inventory.connector.json",
            include_str!("../../../fixtures/ir/retail-inventory.connector.json"),
        ),
        (
            "fixtures/ir/retail-replenishment.widget.json",
            include_str!("../../../fixtures/ir/retail-replenishment.widget.json"),
        ),
        (
            "fixtures/ir/retail-sku.entity.json",
            include_str!("../../../fixtures/ir/retail-sku.entity.json"),
        ),
        (
            "verification/ontology/packs/company-commerce-v0/pack.json",
            include_str!("../../../verification/ontology/packs/company-commerce-v0/pack.json"),
        ),
        (
            "verification/ontology/packs/quantity-units-v0/pack.json",
            include_str!("../../../verification/ontology/packs/quantity-units-v0/pack.json"),
        ),
    ];

    for (name, source) in fixtures {
        assert_fixture_roundtrip(name, source);
    }

    let conformance: serde_json::Value = serde_json::from_str(include_str!(
        "../../../verification/archive/rules-dmn-v1/fixtures/conformance.json"
    ))
    .expect("rule conformance fixture should decode");
    let rule_artifact = serde_json::to_string(
        conformance
            .get("artifact")
            .expect("rule conformance fixture should contain an artifact"),
    )
    .expect("nested rule artifact should encode");
    assert_fixture_roundtrip(
        "verification/archive/rules-dmn-v1/fixtures/conformance.json#artifact",
        &rule_artifact,
    );
}
