//! Unit tests for artifact-envelope and graph-load validation.

#[cfg(test)]
mod suite {
    use std::collections::BTreeMap;

    use super::super::action::validate_closed_action_schema;
    use super::super::*;
    #[allow(
        clippy::wildcard_imports,
        reason = "unit test module covering all IR artifact types"
    )]
    use norn_ir::*;
    use serde_json::json;

    /// An `ActionIR` input's nullable scalar leaf carries what its scalar leaf
    /// carries (a string `enum`, a decimal `nornType`, display labels) beside
    /// null; anything else beside a type array stays refused.
    #[test]
    fn nullable_action_input_leaves_carry_their_scalar_keywords() {
        let check = |leaf: serde_json::Value| {
            let input = json!({
                "type": "object", "required": ["value"], "additionalProperties": false,
                "properties": { "value": leaf },
            });
            let tree = norn_ir::json_tree_from_value(input).expect("schema tree");
            let mut diagnostics = Vec::new();
            validate_closed_action_schema(
                Some(&tree),
                "$.inputSchema",
                0,
                true,
                &mut diagnostics,
                Some("app.x.action"),
            );
            diagnostics
                .into_iter()
                .map(|d| d.message)
                .collect::<Vec<_>>()
        };
        for admitted in [
            json!({ "type": ["string", "null"], "enum": ["pass", "fail"], "title": "Status" }),
            json!({ "type": ["number", "null"], "nornType": { "kind": "decimal", "scale": 18 } }),
            json!({ "type": ["integer", "null"], "description": "count" }),
            json!({ "type": ["string", "null"] }),
            json!({ "type": ["null", "boolean", "number", "string", "array", "object"] }),
        ] {
            assert_eq!(check(admitted.clone()), Vec::<String>::new(), "{admitted}");
        }
        for refused in [
            json!({ "type": ["string", "null"], "minimum": 0 }),
            json!({ "type": ["number", "null"], "enum": ["a"] }),
            json!({ "type": ["number", "null"], "nornType": { "kind": "decimal", "scale": 19 } }),
            json!({ "type": ["null", "boolean", "number", "string", "array", "object"], "title": "x" }),
            json!({ "type": ["string", "integer"] }),
        ] {
            assert_ne!(check(refused.clone()).len(), 0, "{refused}");
        }
    }

    fn provenance() -> ArtifactProvenance {
        ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        }
    }

    fn entity_payload() -> JsonValue {
        // Minimal valid entity payload: `fields` is required and must be an object.
        let mut fields_map: BTreeMap<String, JsonValue> = BTreeMap::new();
        let mut field_def: BTreeMap<String, JsonValue> = BTreeMap::new();
        field_def.insert("type".to_string(), JsonValue::String("text".to_string()));
        field_def.insert("required".to_string(), JsonValue::Bool(true));
        fields_map.insert("id".to_string(), JsonValue::Object(field_def));
        let mut payload: BTreeMap<String, JsonValue> = BTreeMap::new();
        payload.insert(
            "objectKind".to_string(),
            JsonValue::String("commerce.Product".to_string()),
        );
        payload.insert("fields".to_string(), JsonValue::Object(fields_map));
        JsonValue::Object(payload)
    }

    fn entity_artifact() -> ArtifactEnvelope {
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(&ArtifactKind::Entity, entity_payload())
                .expect("entity payload is an object"),
        }
    }

    #[test]
    fn rejects_unknown_top_level_payload_fields_with_a_stable_diagnostic() {
        let mut source =
            serde_json::to_value(entity_artifact()).expect("entity artifact should serialize");
        source["payload"]["unknownTopLevel"] = serde_json::json!(true);
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("unknown field should reach validation");

        let diagnostics = artifact
            .validate()
            .expect_err("unknown payload fields must fail closed");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "entity.validation.invalid_payload"
                && diagnostic.message == "unknown field 'unknownTopLevel' is not allowed"
                && diagnostic.path.as_deref() == Some("payload.unknownTopLevel")
        }));
    }

    #[test]
    fn rejects_non_object_extensions_with_a_validation_diagnostic() {
        let mut source =
            serde_json::to_value(entity_artifact()).expect("entity artifact should serialize");
        source["payload"]["extensions"] = serde_json::json!("not-an-object-map");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("invalid extensions should reach validation");

        let diagnostics = artifact
            .validate()
            .expect_err("non-object extensions must fail validation");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "entity.validation.invalid_payload"
                && diagnostic.message == "extensions must be an object map"
                && diagnostic.path.as_deref() == Some("payload.extensions")
        }));
    }

    #[test]
    fn rejects_dependency_without_explicit_version() {
        let mut artifact = entity_artifact();
        artifact.dependencies = vec![ArtifactDependency {
            artifact_id: "commerce.entity.category".to_string(),
            kind: ArtifactKind::Entity,
            version: None,
            required: true,
        }];

        let diagnostics = artifact
            .validate()
            .expect_err("versionless dependency should fail");

        assert_eq!(diagnostics[0].code, "ir.validation.invalid_dependency");
        assert_eq!(
            diagnostics[0].remediation,
            Some(DiagnosticRemediation::ResolveDependency)
        );
    }

    #[test]
    fn rejects_self_dependency() {
        let mut artifact = entity_artifact();
        artifact.dependencies = vec![ArtifactDependency {
            artifact_id: "commerce.entity.product".to_string(),
            kind: ArtifactKind::Entity,
            version: Some("0.1.0".to_string()),
            required: true,
        }];

        let diagnostics = artifact
            .validate()
            .expect_err("self dependency should fail");

        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ir.validation.invalid_dependency")
        );
    }

    #[test]
    fn validates_typed_connector_command_and_keyset_profiles() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "delta",
                    "resourceId": "sales_orders",
                    "endpoint": "/api/resource/{doctype}",
                    "sinceParam": "modified",
                    "updatedAtPath": "$.modified",
                    "itemsPath": "$.data",
                    "queryParams": { "fields": "[\"name\"]" },
                    "keyset": {
                        "filtersParam": "filters",
                        "orFilterParam": "or_filters",
                        "fieldsParam": "fields",
                        "orderParam": "order_by",
                        "pageParam": "limit_start",
                        "sizeParam": "limit_page_length",
                        "watermarkField": "modified",
                        "tieBreakerField": "name",
                        "fields": ["name", "modified"],
                        "pageSize": 100
                    }
                }],
                "mappings": [],
                "commands": [{
                    "actionId": "cancel_sales_order",
                    "operation": "cancel",
                    "doctypes": ["Sales Order"],
                    "endpoint": "/api/resource/{doctype}/{name}",
                    "method": "PUT",
                    "bodyFields": ["reason"],
                    "fixedBody": { "docstatus": 2 },
                    "idempotency": {
                        "keyField": "norn_idempotency_key",
                        "headerName": "X-Norn-Idempotency-Key",
                        "receipt": {
                            "doctype": "Norn Command Receipt",
                            "tableField": "norn_command_receipts",
                            "keyField": "idempotency_key",
                            "operationField": "operation",
                            "parentField": "parent",
                            "parentTypeField": "parenttype",
                            "parentTableField": "parentfield",
                            "timestampField": "at"
                        },
                        "createLookup": {
                            "endpoint": "/api/resource/{doctype}",
                            "itemsPath": "$.data",
                            "fields": ["name", "modified"],
                            "filtersParam": "filters",
                            "fieldsParam": "fields",
                            "limitParam": "limit_page_length",
                            "maxResults": 2,
                            "filters": [{ "field": "norn_idempotency_key", "value": "idempotency_key" }]
                        },
                        "receiptLookup": {
                            "endpoint": "/api/resource/{doctype}",
                            "itemsPath": "$.data",
                            "fields": ["name", "modified"],
                            "filtersParam": "filters",
                            "fieldsParam": "fields",
                            "limitParam": "limit_page_length",
                            "maxResults": 2,
                            "filters": [{ "entity": "Norn Command Receipt", "field": "parenttype", "value": "parent_type" }]
                        }
                    },
                    "lifecycle": {
                        "documentPath": "$.data",
                        "identityField": "name",
                        "versionField": "modified",
                        "statusField": "docstatus",
                        "initialStatus": 0,
                        "submittedStatus": 1,
                        "cancelledStatus": 2
                    },
                    "conflicts": [{ "status": 409, "errorType": "TimestampMismatchError", "category": "version" }]
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_whitespace_padded_poll_fanout_bindings() {
        let canonical =
            include_str!("../../../../fixtures/connectors/google-directory.connector.json");
        let source: serde_json::Value =
            serde_json::from_str(canonical).expect("Google connector artifact");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("connector envelope");
        artifact
            .validate()
            .expect("canonical poll profile is admitted");

        let mut invalid = source;
        invalid["payload"]["extensions"]["norn.poll/1"]["feeds"][3]["fanout"]["pathBinding"] =
            json!(" groupId ");
        let artifact: ArtifactEnvelope =
            serde_json::from_value(invalid).expect("mutated connector envelope");
        let diagnostics = artifact
            .validate()
            .expect_err("binding whitespace changes the declared endpoint key");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "connector.validation.invalid_payload"
                && diagnostic.path.as_deref()
                    == Some("payload.extensions.norn.poll/1.feeds[3].fanout")
        }));
    }

    #[test]
    fn poll_fanout_accepts_declared_query_binding_and_refuses_unbound_name() {
        let canonical =
            include_str!("../../../../fixtures/connectors/google-directory.connector.json");
        let mut source: serde_json::Value =
            serde_json::from_str(canonical).expect("connector fixture");
        let resources = source["payload"]["resources"]
            .as_array_mut()
            .expect("resources");
        let resource = resources
            .iter_mut()
            .find(|resource| {
                resource["endpoint"]
                    .as_str()
                    .is_some_and(|endpoint| endpoint.contains("{groupId}"))
            })
            .expect("child resource");
        resource["endpoint"] = json!("/members");
        resource["queryParams"] = json!({"group": "{groupId}"});
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("query connector");
        artifact.validate().expect("query-bound fanout admitted");
        let resources = source["payload"]["resources"]
            .as_array_mut()
            .expect("resources");
        let resource = resources
            .iter_mut()
            .find(|resource| resource["endpoint"] == "/members")
            .expect("child");
        resource["queryParams"] = json!({"group": "literal"});
        let artifact: ArtifactEnvelope = serde_json::from_value(source).expect("unbound connector");
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn poll_connector_paths_accept_canonical_erpnext_and_google_packs() {
        let fixtures = [
            (
                "ERPNext",
                include_str!("../../../../fixtures/connectors/frappe.connector.json"),
            ),
            (
                "Google Directory",
                include_str!("../../../../fixtures/connectors/google-directory.connector.json"),
            ),
        ];

        for (name, source) in fixtures {
            let value: serde_json::Value = serde_json::from_str(source).expect("provider artifact");
            let artifact: ArtifactEnvelope =
                serde_json::from_value(value).expect("provider connector envelope");
            assert!(artifact.validate().is_ok(), "{name} paths remain admitted");
        }
    }

    #[test]
    fn poll_connector_paths_report_each_malformed_key_at_its_field() {
        let canonical =
            include_str!("../../../../fixtures/connectors/google-directory.connector.json");
        let source: serde_json::Value = serde_json::from_str(canonical).expect("Google artifact");
        let mut malformed_paths = vec![
            String::new(),
            " /id".to_owned(),
            "/id ".to_owned(),
            "a..b".to_owned(),
            "a[no]".to_owned(),
            "a[1".to_owned(),
            "a[1][2]".to_owned(),
            "a[1]junk".to_owned(),
            "a[999999999999999999999999999999999999999]".to_owned(),
            "/id~2".to_owned(),
            "/id\u{1}".to_owned(),
        ];
        malformed_paths.push(format!("/{}", "a".repeat(1_024)));

        for path in malformed_paths {
            let mut invalid_record_key = source.clone();
            invalid_record_key["payload"]["extensions"]["norn.poll/1"]["feeds"][1]["recordKeyPath"] =
                json!(path);
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid_record_key).expect("record key fixture");
            let diagnostics = artifact
                .validate()
                .expect_err("malformed recordKeyPath must be rejected");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.message.contains("recordKeyPath is invalid:")
                    && diagnostic.path.as_deref()
                        == Some("payload.extensions.norn.poll/1.feeds[1].recordKeyPath")
            }));

            let mut invalid_fanout_key = source.clone();
            invalid_fanout_key["payload"]["extensions"]["norn.poll/1"]["feeds"][3]["fanout"]["keyPath"] =
                json!(path);
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid_fanout_key).expect("fanout key fixture");
            let diagnostics = artifact
                .validate()
                .expect_err("malformed fanout keyPath must be rejected");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.message.contains("fanout keyPath is invalid:")
                    && diagnostic.path.as_deref()
                        == Some("payload.extensions.norn.poll/1.feeds[3].fanout.keyPath")
            }));
        }
    }

    #[test]
    fn rejects_duplicate_poll_resource_ids() {
        let canonical =
            include_str!("../../../../fixtures/connectors/google-directory.connector.json");
        let mut source: serde_json::Value =
            serde_json::from_str(canonical).expect("Google connector artifact");
        let duplicate = source["payload"]["resources"][0].clone();
        source["payload"]["resources"]
            .as_array_mut()
            .expect("resource array")
            .push(duplicate);
        let artifact: ArtifactEnvelope =
            serde_json::from_value(source).expect("mutated connector envelope");
        let diagnostics = artifact
            .validate()
            .expect_err("resource ids must be unique");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "connector.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.resources[4].resourceId")
        }));
    }

    #[test]
    fn rejects_idempotency_lookups_that_cannot_detect_duplicate_matches() {
        let canonical = include_str!("../../../../fixtures/connectors/frappe.connector.json");
        let source: serde_json::Value =
            serde_json::from_str(canonical).expect("canonical ERPNext connector artifact");
        let admitted: ArtifactEnvelope =
            serde_json::from_value(source.clone()).expect("canonical connector envelope");
        admitted
            .validate()
            .expect("canonical connector lookup limits are admitted");

        for lookup_name in ["createLookup", "receiptLookup"] {
            let mut invalid = source.clone();
            invalid["payload"]["commands"][0]["idempotency"][lookup_name]["maxResults"] = json!(1);
            let artifact: ArtifactEnvelope =
                serde_json::from_value(invalid).expect("mutated connector artifact");

            let expected_path = format!("payload.commands[0].idempotency.{lookup_name}.maxResults");
            let diagnostics = artifact
                .validate()
                .expect_err("one result cannot establish lookup uniqueness");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some(expected_path.as_str())
                    && diagnostic.message.contains("from 2 through")
            }));
        }
    }

    #[test]
    fn validates_optional_connector_command_key_header_with_resource_path() {
        let payload = |command_key_header: &serde_json::Value| {
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "records",
                    "delimiter": ",",
                    "columns": ["name"],
                    "commandKeyHeader": command_key_header
                }],
                "mappings": []
            })
        };
        assert!(
            artifact_with_payload(
                &ArtifactKind::Connector,
                payload(&json!("X-Norn-Command-Key")),
            )
            .validate()
            .is_ok()
        );

        for invalid in [json!(" "), json!(42), json!(null)] {
            let diagnostics = artifact_with_payload(&ArtifactKind::Connector, payload(&invalid))
                .validate()
                .expect_err("command key header must be a non-empty string");
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "connector.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some("payload.resources[0].commandKeyHeader")
            }));
        }
    }

    #[test]
    fn rejects_connector_body_fields_that_override_reserved_contract_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "records",
                    "delimiter": ",",
                    "columns": ["name"]
                }],
                "mappings": [],
                "commands": [{
                    "operation": "update",
                    "endpoint": "/api/resource/{doctype}/{name}",
                    "method": "PUT",
                    "bodyFields": ["modified"],
                    "idempotency": {
                        "keyField": "norn_idempotency_key",
                        "receipt": {
                            "doctype": "Receipt",
                            "tableField": "receipts",
                            "keyField": "key",
                            "operationField": "operation",
                            "parentField": "parent",
                            "parentTypeField": "parenttype",
                            "parentTableField": "parentfield"
                        },
                        "createLookup": {
                            "endpoint": "/lookup/{doctype}", "itemsPath": "$.data", "fields": ["name"],
                            "filtersParam": "filters", "fieldsParam": "fields", "limitParam": "limit", "maxResults": 2,
                            "filters": [{ "field": "key", "value": "idempotency_key" }]
                        },
                        "receiptLookup": {
                            "endpoint": "/lookup/{doctype}", "itemsPath": "$.data", "fields": ["name"],
                            "filtersParam": "filters", "fieldsParam": "fields", "limitParam": "limit", "maxResults": 2,
                            "filters": [{ "field": "key", "value": "idempotency_key" }]
                        }
                    },
                    "lifecycle": {
                        "documentPath": "$.data", "identityField": "name", "versionField": "modified",
                        "statusField": "docstatus", "initialStatus": 0, "submittedStatus": 1, "cancelledStatus": 2
                    }
                }]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("reserved body field should fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some("payload.commands[0].bodyFields[0]")
        }));
    }

    #[test]
    fn validates_connector_custom_step_hook_schemas() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "inventory",
                    "delimiter": ",",
                    "columns": ["sku"]
                }],
                "mappings": [],
                "customStepHooks": [{
                    "hookId": "retail.connector.inventory.fetch_page",
                    "installId": "install_inventory_fetch_page",
                    "extensionVersion": "0.1.0",
                    "requiredServices": ["provider_client"],
                    "scopeCapabilities": ["connector.run"],
                    "inputSchema": { "type": "object" },
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" }
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_connector_custom_step_hook_without_schema_objects() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Connector,
            json!({
                "connectorVersion": "0.1",
                "auth": { "kind": "none" },
                "resources": [{
                    "kind": "csv_import",
                    "resourceId": "inventory",
                    "delimiter": ",",
                    "columns": ["sku"]
                }],
                "mappings": [],
                "customStepHooks": [{
                    "hookId": "retail.connector.inventory.fetch_page",
                    "installId": "install_inventory_fetch_page",
                    "extensionVersion": "0.1.0",
                    "requiredServices": ["provider_client"],
                    "scopeCapabilities": ["connector.run"],
                    "inputSchema": "object",
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" }
                }]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("custom step schema should fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == Some("payload.customStepHooks[0].inputSchema".to_string())
        }));
    }

    #[test]
    fn validates_customization_field_overlay_payload() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.loyalty_tier",
                "intent": {
                    "summary": "Add loyalty tier tracking to customers",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "field",
                    "operations": [{
                        "op": "add_field",
                        "field": "loyaltyTier",
                        "definition": {
                            "type": "text",
                            "required": false,
                            "validation": { "enum": ["bronze", "silver", "gold"] }
                        }
                    }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_customization_custom_hook_without_executable_boundary() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.live_validation",
                "intent": {
                    "summary": "Validate customer credit state before approval",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "validation",
                    "operations": [{ "op": "custom_hook" }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"]
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("custom hook must declare executable contract");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "customization.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.executable")
        }));
    }

    #[test]
    fn rejects_customization_executable_with_ambient_database_access() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Customization,
            json!({
                "customizationVersion": "0.1",
                "customizationId": "crm.customer.live_validation",
                "intent": {
                    "summary": "Validate customer credit state before approval",
                    "requestedBy": "agent"
                },
                "targetArtifact": {
                    "artifactId": "crm.entity.customer",
                    "kind": "entity",
                    "version": "0.1.0"
                },
                "overlay": {
                    "kind": "validation",
                    "operations": [{ "op": "custom_hook" }]
                },
                "approval": { "state": "proposed", "required": true },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "executable": {
                    "boundary": "capability_rpc",
                    "inputSchema": { "type": "object" },
                    "outputSchema": { "type": "object" },
                    "errorSchema": { "type": "object" },
                    "requiredServices": ["customer_state"],
                    "scopeCapabilities": ["crm.customer.read"],
                    "resourceLimits": {
                        "maxWallTimeMs": 1000,
                        "maxMemoryMb": 128,
                        "maxNetworkCalls": 2,
                        "maxStorageBytes": 0
                    },
                    "directDatabaseAccess": true
                }
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("direct DB access must fail closed");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "customization.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.executable.directDatabaseAccess")
        }));
    }

    #[test]
    fn validates_action_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Action,
            json!({
                "actionId": "retail.replenishment.approve",
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_action_payload_missing_versioned_contract_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Action,
            json!({
                "actionId": "retail.replenishment.approve",
                "ledgered": true
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("ActionIR contract fields should be required");
        for path in ["payload.scopeRequired", "payload.idempotent"] {
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "action.validation.invalid_payload"
                        && diagnostic.path.as_deref() == Some(path)
                }),
                "expected action.validation.invalid_payload at {path}, got: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn validates_rule_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleId": "retail.invariant.stock_nonnegative",
                "appliesTo": "retail.StockPosition",
                "expression": "quantity >= 0",
                "severity": "error"
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn validates_modern_rule_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleVersion": "0.1",
                "ruleId": "retail.rule.replenishment_review",
                "inputSchema": { "type": "object", "required": ["projectedValue"] },
                "outputSchema": { "type": "object", "required": ["reviewRequired"] },
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "decision": {
                    "kind": "decision_table",
                    "hitPolicy": "first",
                    "inputs": [{ "field": "projectedValue", "type": "number" }],
                    "rows": [
                        {
                            "rowId": "high_value",
                            "when": [{ "field": "projectedValue", "op": "gte", "value": 1000 }],
                            "output": { "reviewRequired": true, "reason": "high_value" }
                        },
                        {
                            "rowId": "low_value",
                            "when": [],
                            "output": { "reviewRequired": false, "reason": "low_value" }
                        }
                    ]
                }
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_modern_rule_payload_with_required_services() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleVersion": "0.1",
                "ruleId": "retail.rule.live_credit_check",
                "inputSchema": { "type": "object" },
                "outputSchema": { "type": "object" },
                "scopeRequired": ["orgId"],
                "requiredServices": ["credit_provider"],
                "decision": { "kind": "literal", "value": { "allowed": true } }
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("deterministic RuleIR must reject live services");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "rule.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.requiredServices")
        }));
    }

    #[test]
    fn rejects_rule_payload_missing_invariant_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Rule,
            json!({
                "ruleId": "retail.invariant.stock_nonnegative",
                "severity": "fatal"
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("RuleIR contract fields should be required");
        for path in [
            "payload.appliesTo",
            "payload.expression",
            "payload.severity",
        ] {
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "rule.validation.invalid_payload"
                        && diagnostic.path.as_deref() == Some(path)
                }),
                "expected rule.validation.invalid_payload at {path}, got: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn validates_modern_policy_payload_shape() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Policy,
            json!({
                "policyVersion": "0.1",
                "policyId": "retail.policy.replenishment_review",
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "obligations": [{
                    "obligationId": "high_value_review",
                    "appliesTo": "retail.ReplenishmentProposal",
                    "ruleId": "retail.rule.replenishment_review",
                    "evidenceRequired": ["proposal", "policy_trace"],
                    "reviewRoles": ["inventory_manager"]
                }]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_policy_payload_without_obligations() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Policy,
            json!({
                "policyVersion": "0.1",
                "policyId": "retail.policy.replenishment_review",
                "scopeRequired": ["orgId"],
                "obligations": []
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("PolicyIR must require obligations");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "policy.validation.invalid_payload"
                && diagnostic.path.as_deref() == Some("payload.obligations")
        }));
    }

    #[test]
    fn accepts_legacy_ir_v01_composition_pack_without_discriminator() {
        let artifact = artifact_with_payload(
            &ArtifactKind::BusinessPack,
            json!({
                "packId": "retail.pack.legacy",
                "packVersion": "0.1",
                "artifacts": [
                    { "artifactId": "retail.StockPosition.entity", "kind": "entity", "version": "0.1.0" }
                ]
            }),
        );

        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_business_pack_payload_without_artifacts() {
        let artifact = artifact_with_payload(
            &ArtifactKind::BusinessPack,
            json!({
                "packId": "retail.pack.core",
                "packVersion": "0.1",
                "packKind": "composition",
                "artifacts": []
            }),
        );

        let diagnostics = artifact
            .validate()
            .expect_err("BusinessPackIR contract fields should be required");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "business_pack.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some("payload.artifacts")
            }),
            "expected payload.artifacts, got: {diagnostics:?}"
        );
    }

    #[test]
    fn plans_bounded_graph_load_requests() {
        let request = GraphLoadRequest {
            request_id: "graph_req_1".to_string(),
            correlation_id: "corr_1".to_string(),
            scope: ScopeRef {
                org_id: Some("org_demo".to_string()),
                environment_id: Some("dev".to_string()),
                ..ScopeRef::default()
            },
            scope_required: vec!["orgId".to_string(), "environmentId".to_string()],
            shape: GraphReadShape {
                root_object_kind: "customer".to_string(),
                root_ids: vec!["customer_1".to_string(), "customer_2".to_string()],
                fields: vec!["name".to_string(), "email".to_string()],
                relations: vec![GraphRelationShape {
                    relation_kind: "customer_has_order".to_string(),
                    direction: GraphTraversalDirection::Outbound,
                    target_object_kind: "order".to_string(),
                    fields: vec!["orderNumber".to_string(), "total".to_string()],
                    include_relation_metadata: Some(true),
                    limit: 20,
                    relations: vec![GraphRelationShape {
                        relation_kind: "order_has_line_item".to_string(),
                        direction: GraphTraversalDirection::Outbound,
                        target_object_kind: "line_item".to_string(),
                        fields: vec!["sku".to_string(), "quantity".to_string()],
                        include_relation_metadata: None,
                        limit: 50,
                        relations: Vec::new(),
                    }],
                }],
            },
            budget: GraphQueryBudget {
                max_depth: 3,
                max_root_objects: 100,
                max_relation_shapes: 20,
                max_relation_batches: 20,
                max_rows: 10_000,
                allow_higher_risk_policy: None,
            },
        };

        let plan = plan_graph_load(&request);

        assert!(plan.can_execute);
        assert_eq!(
            plan.root_identity_keys,
            vec!["customer:customer_1", "customer:customer_2"]
        );
        assert_eq!(plan.evidence.depth, 2);
        assert_eq!(plan.evidence.relation_batch_count, 2);
        assert!(plan.relation_batches[0].include_relation_metadata);
    }

    #[test]
    fn reports_graph_load_scope_and_budget_diagnostics() {
        let request = GraphLoadRequest {
            request_id: "graph_req_2".to_string(),
            correlation_id: "corr_2".to_string(),
            scope: ScopeRef {
                environment_id: Some("dev".to_string()),
                ..ScopeRef::default()
            },
            scope_required: vec!["orgId".to_string(), "environmentId".to_string()],
            shape: GraphReadShape {
                root_object_kind: "customer".to_string(),
                root_ids: vec![
                    "customer_1".to_string(),
                    "customer_2".to_string(),
                    "customer_3".to_string(),
                ],
                fields: vec!["name".to_string()],
                relations: vec![GraphRelationShape {
                    relation_kind: "customer_has_order".to_string(),
                    direction: GraphTraversalDirection::Outbound,
                    target_object_kind: "order".to_string(),
                    fields: vec!["orderNumber".to_string()],
                    include_relation_metadata: None,
                    limit: 1000,
                    relations: Vec::new(),
                }],
            },
            budget: GraphQueryBudget {
                max_depth: 1,
                max_root_objects: 2,
                max_relation_shapes: 1,
                max_relation_batches: 1,
                max_rows: 100,
                allow_higher_risk_policy: None,
            },
        };

        let plan = plan_graph_load(&request);
        let codes = plan
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>();

        assert!(!plan.can_execute);
        assert!(codes.contains(&"graph_load.validation.missing_scope"));
        assert!(codes.contains(&"graph_load.validation.over_budget"));
    }

    fn artifact_with_payload(kind: &ArtifactKind, payload: serde_json::Value) -> ArtifactEnvelope {
        let payload: JsonValue =
            norn_ir::json_tree_from_value(payload).expect("payload should decode to JsonValue");
        let payload =
            ArtifactPayload::from_kind_json(kind, payload).expect("test payload is an object");
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.artifact.demo".to_string(),
            namespace: "commerce".to_string(),
            name: "Demo".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload,
        }
    }

    // -----------------------------------------------------------------------
    // Entity / Relation payload validation (closing lib.rs:487 hole)
    // -----------------------------------------------------------------------

    #[test]
    fn validates_entity_with_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.Product",
                "fields": { "sku": { "type": "text", "required": true } }
            }),
        );
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_entity_without_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({ "objectKind": "commerce.Product" }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("entity without fields must fail");
        assert!(
            diagnostics.iter().any(|d| {
                d.code == "entity.validation.invalid_payload"
                    && d.path.as_deref() == Some("payload.fields")
            }),
            "expected entity.validation.invalid_payload at payload.fields, got: {diagnostics:?}"
        );
    }

    #[test]
    fn rejects_entity_with_non_object_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({ "objectKind": "commerce.Product", "fields": "not-an-object" }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("entity with non-object fields must fail");
        assert!(
            diagnostics.iter().any(|d| {
                d.code == "entity.validation.invalid_payload"
                    && d.path.as_deref() == Some("payload.fields")
            }),
            "expected entity.validation.invalid_payload at payload.fields, got: {diagnostics:?}"
        );
    }

    #[test]
    fn entity_and_relation_ir_reject_unimplemented_semantics_before_promotion() {
        let entity = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.User",
                "fields": {
                    "role": { "type": "string", "required": true, "enum": ["viewer"] }
                },
                "computedBy": "ambient_callback"
            }),
        );
        let entity_diagnostics = entity
            .validate()
            .expect_err("ignored EntityIR semantics must fail closed");
        let entity_paths = entity_diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.path.as_deref())
            .collect::<Vec<_>>();
        assert!(entity_paths.contains(&"payload.computedBy"));
        assert!(entity_paths.contains(&"payload.fields.role.enum"));

        let relation = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.User",
                "to": "commerce.Team",
                "cardinality": "many_to_many",
                "through": "commerce.Membership"
            }),
        );
        let relation_diagnostics = relation
            .validate()
            .expect_err("unimplemented RelationIR through semantics must fail closed");
        assert!(
            relation_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path.as_deref() == Some("payload.through"))
        );
    }

    #[test]
    fn entity_ir_accepts_only_closed_string_enum_semantics() {
        let valid = artifact_with_payload(
            &ArtifactKind::Entity,
            json!({
                "objectKind": "commerce.Order",
                "fields": {
                    "status": {
                        "type": "enum",
                        "required": true,
                        "values": ["draft", "submitted"]
                    }
                }
            }),
        );
        valid
            .validate()
            .expect("closed string enum semantics should validate");

        for values in [json!([]), json!(["draft", "draft"]), json!(["draft", 3])] {
            let invalid = artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "commerce.Order",
                    "fields": { "status": { "type": "enum", "values": values } }
                }),
            );
            invalid
                .validate()
                .expect_err("empty, duplicate, and non-string enum values must reject");
        }
    }

    #[test]
    fn entity_ir_rejects_noncanonical_semantic_object_kinds() {
        for object_kind in [
            " inventory.StockPosition",
            "inventory.Stock Position",
            "inventory.\tStockPosition",
            "inventory.*",
            "inventory.\0StockPosition",
            "inventory.\u{a0}StockPosition",
        ] {
            let invalid = artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": object_kind,
                    "fields": {}
                }),
            );
            let diagnostics = invalid
                .validate()
                .expect_err("noncanonical semantic identities must reject");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| { diagnostic.path.as_deref() == Some("payload.objectKind") })
            );
        }
    }

    #[test]
    fn entity_ir_accepts_only_closed_mutation_authority_descriptors() {
        for authority in [
            json!({ "kind": "generic_action" }),
            json!({ "kind": "domain_engine", "domain": "inventory" }),
            json!({ "kind": "domain_engine", "domain": "accounting" }),
        ] {
            artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "inventory.StockPosition",
                    "fields": {},
                    "mutationAuthority": authority
                }),
            )
            .validate()
            .expect("closed mutation authority should validate");
        }

        for authority in [
            json!({ "kind": "domain_engine" }),
            json!({ "kind": "domain_engine", "domain": "unknown" }),
            json!({ "kind": "generic_action", "domain": "inventory" }),
            json!({ "kind": "domain_engine", "domain": "inventory", "callback": "ambient" }),
        ] {
            artifact_with_payload(
                &ArtifactKind::Entity,
                json!({
                    "objectKind": "inventory.StockPosition",
                    "fields": {},
                    "mutationAuthority": authority
                }),
            )
            .validate()
            .expect_err("open, incomplete, or unknown mutation authority must reject");
        }
    }

    #[test]
    fn validates_relation_with_required_fields() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.Customer",
                "to": "commerce.Invoice",
                "cardinality": "one_to_many"
            }),
        );
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn rejects_relation_with_invalid_cardinality() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "from": "commerce.Customer",
                "to": "commerce.Invoice",
                "cardinality": "sometimes"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("relation with invalid cardinality must fail");
        assert!(
            diagnostics.iter().any(|d| {
                d.code == "relation.validation.invalid_payload"
                    && d.path.as_deref() == Some("payload.cardinality")
            }),
            "expected payload.cardinality, got: {diagnostics:?}"
        );
    }

    #[test]
    fn rejects_relation_without_from() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Relation,
            json!({
                "to": "commerce.Invoice"
            }),
        );
        let diagnostics = artifact
            .validate()
            .expect_err("relation without from must fail");
        assert!(
            diagnostics.iter().any(|d| {
                d.code == "relation.validation.invalid_payload"
                    && d.path.as_deref() == Some("payload.from")
            }),
            "expected payload.from, got: {diagnostics:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Derived / ViewIR v0 tests
    // -----------------------------------------------------------------------

    fn valid_derived_payload() -> serde_json::Value {
        json!({
            "viewVersion": "0.1",
            "viewId": "commerce.view.open_invoices",
            "inputs": [
                {
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }
            ],
            "calc": { "kind": "field_projection", "fields": ["id"] },
            "outputSchema": { "type": "array" },
            "maintenance": "recompute_on_read"
        })
    }

    #[test]
    fn validates_minimal_derived_view() {
        let artifact = artifact_with_payload(&ArtifactKind::Derived, valid_derived_payload());
        assert!(
            artifact.validate().is_ok(),
            "minimal derived view should be valid: {:?}",
            artifact.validate()
        );
    }

    /// Each case breaks one member of `valid_derived_payload()` and must be
    /// refused under its code at that member's path, so a refusal for some
    /// other reason cannot pass.
    #[test]
    fn rejects_derived_defects_at_their_paths() {
        const INVALID: &str = "derived.validation.invalid_payload";
        const IO_IN_VIEW: &str = "derived.validation.io_in_view";
        for (label, member, value, code, path) in [
            (
                "empty inputs",
                "inputs",
                json!([]),
                INVALID,
                "payload.inputs",
            ),
            (
                // Only entity / relation / derived are valid input kinds.
                "action input kind",
                "inputs",
                json!([{
                    "artifactId": "commerce.action.something",
                    "kind": "action",
                    "version": "0.1.0",
                    "alias": "something"
                }]),
                INVALID,
                "payload.inputs[0].kind",
            ),
            (
                "versionless input",
                "inputs",
                json!([{
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "alias": "invoice"
                }]),
                INVALID,
                "payload.inputs[0].version",
            ),
            (
                "as_of_param without an as_of input",
                "temporalScope",
                json!({ "mode": "as_of_param" }),
                INVALID,
                "payload.inputs",
            ),
            (
                "field_projection with empty fields",
                "calc",
                json!({ "kind": "field_projection", "fields": [] }),
                INVALID,
                "payload.calc.fields",
            ),
            (
                "effectful_ts calc",
                "calc",
                json!({ "kind": "effectful_ts", "sourceRef": "some/module.ts" }),
                IO_IN_VIEW,
                "payload.calc.kind",
            ),
            (
                "raw_promise calc",
                "calc",
                json!({ "kind": "raw_promise", "sourceRef": "some/module.ts" }),
                IO_IN_VIEW,
                "payload.calc.kind",
            ),
        ] {
            let mut payload = valid_derived_payload();
            payload[member] = value;
            let diagnostics = artifact_with_payload(&ArtifactKind::Derived, payload)
                .validate()
                .expect_err(label);
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.code == code && d.path.as_deref() == Some(path)),
                "{label}: expected {code} at {path}, got: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn validates_derived_with_as_of_param_temporal_scope_and_as_of_input() {
        let artifact = artifact_with_payload(
            &ArtifactKind::Derived,
            json!({
                "viewVersion": "0.1",
                "viewId": "compliance.view.invoice_at_submission",
                "inputs": [
                    {
                        "artifactId": "einvoicing.entity.invoice",
                        "kind": "entity",
                        "version": "0.1.0",
                        "alias": "invoice"
                    },
                    {
                        "artifactId": "einvoicing.entity.submission_event",
                        "kind": "entity",
                        "version": "0.1.0",
                        "alias": "as_of"
                    }
                ],
                "calc": { "kind": "field_projection", "fields": ["id"] },
                "outputSchema": { "type": "array" },
                "maintenance": "recompute_on_read",
                "temporalScope": { "mode": "as_of_param" }
            }),
        );
        assert!(
            artifact.validate().is_ok(),
            "as_of_param with as_of input should be valid: {:?}",
            artifact.validate()
        );
    }
}

mod catalog_protocol_tests {
    use super::super::*;
    #[test]
    fn write_only_graphql_connector_requires_valid_typed_protocol_and_mapping_pin() {
        let source = include_str!(
            "../../../../fixtures/dev/catalog-sync/definitions/shopify_demo.connector.json"
        );
        let value: serde_json::Value = serde_json::from_str(source).expect("fixture");
        let artifact = crate::decode_artifact_json(&value).expect("typed connector");
        assert!(
            artifact.validate().is_ok(),
            "write-only destination is declarative"
        );
        for extension in [
            serde_json::json!({"norn.shopify.graphql/1":{"apiVersion":"2025-01"}}),
            serde_json::json!({"norn.shopify.graphql/1":{"apiVersion":"2026-04","query":"arbitrary"}}),
            serde_json::json!({"norn.catalog.projection/1":{"actor":"catalog_projector","mapping":{"schemaId":"m","version":"1","sha256":"unpinned"}}}),
        ] {
            let mut invalid = value.clone();
            invalid["payload"]["extensions"] = extension;
            let artifact =
                crate::decode_artifact_json(&invalid).expect("extension reaches checker");
            assert!(
                artifact.validate().is_err(),
                "malformed protocol/profile must fail admission"
            );
        }
        let mut empty = value;
        empty["payload"]["commands"] = serde_json::json!([]);
        assert!(
            crate::decode_artifact_json(&empty)
                .expect("empty profile")
                .validate()
                .is_err()
        );
    }
}
