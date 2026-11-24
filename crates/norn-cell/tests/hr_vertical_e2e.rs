//! End-to-end `norn-cell` execution suite for the HR vertical (`time` + `org` +
//! `hr` + `loc-eg` + `hr-import`):
//! 1. Static admission of the full vertical against the Core base.
//! 2. Jurisdiction-agnostic base `hr` pack alone (`LaborCapacity`, `TimeInterval`,
//!    `LeaveRequest`/`LeaveBalance` `SliceKey::Link`, `AttendanceEvent` &
//!    `hr.attendance.reverse`, `PayrollFund`/`SalarySlip`/`PayrollRun`,
//!    `ExpenseClaim`, and Cedar `AccessIR` ownership & separation of duties).
//! 3. Conjunctive `hr ∧ loc-eg` composition (Egypt Labor Law No. 14 of 2025)
//!    with simultaneous multi-jurisdiction isolation (`EG` vs `GB` and
//!    pre-`2025-09-01` historical records) and `loc_eg.LaborProfile` 12h rule.
//! 4. Core 07 `hr-import` evidence-anchored ingest, `FEED-CONFLICT` supersession,
//!    and immutable terminal resolution refusal.
//! 5. Durable PII marking persistence, masking, reveal receipts, and
//!    exact-version declassification.

#[cfg(feature = "native")]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "HR vertical end-to-end kernel execution fixtures"
)]
mod suite {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use norn_cell::{
        InstalledDefinitionInput, MAIN_WORLD, MarkingDeclassification, MarkingReveal, Model,
        SqliteCell, StoreOptions,
    };
    use norn_checker::{
        EntityArtifactSetContext, ReceiptSources, admission_receipt_sha256, check_artifact_set,
        check_artifact_set_with_receipt, marking_catalog_from_artifacts,
    };
    use norn_ir::{
        AccessIr, ActionRoot, ArtifactEnvelope, ArtifactKind, DataQualityRuleRef, DecisionOutput,
        FactChange, InterfaceDef, MarkingId, MaskPolicy, ObjectTypeDef, ObjectTypeName, PropertyId,
        PropertyTypeDef, ResourceLimitProfile, RuntimeValue,
    };
    use norn_kernel::authz::{
        self, AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation,
        DenyReason, Pdp, PdpActionAuthorizer, PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
    };
    use norn_kernel::cell_host::{JsonObject, JsonValue};
    use norn_kernel::governing_laws::LawEvaluationError;
    use norn_kernel::{
        ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
        ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
        Command, CommitError, DecisionRefusal, DecisionTurn, FactEvent, FactKey, FactMutation,
        FactVersion, FactWrite, LinkKey, LinkMutation, LinkWrite, LogicalTime, TypeCatalog, World,
        commit_action, load_slice, prepare_decision,
    };
    use serde_json::{Value, json};

    type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

    const SCOPE_KEY: &str = "org_global_hr/dev";
    const ORG_ID: &str = "org_global_hr";
    const ENV_ID: &str = "dev";
    const KNOWN_AT: u64 = 1_795_420_800_000;

    const CORE_BASE_FILES: &[&str] = &[
        "core.Agent.interface.json",
        "core.AgentRole.interface.json",
        "core.Resource.interface.json",
        "core.Commitment.interface.json",
        "core.Hold.interface.json",
        "core.EconomicEvent.interface.json",
        "core.LedgerEffect.interface.json",
        "core.Claim.interface.json",
        "core.TimeInterval.interface.json",
        "identity.BusinessUnit.entity.json",
        "identity.Directory.interface.json",
        "party.Organization.entity.json",
        "norn.SourceEvidence.interface.json",
        "norn.SourceDocument.interface.json",
        "norn.ObservedLinkClaim.entity.json",
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ];

    const ALL_HR_PACKS: &[&str] = &["time", "org", "hr", "loc-eg", "hr-import"];
    const BASE_HR_PACKS: &[&str] = &["time", "org", "hr"];
    const HR_WITH_EG_PACKS: &[&str] = &["time", "org", "hr", "loc-eg"];
    const HR_IMPORT_PACKS: &[&str] = &["time", "org", "hr", "hr-import"];

    const TEST_PRINCIPALS: &[&str] = &[
        "principal:employee",
        "principal:employee_other",
        "principal:branch_manager",
        "principal:hr_manager",
        "principal:hr_reviewer",
        "principal:payroll_officer",
        "principal:hr_admin",
        "principal:hr_solver_worker",
        "principal:hr_import_worker",
    ];

    const RESOURCE_OWNER_FIELDS: &[&str] = &[
        "requester_user_id",
        "prepared_by_user_id",
        "applicant_user_id",
        "reviewer_user_id",
    ];

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read_json_value(path: &Path) -> Value {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn load_core_base_values() -> Vec<Value> {
        let core_dir = repo_root().join("registry/core/artifacts");
        CORE_BASE_FILES
            .iter()
            .map(|name| read_json_value(&core_dir.join(name)))
            .collect()
    }

    fn load_pack_values(packs: &[&str]) -> Vec<Value> {
        let packs_root = repo_root().join("registry/packs");
        let mut paths = Vec::new();
        for pack in packs {
            let dir = packs_root.join(pack).join("artifacts");
            let mut pack_paths: Vec<PathBuf> = fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
                .map(|entry| entry.expect("artifact dir entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            pack_paths.sort();
            paths.extend(pack_paths);
        }
        paths.iter().map(|path| read_json_value(path)).collect()
    }

    fn decode_envelopes(values: &[Value]) -> Vec<ArtifactEnvelope> {
        values
            .iter()
            .map(|value| serde_json::from_value(value.clone()).expect("artifact envelope decodes"))
            .collect()
    }

    fn action_call_type() -> ObjectTypeDef {
        let required = ["actionId", "actor", "turn", "transition", "from", "to"];
        let nullable_text = [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ];
        let nullable_json = [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ];
        let mut fields: Vec<Value> = required
            .iter()
            .map(|id| json!({"id": id, "type": {"kind": "text"}}))
            .collect();
        fields.extend(nullable_text.iter().map(
            |id| json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}}),
        ));
        fields.extend(nullable_json.iter().map(
            |id| json!({"id": id, "type": {"kind": "json-ext"}, "constraints": {"nullable": true}}),
        ));
        serde_json::from_value(json!({"name": "norn.ActionCall", "properties": fields}))
            .expect("norn.ActionCall schema decodes")
    }

    struct RegisteredAction {
        root: ActionRoot,
        artifact_id: String,
        version: String,
    }

    struct HrPrincipals {
        pdp: Pdp,
    }

    fn test_scope_key() -> ScopeKey {
        ScopeKey {
            org: ORG_ID.to_owned(),
            tenant: None,
            workspace: None,
            environment: Some(ENV_ID.to_owned()),
            business_unit: None,
        }
    }

    fn test_scope_map() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("orgId".to_owned(), ORG_ID.to_owned()),
            ("environmentId".to_owned(), ENV_ID.to_owned()),
        ])
    }

    fn actor_roles(actor: &str) -> Vec<String> {
        match actor {
            "principal:employee" | "principal:employee_other" => vec!["employee".to_owned()],
            "principal:branch_manager" => vec!["branch_manager".to_owned()],
            "principal:hr_manager" => vec!["hr_manager".to_owned()],
            "principal:hr_reviewer" => vec!["hr_reviewer".to_owned()],
            "principal:payroll_officer" => vec!["payroll_officer".to_owned()],
            "principal:hr_admin" => vec!["hr_admin".to_owned()],
            "principal:hr_solver_worker" => vec!["hr_solver_worker".to_owned()],
            "principal:hr_import_worker" => vec!["hr_import_worker".to_owned()],
            _ => Vec::new(),
        }
    }

    impl HrPrincipals {
        fn new(
            types: &[ObjectTypeDef],
            interfaces: &[InterfaceDef],
            properties: &[PropertyTypeDef],
            actions: &BTreeMap<String, RegisteredAction>,
            policies: &[AccessIr],
        ) -> Self {
            let action_roots: Vec<ActionRoot> = actions.values().map(|a| a.root.clone()).collect();
            let base_projection = norn_projections::project_cedar_schema_with_catalog(
                types,
                interfaces,
                properties,
                &action_roots,
            );
            let inferred = authz::infer_principal_attributes(policies, &base_projection)
                .expect("infer HR principal attributes");
            let projection =
                norn_projections::project_cedar_schema_with_catalog_and_principal_attributes(
                    types,
                    interfaces,
                    properties,
                    &action_roots,
                    &inferred,
                );
            let host_schema = norn_projections::project_host_access_schema(types);
            let schema = AuthzSchema::from_projected_cedarschema_fragments(&[
                &projection.schema,
                &host_schema,
            ])
            .expect("HR Cedar schema loads");
            let compiled = authz::compile_all_projected(&schema, policies, &projection)
                .expect("HR AccessIR policies compile against projected Cedar schema");

            let mut all_capabilities: Vec<String> =
                action_roots.iter().map(|a| a.action_id.clone()).collect();
            all_capabilities.push("norn.read".to_owned());
            all_capabilities.push("norn.reveal".to_owned());

            let grants: Vec<CapabilityGrant> = TEST_PRINCIPALS
                .iter()
                .map(|subject| CapabilityGrant {
                    subject_id: (*subject).to_owned(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    scope: Some(test_scope_key()),
                    capabilities: all_capabilities.clone(),
                    binding: None,
                })
                .collect();

            Self {
                pdp: Pdp::new(compiled, &grants).expect("HR PDP constructs"),
            }
        }

        fn access_request(
            actor: &str,
            action: &str,
            target: &FactKey,
            target_values: Option<&JsonObject>,
            dimensions: &BTreeMap<String, String>,
        ) -> Result<AccessRequest, ActionDenial> {
            let request_scope = ScopeKey {
                org: dimensions
                    .get("orgId")
                    .ok_or(ActionDenial::Invisible)?
                    .clone(),
                tenant: dimensions.get("tenantId").cloned(),
                workspace: dimensions.get("workspaceId").cloned(),
                environment: dimensions.get("environmentId").cloned(),
                business_unit: dimensions.get("businessUnitId").cloned(),
            };
            let mut resource_attributes = BTreeMap::new();
            if let Some(values) = target_values {
                for field in RESOURCE_OWNER_FIELDS {
                    if let Some(JsonValue::String(text)) = values.get(*field) {
                        resource_attributes
                            .insert((*field).to_owned(), AttrValue::String(text.clone()));
                    }
                }
            }
            Ok(AccessRequest {
                principal: PrincipalInput {
                    subject_id: actor.to_owned(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    roles: actor_roles(actor),
                    attributes: BTreeMap::from([(
                        "id".to_owned(),
                        AttrValue::String(actor.to_owned()),
                    )]),
                },
                scope: Some(request_scope.clone()),
                capability: action.to_owned(),
                action_id: action.to_owned(),
                resource: ResourceInput {
                    object_type: target.object_type.as_str().to_owned(),
                    id: target.key.clone(),
                    scope: request_scope,
                    attributes: resource_attributes,
                },
                context: BTreeMap::new(),
                binding: None,
                delegation: Delegation::Direct,
            })
        }

        fn decide_reveal(&self, actor: &str, marking_id: &str) -> PdpDecision {
            let base_request = Self::access_request(
                actor,
                "norn.reveal",
                &FactKey::new("norn.Marking", marking_id),
                None,
                &test_scope_map(),
            )
            .expect("reveal request builds");
            self.pdp
                .decide_marking(&base_request, marking_id, "norn.reveal")
        }
    }

    impl ActionAuthorizer for HrPrincipals {
        fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            PdpActionAuthorizer::new(self.pdp.clone(), |req: &ActionAuthzRequest<'_>| {
                Self::access_request(
                    req.actor,
                    &req.action.action_id,
                    req.target,
                    req.target_values,
                    req.scope,
                )
            })
            .authorize(request)
        }

        fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
            if actor_roles(request.actor).is_empty()
                || request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
                || request.scope.get("environmentId").map(String::as_str) != Some(ENV_ID)
            {
                return Err(ActionDenial::Invisible);
            }
            Ok(())
        }

        fn authorize_read_type(
            &self,
            request: &ActionReadTypeAuthzRequest<'_>,
        ) -> Result<(), ActionDenial> {
            if actor_roles(request.actor).is_empty()
                || request.scope.get("orgId").map(String::as_str) != Some(ORG_ID)
                || request.scope.get("environmentId").map(String::as_str) != Some(ENV_ID)
            {
                return Err(ActionDenial::Invisible);
            }
            Ok(())
        }
    }

    struct ExtractedCatalog {
        types: Vec<ObjectTypeDef>,
        properties: Vec<PropertyTypeDef>,
        interfaces: Vec<InterfaceDef>,
        actions: BTreeMap<String, RegisteredAction>,
        policies: Vec<AccessIr>,
    }

    fn extract_catalog(base_values: &[Value], artifact_values: &[Value]) -> ExtractedCatalog {
        let mut types = Vec::new();
        let mut properties = Vec::new();
        let mut interfaces = Vec::new();
        let mut actions = BTreeMap::new();
        let mut policies = Vec::new();

        for document in base_values.iter().chain(artifact_values.iter()) {
            let payload = &document["payload"];
            if let Some(definition) = payload.get("objectType") {
                types.push(serde_json::from_value(definition.clone()).expect("objectType decodes"));
            }
            if let Some(definition) = payload.get("interface") {
                interfaces
                    .push(serde_json::from_value(definition.clone()).expect("interface decodes"));
            }
            if let Some(Value::Array(definitions)) = payload.get("propertyTypes") {
                for definition in definitions {
                    properties.push(
                        serde_json::from_value(definition.clone()).expect("propertyType decodes"),
                    );
                }
            }
            if document["kind"] == "action" {
                let root: ActionRoot =
                    serde_json::from_value(payload.clone()).expect("action root decodes");
                let artifact_id = document["artifactId"]
                    .as_str()
                    .expect("artifactId")
                    .to_owned();
                let version = document["version"].as_str().expect("version").to_owned();
                actions.insert(
                    root.action_id.clone(),
                    RegisteredAction {
                        root,
                        artifact_id,
                        version,
                    },
                );
            }
            if document["kind"] == "policy" {
                policies
                    .push(serde_json::from_value(payload.clone()).expect("policy root decodes"));
            }
        }
        types.push(action_call_type());
        ExtractedCatalog {
            types,
            properties,
            interfaces,
            actions,
            policies,
        }
    }

    struct HrVerticalFixture {
        base_values: Vec<Value>,
        artifact_values: Vec<Value>,
        artifacts: Vec<ArtifactEnvelope>,
        admission_receipt: String,
        receipt_sha256: String,
        types: Vec<ObjectTypeDef>,
        properties: Vec<PropertyTypeDef>,
        interfaces: Vec<InterfaceDef>,
        actions: BTreeMap<String, RegisteredAction>,
        laws: Vec<ArtifactEnvelope>,
        principals: HrPrincipals,
    }

    impl HrVerticalFixture {
        fn load(packs: &[&str]) -> Self {
            let base_values = load_core_base_values();
            let artifact_values = load_pack_values(packs);
            let base = decode_envelopes(&base_values);
            let artifacts = decode_envelopes(&artifact_values);
            let to_runtime = |values: &[Value]| -> Vec<RuntimeValue> {
                values
                    .iter()
                    .map(|value| {
                        RuntimeValue::from_json(value.clone()).expect("exact artifact JSON")
                    })
                    .collect()
            };
            let sources = to_runtime(&artifact_values);
            let base_sources = to_runtime(&base_values);
            let registry = BTreeSet::new();
            let checked = check_artifact_set_with_receipt(
                &ReceiptSources::new(&sources, &base_sources).expect("receipt sources"),
                &EntityArtifactSetContext {
                    registry: &registry,
                    base: &base,
                    tenant: None,
                },
                &ResourceLimitProfile::norn_limits_v1(),
            )
            .expect("HR vertical admission receipt");
            assert!(
                checked.receipt.admitted,
                "packs {packs:?} must admit cleanly: {:#?}",
                checked.diagnostics
            );
            let admission_receipt = String::from_utf8(
                norn_ir::canonical::serialize(
                    &checked
                        .receipt
                        .to_canonical_value()
                        .expect("admission receipt canonical value"),
                )
                .expect("admission receipt canonicalizes"),
            )
            .expect("admission receipt is UTF-8");
            let receipt_sha256 =
                admission_receipt_sha256(&admission_receipt).expect("admission receipt sha256");
            let catalog = extract_catalog(&base_values, &artifact_values);
            let laws: Vec<ArtifactEnvelope> = artifacts
                .iter()
                .filter(|artifact| artifact.kind() == ArtifactKind::Law)
                .cloned()
                .collect();
            let principals = HrPrincipals::new(
                &catalog.types,
                &catalog.interfaces,
                &catalog.properties,
                &catalog.actions,
                &catalog.policies,
            );

            Self {
                base_values,
                artifact_values,
                artifacts,
                admission_receipt,
                receipt_sha256,
                types: catalog.types,
                properties: catalog.properties,
                interfaces: catalog.interfaces,
                actions: catalog.actions,
                laws,
                principals,
            }
        }

        fn model(&self) -> Model {
            Model::from_definitions_with_interfaces(&self.types, &self.properties, &self.interfaces)
                .expect("HR vertical model projects")
                .with_governing_laws(&self.laws)
                .expect("HR vertical governing laws bind")
        }

        fn catalog(&self) -> TypeCatalog<'_> {
            TypeCatalog::new(&self.types)
                .with_property_types(&self.properties)
                .with_interfaces(&self.interfaces)
                .with_governing_laws(&self.laws)
        }

        fn install_input(&self) -> InstalledDefinitionInput {
            let mut combined = self.artifact_values.clone();
            combined.extend(self.base_values.iter().cloned());
            let canonical_set = String::from_utf8(
                RuntimeValue::from_json(Value::Array(combined))
                    .expect("HR vertical + core base artifact set is exact JSON")
                    .canonical_json()
                    .expect("HR vertical + core base artifact set canonicalizes"),
            )
            .expect("canonical HR vertical + core base set is UTF-8");
            InstalledDefinitionInput {
                canonical_set,
                registry: Vec::new(),
                receipt_sha256: self.receipt_sha256.clone(),
                admission_receipt: Some(self.admission_receipt.clone()),
                readmission: None,
                diff: json!({"added": {"count": self.artifacts.len()}}),
                installed_at: KNOWN_AT,
                layout: None,
            }
        }

        fn open_installed_cell(&self, path: &Path) -> Cell {
            let model = self.model();
            let mut cell = Cell::open(
                path,
                model.clone(),
                StoreOptions::default(),
                SCOPE_KEY,
                |_: &Command<JsonObject>| panic!("HR vertical E2E actions are synchronous"),
            )
            .expect("SQLite HR cell opens");
            cell.advance_to(LogicalTime::from_millis(KNOWN_AT));
            cell.install_definitions(model, self.install_input(), Some(0))
                .expect("admitted HR vertical definitions install");
            cell
        }

        fn invoke(
            &self,
            cell: &mut Cell,
            action_id: &str,
            input: &Value,
            actor: &str,
            idempotency_key: &str,
        ) -> Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>> {
            let registered = &self.actions[action_id];
            let request = ActionCall {
                action: &registered.root,
                action_artifact_id: &registered.artifact_id,
                action_version: &registered.version,
                scope: test_scope_map(),
                actor: actor.to_owned(),
                input: decode_json_object(input),
                idempotency_key: idempotency_key.to_owned(),
                object_type: None,
                turn: DecisionTurn {
                    id: format!("turn:{idempotency_key}"),
                    now: LogicalTime::from_millis(KNOWN_AT),
                    world: World::Main,
                    consumes: None,
                },
                trace: None,
            };
            commit_action(cell, &self.catalog(), &request, &self.principals).map_err(Box::new)
        }
    }

    fn decode_json_object(value: &Value) -> JsonObject {
        serde_json::from_value(value.clone()).expect("JSON object decodes")
    }

    fn put_fact(object_type: &str, id: &str, values: &Value) -> FactWrite {
        FactWrite {
            key: FactKey::new(object_type, id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(values.clone()).expect("fact values decode"),
            },
        }
    }

    fn add_link(
        from_type: &str,
        from_id: &str,
        link: &str,
        to_type: &str,
        to_id: &str,
    ) -> LinkWrite {
        LinkWrite {
            key: LinkKey::new(
                FactKey::new(from_type, from_id),
                link,
                FactKey::new(to_type, to_id),
            ),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }
    }

    fn status_of(cell: &Cell, object_type: &str, id: &str) -> String {
        cell.load(&FactKey::new(object_type, id))
            .expect("object loads")
            .values
            .expect("object exists")["status"]
            .as_str()
            .expect("status is text")
            .to_owned()
    }

    fn assert_precondition_false(
        result: Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>>,
        context: &str,
    ) {
        let refusal = result.expect_err(context);
        assert!(
            matches!(
                refusal.as_ref(),
                ActionRefusal::PreconditionFailed {
                    precondition,
                    reason: None
                } if !precondition.is_empty()
            ),
            "{context}: expected PreconditionFailed {{ reason: None }}, got {refusal:?}"
        );
    }

    fn assert_permission_denied(
        result: Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>>,
        context: &str,
    ) {
        let refusal = result.expect_err(context);
        assert_eq!(refusal.code(), "action.permission_denied", "{context}");
    }

    fn commit_single_with_links(
        cell: &mut Cell,
        object_type: &str,
        id: &str,
        values: &Value,
        links: &[(&str, &str, &str)],
    ) -> Result<(), CommitError<norn_cell::CellError>> {
        let mut commit = CellCommit::empty();
        commit.writes.push(put_fact(object_type, id, values));
        for (link, to_type, to_id) in links {
            commit
                .links
                .push(add_link(object_type, id, link, to_type, to_id));
        }
        cell.commit(commit).map(|_| ())
    }

    fn assert_law_refusal(
        result: Result<(), CommitError<norn_cell::CellError>>,
        expected_law_id: &str,
        context: &str,
    ) {
        let error = result.expect_err(context);
        match error {
            CommitError::Law {
                reason: LawEvaluationError::Violated(failure),
                ..
            } => match &failure.rule_ref {
                DataQualityRuleRef::Law { law_id, .. } => {
                    assert_eq!(law_id, expected_law_id, "{context}");
                }
                other => panic!("{context}: expected Law rule_ref, got {other:?}"),
            },
            other => {
                panic!("{context}: expected CommitError::Law({expected_law_id}), got {other:?}")
            }
        }
    }

    fn assert_invariant_refusal(
        result: Result<(), CommitError<norn_cell::CellError>>,
        expected_id: &str,
        context: &str,
    ) {
        let error = result.expect_err(context);
        assert!(
            matches!(
                &error,
                CommitError::InvariantViolation { id, .. } if id.as_str() == expected_id
            ),
            "{context}: expected CommitError::InvariantViolation({expected_id}), got {error:?}"
        );
    }

    fn assert_action_law_refusal(
        result: Result<ActionOutcome, Box<ActionRefusal<norn_cell::CellError>>>,
        expected_law_id: &str,
        context: &str,
    ) {
        let refusal = result.expect_err(context);
        match refusal.as_ref() {
            ActionRefusal::Decision(DecisionRefusal::Law {
                reason: LawEvaluationError::Violated(failure),
                ..
            }) => match &failure.rule_ref {
                DataQualityRuleRef::Law { law_id, .. } => {
                    assert_eq!(law_id, expected_law_id, "{context}");
                }
                other => panic!("{context}: expected Law rule_ref, got {other:?}"),
            },
            other => panic!(
                "{context}: expected ActionRefusal::Decision(Law({expected_law_id})), got {other:?}"
            ),
        }
    }

    fn assert_immutable_state_write(
        fixture: &HrVerticalFixture,
        cell: &Cell,
        object_type: &str,
        id: &str,
        property: &str,
        new_value: &str,
        expected_state: &str,
    ) {
        let key = FactKey::new(object_type, id);
        let slice = load_slice(cell, [key.clone()]).expect("load slice for immutability check");
        let turn = DecisionTurn {
            id: format!("turn:immut:{id}"),
            now: LogicalTime::from_millis(KNOWN_AT),
            world: World::Main,
            consumes: None,
        };
        let output = DecisionOutput {
            facts: vec![FactChange::Set {
                object_type: ObjectTypeName(object_type.to_owned()),
                key: id.to_owned(),
                property: PropertyId(property.to_owned()),
                value: RuntimeValue::String(new_value.to_owned()),
            }],
            commands: Vec::new(),
            timers: Vec::new(),
        };
        let refusal =
            prepare_decision::<norn_cell::CellError>(&fixture.catalog(), &slice, &turn, &output)
                .expect_err("write to immutable terminal lifecycle state must refuse");
        assert!(
            matches!(
                &refusal,
                DecisionRefusal::ImmutableStateWrite {
                    key: refused_key,
                    lifecycle,
                    state,
                } if *refused_key == key && lifecycle == "status" && state == expected_state
            ),
            "expected ImmutableStateWrite(status={expected_state}) on {key}, got {refusal:?}"
        );
    }

    fn eg_54h_employment_json() -> Value {
        json!({
            "id": "emp_eg_ongoing_54h", "jurisdiction_code": "EG",
            "effective_from": "2024-01-01", "effective_to": null, "currency": "EGP",
            "base_salary": 18000, "hourly_overtime_rate": 150, "max_weekly_hours": 54,
            "status": "active"
        })
    }

    fn eg_15d_leave_allocation_json() -> Value {
        json!({
            "id": "la_eg_std_15d", "jurisdiction_code": "EG", "leave_kind": "annual",
            "entitlement_tier": "standard", "hazardous_or_remote": false,
            "qualifying_service_months": null, "starts_on": "2026-01-01",
            "ends_on": "2026-12-31", "allocated_days": 15,
            "carry_forward_days": 0, "status": "active"
        })
    }

    fn mask_text(value: &str, policy: &MaskPolicy) -> Option<String> {
        match policy {
            MaskPolicy::Redacted {} => Some("[REDACTED]".to_owned()),
            MaskPolicy::LastN { count } => {
                let chars: Vec<char> = value.chars().collect();
                let keep = usize::try_from(*count).unwrap_or(usize::MAX);
                let tail: String = if chars.len() > keep {
                    chars
                        .get(chars.len().saturating_sub(keep)..)
                        .unwrap_or_default()
                        .iter()
                        .collect()
                } else {
                    String::new()
                };
                Some(format!("[REDACTED]{tail}"))
            }
            MaskPolicy::Null {} | MaskPolicy::Refuse {} => None,
            MaskPolicy::Pseudonym { subject } => Some(format!("{subject} #PSEUDONYM")),
        }
    }

    fn seed_common_org_and_people(cell: &mut Cell) {
        let mut seed = CellCommit::empty();
        seed.writes.extend([
            put_fact(
                "time.Calendar",
                "cal_main",
                &json!({
                    "id": "cal_main",
                    "code": "global_utc",
                    "label": "Main Operating Calendar",
                    "iana_timezone": "UTC",
                    "dst_policy": "reject",
                    "tzdb_version": "2025b",
                    "workweek_start": "monday"
                }),
            ),
            put_fact(
                "org.OrgUnit",
                "unit_main",
                &json!({
                    "id": "unit_main",
                    "display_name": "Main Branch",
                    "kind": "organization",
                    "unit_code": "BRANCH-01"
                }),
            ),
            put_fact(
                "hr.Person",
                "person_ada",
                &json!({
                    "id": "person_ada",
                    "display_name": "Ada Lovelace",
                    "kind": "person",
                    "gender": "female",
                    "national_id": "29805150101234",
                    "phone": "+15550000001",
                    "primary_skill": "cashier"
                }),
            ),
            put_fact(
                "org.Membership",
                "mem_ada",
                &json!({
                    "id": "mem_ada",
                    "role": "cashier",
                    "valid_from": "2024-01-01",
                    "valid_to": null
                }),
            ),
            put_fact(
                "hr.LeavePolicy",
                "lp_std",
                &json!({
                    "id": "lp_std",
                    "policy_code": "STD-2026",
                    "label": "Standard Leave Policy",
                    "annual_base_days": 21,
                    "sick_base_days": 10,
                    "emergency_base_days": 3,
                    "max_carry_forward_days": 5,
                    "status": "active"
                }),
            ),
        ]);
        seed.links.extend([
            add_link(
                "org.Membership",
                "mem_ada",
                "agent",
                "hr.Person",
                "person_ada",
            ),
            add_link(
                "org.Membership",
                "mem_ada",
                "unit",
                "org.OrgUnit",
                "unit_main",
            ),
            add_link(
                "hr.LeavePolicy",
                "lp_std",
                "unit",
                "org.OrgUnit",
                "unit_main",
            ),
        ]);
        cell.commit(seed)
            .expect("seed common org, calendar, and person");
    }

    // ---------------------------------------------------------------- Test 1: Full Static Admission

    #[test]
    fn hr_vertical_packs_admit_together_against_core_base() {
        let base = decode_envelopes(&load_core_base_values());
        let artifacts = decode_envelopes(&load_pack_values(ALL_HR_PACKS));
        let registry = BTreeSet::new();
        let findings = check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &registry,
                base: &base,
                tenant: None,
            },
        );
        let errors: Vec<_> = findings.iter().filter(|d| d.is_error()).collect();
        assert!(
            errors.is_empty(),
            "HR vertical must admit cleanly: {errors:#?}"
        );

        let law_ids: BTreeSet<&str> = artifacts
            .iter()
            .filter(|a| a.kind() == ArtifactKind::Law)
            .map(|a| a.artifact_id.as_str())
            .collect();
        for expected in [
            "hr.LaborCapacity.non_negative.law",
            "hr.LeaveBalance.non_negative.law",
            "hr.PayrollFund.non_negative.law",
            "hr.SalarySlip.conservation.law",
            "hr.ExpenseClaim.amount_conservation.law",
            "loc_eg.LaborProfile.statutory_floors.law",
            "loc_eg.hr.PreferenceProfile.labor_law_14_2025.law",
            "loc_eg.hr.Employment.max_weekly_hours.law",
            "loc_eg.hr.LeaveAllocation.annual_floor.law",
            "hr_import.SourceBatch.content_addressed.law",
            "hr_import.FieldEvidence.valid_anchor.law",
            "hr_import.ExtractedCandidate.confidence_bounds.law",
            "hr_import.ExtractedCandidate.promoted_requires_clean_review.law",
            "hr_import.ReviewDecision.authenticated.law",
            "hr_import.DataQualityFinding.terminal_requires_audit_evidence.law",
            "hr_import.DataQualityFinding.feed_conflict_is_disputed.law",
        ] {
            assert!(law_ids.contains(expected), "missing law {expected}");
        }
        let law_findings: Vec<_> = findings
            .iter()
            .filter(|d| {
                d.artifact_id
                    .as_deref()
                    .is_some_and(|id| law_ids.contains(id))
            })
            .collect();
        assert!(law_findings.is_empty(), "law findings: {law_findings:#?}");

        let catalog = marking_catalog_from_artifacts(&artifacts);
        assert_eq!(
            catalog
                .get(&MarkingId("pii.gov_id".to_owned()))
                .unwrap()
                .mask,
            MaskPolicy::LastN { count: 4 }
        );
        assert_eq!(
            catalog
                .get(&MarkingId("pii.raw_chat_media".to_owned()))
                .unwrap()
                .mask,
            MaskPolicy::Redacted {}
        );
    }

    // ---------------------------------------------------------------- Test 2: Base `hr` Pack Alone

    fn assert_labor_capacity_and_time_interval(cell: &mut Cell) {
        commit_single_with_links(
            cell,
            "hr.LaborCapacity",
            "lc_ada_w48",
            &json!({
                "id": "lc_ada_w48", "resource_code": "labor_hours", "quantity": 40,
                "unit": "h", "currency": null, "valuation_method": null,
                "role_code": "cashier", "period_code": "2026-W48"
            }),
            &[
                ("owner", "hr.Person", "person_ada"),
                ("custodian", "org.OrgUnit", "unit_main"),
            ],
        )
        .expect("valid positive labor capacity commits");

        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.LaborCapacity",
                "lc_neg",
                &json!({
                    "id": "lc_neg", "resource_code": "labor_hours", "quantity": -4,
                    "unit": "h", "currency": null, "valuation_method": null,
                    "role_code": "cashier", "period_code": "2026-W48"
                }),
                &[
                    ("owner", "hr.Person", "person_ada"),
                    ("custodian", "org.OrgUnit", "unit_main"),
                ],
            ),
            "hr.LaborCapacity.non_negative",
            "negative hr.LaborCapacity must be refused by hr.LaborCapacity.non_negative",
        );

        // core.TimeInterval proper_interval invariant refuses negative/zero duration.
        assert_invariant_refusal(
            commit_single_with_links(
                cell,
                "hr.ShiftRequirement",
                "sr_neg",
                &json!({
                    "id": "sr_neg", "role_code": "cashier",
                    "starts_at": "2026-11-28T17:00:00Z", "ends_at": "2026-11-28T09:00:00Z",
                    "timezone": "UTC", "required_count": 1
                }),
                &[("unit", "org.OrgUnit", "unit_main")],
            ),
            "proper_interval",
            "negative-duration core.TimeInterval must be refused",
        );

        // Jurisdiction-agnostic base `hr` admits EG 54h/wk preference, EG 54h/wk employment,
        // and EG 15d annual leave allocation when `loc-eg` is NOT installed.
        commit_single_with_links(
            cell,
            "hr.PreferenceProfile",
            "pref_base_54h",
            &json!({
                "id": "pref_base_54h", "jurisdiction_code": "EG", "label": "Generic 54h",
                "currency": "USD", "weekly_overtime_budget": 1500, "min_rest_hours": 10,
                "max_daily_hours": 10, "max_weekly_hours": 54, "min_weekly_rest_hours": 24,
                "max_consecutive_days": 6, "weekend_fairness_priority": "high",
                "staff_preference_priority": "medium", "avoid_split_shifts": true
            }),
            &[
                ("unit", "org.OrgUnit", "unit_main"),
                ("calendar", "time.Calendar", "cal_main"),
            ],
        )
        .expect("base hr pack admits EG 54h preference without loc-eg");
        commit_single_with_links(
            cell,
            "hr.Employment",
            "emp_eg_ongoing_54h",
            &eg_54h_employment_json(),
            &[
                ("person", "hr.Person", "person_ada"),
                ("membership", "org.Membership", "mem_ada"),
                ("home_unit", "org.OrgUnit", "unit_main"),
            ],
        )
        .expect("base hr pack admits exact EG 54h employment without loc-eg");
    }

    fn seed_leave_balances(cell: &mut Cell) {
        let mut seed = CellCommit::empty();
        seed.writes.extend([
        put_fact(
            "hr.LeaveBalance",
            "bal_ada",
            &json!({
                "id": "bal_ada", "resource_code": "annual_leave", "quantity": 5, "unit": "d",
                "currency": null, "valuation_method": null, "leave_kind": "annual", "period_year": 2026
            }),
        ),
        put_fact(
            "hr.LeaveBalance",
            "bal_other",
            &json!({
                "id": "bal_other", "resource_code": "annual_leave", "quantity": 20, "unit": "d",
                "currency": null, "valuation_method": null, "leave_kind": "annual", "period_year": 2026
            }),
        ),
    ]);
        seed.links.extend([
            add_link(
                "hr.LeaveBalance",
                "bal_ada",
                "owner",
                "hr.Person",
                "person_ada",
            ),
            add_link(
                "hr.LeaveBalance",
                "bal_other",
                "owner",
                "hr.Person",
                "person_ada",
            ),
        ]);
        cell.commit(seed).expect("seed leave balances");
    }

    fn seed_leave_fixtures(cell: &mut Cell) {
        seed_leave_balances(cell);
        let mut seed = CellCommit::empty();
        seed.writes.extend([
        put_fact(
            "hr.LeaveRequest",
            "lr_valid",
            &json!({
                "id": "lr_valid", "requester_user_id": "principal:employee", "leave_kind": "annual",
                "starts_on": "2026-12-01", "ends_on": "2026-12-02", "days": 2,
                "medical_note": null, "status": "draft"
            }),
        ),
        put_fact(
            "hr.LeaveRequest",
            "lr_over",
            &json!({
                "id": "lr_over", "requester_user_id": "principal:employee", "leave_kind": "annual",
                "starts_on": "2026-12-05", "ends_on": "2026-12-14", "days": 10,
                "medical_note": null, "status": "draft"
            }),
        ),
        put_fact(
            "hr.LeaveRequest",
            "lr_mgr_own",
            &json!({
                "id": "lr_mgr_own", "requester_user_id": "principal:branch_manager", "leave_kind": "annual",
                "starts_on": "2026-12-18", "ends_on": "2026-12-19", "days": 2,
                "medical_note": null, "status": "pending"
            }),
        ),
    ]);
        for req in ["lr_valid", "lr_over", "lr_mgr_own"] {
            seed.links.push(add_link(
                "hr.LeaveRequest",
                req,
                "person",
                "hr.Person",
                "person_ada",
            ));
            seed.links.push(add_link(
                "hr.LeaveRequest",
                req,
                "balance",
                "hr.LeaveBalance",
                "bal_ada",
            ));
        }
        cell.commit(seed).expect("seed leave requests");
        // Base `hr` without `loc-eg` admits the exact 15-day EG standard annual leave allocation
        // that `loc_eg.hr.LeaveAllocation.annual_floor` refuses once `loc-eg` is composed.
        commit_single_with_links(
            cell,
            "hr.LeaveAllocation",
            "la_eg_std_15d",
            &eg_15d_leave_allocation_json(),
            &[
                ("person", "hr.Person", "person_ada"),
                ("policy", "hr.LeavePolicy", "lp_std"),
                ("balance", "hr.LeaveBalance", "bal_ada"),
            ],
        )
        .expect("base hr pack admits exact EG 15d leave allocation without loc-eg");
    }

    fn assert_leave_link_binding_and_access_sod(fixture: &HrVerticalFixture, cell: &mut Cell) {
        seed_leave_fixtures(cell);

        // AccessIR employee ownership: another employee cannot submit principal:employee's request.
        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr.leave.submit",
                &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_ada"}),
                "principal:employee_other",
                "leave-other-emp-denied",
            ),
            "other employee cannot submit another employee's leave request",
        );

        // SliceKey::Link refuses an unlinked balance even when it has ample days.
        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr.leave.submit",
                &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_other"}),
                "principal:employee",
                "leave-unlinked-balance-refused",
            ),
            "unlinked balance must fail SliceKey::Link precondition",
        );

        // Over-balance request refused cleanly.
        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr.leave.submit",
                &json!({"leaveRequestId": "lr_over", "balanceId": "bal_ada"}),
                "principal:employee",
                "leave-over-balance-refused",
            ),
            "over-balance leave request must refuse",
        );

        fixture
            .invoke(
                cell,
                "hr.leave.submit",
                &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_ada"}),
                "principal:employee",
                "leave-submit-valid",
            )
            .expect("owner employee submits linked leave request");
        assert_eq!(status_of(cell, "hr.LeaveRequest", "lr_valid"), "pending");

        // Separation of duties: manager cannot approve own pending leave request.
        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr.leave.approve",
                &json!({"leaveRequestId": "lr_mgr_own", "balanceId": "bal_ada"}),
                "principal:branch_manager",
                "leave-mgr-self-approve-denied",
            ),
            "manager cannot approve own leave request (SoD)",
        );

        fixture
            .invoke(
                cell,
                "hr.leave.approve",
                &json!({"leaveRequestId": "lr_valid", "balanceId": "bal_ada"}),
                "principal:branch_manager",
                "leave-approve-valid",
            )
            .expect("independent branch manager approves valid leave request");
        assert_eq!(status_of(cell, "hr.LeaveRequest", "lr_valid"), "approved");
    }

    fn seed_roster_and_shift_commitment(cell: &mut Cell) {
        commit_single_with_links(
            cell,
            "hr.Roster",
            "ros_w48",
            &json!({
                "id": "ros_w48", "period_start": "2026-11-23", "period_end": "2026-11-29",
                "solver_receipt_digest": "sha256:solver_w48", "status": "published"
            }),
            &[
                ("unit", "org.OrgUnit", "unit_main"),
                (
                    "preference_profile",
                    "hr.PreferenceProfile",
                    "pref_base_54h",
                ),
            ],
        )
        .expect("roster commits");
        commit_single_with_links(
            cell,
            "hr.ShiftCommitment",
            "shift_ada_mon",
            &json!({
                "id": "shift_ada_mon", "status": "promised", "due_at": "2026-11-23T17:00:00Z",
                "quantity": 8, "unit": "h", "currency": null,
                "starts_at": "2026-11-23T09:00:00Z", "ends_at": "2026-11-23T17:00:00Z",
                "timezone": "UTC", "role_code": "cashier"
            }),
            &[
                ("provider", "hr.Person", "person_ada"),
                ("receiver", "org.OrgUnit", "unit_main"),
                ("resource", "hr.LaborCapacity", "lc_ada_w48"),
                ("roster", "hr.Roster", "ros_w48"),
            ],
        )
        .expect("shift commitment commits");
    }

    fn assert_attendance_record_and_reversal(fixture: &HrVerticalFixture, cell: &mut Cell) {
        seed_roster_and_shift_commitment(cell);

        let att = json!({
            "id": "att_ada_mon", "action": "produce", "occurred_at": "2026-11-23T17:05:00Z",
            "quantity": "8", "unit": "h", "currency": null,
            "starts_at": "2026-11-23T09:00:00Z", "ends_at": "2026-11-23T17:00:00Z", "timezone": "UTC",
            "providerId": "person_ada", "receiverId": "unit_main",
            "resourceId": "lc_ada_w48", "shiftCommitmentId": "shift_ada_mon"
        });
        fixture
            .invoke(
                cell,
                "hr.attendance.record",
                &att,
                "principal:branch_manager",
                "att-rec-1",
            )
            .expect("attendance event creates");
        let replay = fixture
            .invoke(
                cell,
                "hr.attendance.record",
                &att,
                "principal:branch_manager",
                "att-rec-1",
            )
            .expect("exact retry replays");
        assert!(matches!(replay, ActionOutcome::Replayed { .. }));

        // Reversal refuses same REA action (`produce`), mismatched quantity (`7`), and mismatched unit (`d`).
        for (rev_id, action, qty, unit, key) in [
            ("att_rev_bad_act", "produce", "8", "h", "rev-bad-act"),
            ("att_rev_bad_qty", "consume", "7", "h", "rev-bad-qty"),
            ("att_rev_bad_unit", "consume", "8", "d", "rev-bad-unit"),
        ] {
            assert_precondition_false(
                fixture.invoke(
                    cell,
                    "hr.attendance.reverse",
                    &json!({
                        "id": rev_id, "action": action, "occurred_at": "2026-11-23T18:00:00Z",
                        "quantity": qty, "unit": unit, "currency": null,
                        "starts_at": "2026-11-23T09:00:00Z", "ends_at": "2026-11-23T17:00:00Z",
                        "timezone": "UTC", "providerId": "person_ada", "receiverId": "unit_main",
                        "resourceId": "lc_ada_w48", "reversesEventId": "att_ada_mon"
                    }),
                    "principal:branch_manager",
                    key,
                ),
                "invalid attendance reversal must refuse",
            );
        }

        fixture
            .invoke(
                cell,
                "hr.attendance.reverse",
                &json!({
                    "id": "att_rev_ok", "action": "consume", "occurred_at": "2026-11-23T18:00:00Z",
                    "quantity": "8", "unit": "h", "currency": null,
                    "starts_at": "2026-11-23T09:00:00Z", "ends_at": "2026-11-23T17:00:00Z",
                    "timezone": "UTC", "providerId": "person_ada", "receiverId": "unit_main",
                    "resourceId": "lc_ada_w48", "reversesEventId": "att_ada_mon"
                }),
                "principal:branch_manager",
                "rev-ok",
            )
            .expect("inverse REA attendance reversal commits");
        assert!(
            cell.load_link(&LinkKey::new(
                FactKey::new("hr.AttendanceEvent", "att_rev_ok"),
                "reverses",
                FactKey::new("hr.AttendanceEvent", "att_ada_mon"),
            ))
            .expect("reverses link loads")
            .exists()
        );
    }

    fn assert_payroll_fund_and_salary_slip(cell: &mut Cell) {
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.PayrollFund",
                "pf_bad_unit",
                &json!({"id": "pf_bad_unit", "resource_code": "PF", "quantity": 1000, "unit": "h", "currency": "USD", "valuation_method": null}),
                &[("owner", "org.OrgUnit", "unit_main")],
            ),
            "hr.PayrollFund.non_negative",
            "PayrollFund with non-null UCUM unit must be refused",
        );
        commit_single_with_links(
            cell,
            "hr.PayrollFund",
            "pf_usd",
            &json!({"id": "pf_usd", "resource_code": "PAYROLL-USD", "quantity": 100_000, "unit": null, "currency": "USD", "valuation_method": null}),
            &[("owner", "org.OrgUnit", "unit_main")],
        )
        .expect("USD payroll fund commits");
        commit_single_with_links(
            cell,
            "hr.PayrollRun",
            "pr_2026_11",
            &json!({
                "id": "pr_2026_11", "period_code": "2026-11",
                "starts_at": "2026-11-01T00:00:00Z", "ends_at": "2026-12-01T00:00:00Z", "timezone": "UTC",
                "currency": "USD", "total_gross": 5000, "total_deductions": 850, "total_net": 4150,
                "prepared_by_user_id": "principal:payroll_officer", "status": "draft"
            }),
            &[
                ("unit", "org.OrgUnit", "unit_main"),
                ("fund", "hr.PayrollFund", "pf_usd"),
            ],
        )
        .expect("payroll run commits");
        let slip_links = [
            ("provider", "org.OrgUnit", "unit_main"),
            ("receiver", "hr.Person", "person_ada"),
            ("resource", "hr.PayrollFund", "pf_usd"),
            ("payroll_run", "hr.PayrollRun", "pr_2026_11"),
        ];
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.SalarySlip",
                "slip_mismatch",
                &json!({
                    "id": "slip_mismatch", "status": "promised", "due_at": "2026-11-30T17:00:00Z",
                    "quantity": 9000, "unit": null, "currency": "USD",
                    "gross_pay": 5000, "total_deductions": 850, "net_pay": 4150
                }),
                &slip_links,
            ),
            "hr.SalarySlip.conservation",
            "SalarySlip with quantity != net_pay must refuse",
        );
        commit_single_with_links(
            cell,
            "hr.SalarySlip",
            "slip_ok",
            &json!({
                "id": "slip_ok", "status": "promised", "due_at": "2026-11-30T17:00:00Z",
                "quantity": 4150, "unit": null, "currency": "USD",
                "gross_pay": 5000, "total_deductions": 850, "net_pay": 4150
            }),
            &slip_links,
        )
        .expect("conserved SalarySlip commits");
    }

    fn assert_payroll_run_and_expenses(fixture: &HrVerticalFixture, cell: &mut Cell) {
        assert_payroll_fund_and_salary_slip(cell);
        fixture
            .invoke(
                cell,
                "hr.payroll.calculate",
                &json!({"payrollRunId": "pr_2026_11"}),
                "principal:payroll_officer",
                "pr-calc",
            )
            .expect("payroll calculates");
        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr.payroll.approve",
                &json!({"payrollRunId": "pr_2026_11"}),
                "principal:payroll_officer",
                "pr-self-approve",
            ),
            "preparer cannot approve own payroll run",
        );
        fixture
            .invoke(
                cell,
                "hr.payroll.approve",
                &json!({"payrollRunId": "pr_2026_11"}),
                "principal:hr_manager",
                "pr-approve",
            )
            .expect("independent HR manager approves payroll");
        fixture
            .invoke(
                cell,
                "hr.payroll.post",
                &json!({"payrollRunId": "pr_2026_11"}),
                "principal:payroll_officer",
                "pr-post",
            )
            .expect("payroll posts with matching PayrollFund");
        assert_eq!(status_of(cell, "hr.PayrollRun", "pr_2026_11"), "posted");
        let exp_links = [
            ("creditor", "hr.Person", "person_ada"),
            ("debtor", "org.OrgUnit", "unit_main"),
        ];
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.ExpenseClaim",
                "exp_bad_amt",
                &json!({
                    "id": "exp_bad_amt", "amount": 250, "currency": "USD", "due_date": "2026-12-20",
                    "requester_user_id": "principal:employee", "claimed_amount": 250,
                    "sanctioned_amount": 220, "description": "Divergent amount", "status": "approved"
                }),
                &exp_links,
            ),
            "hr.ExpenseClaim.amount_conservation",
            "approved ExpenseClaim with amount != sanctioned_amount must refuse",
        );
        commit_single_with_links(
            cell,
            "hr.ExpenseClaim",
            "exp_ok",
            &json!({
                "id": "exp_ok", "amount": 220, "currency": "USD", "due_date": "2026-12-20",
                "requester_user_id": "principal:employee", "claimed_amount": 250,
                "sanctioned_amount": 220, "description": "Site transit", "status": "draft"
            }),
            &exp_links,
        )
        .expect("valid draft ExpenseClaim commits");
        fixture
            .invoke(
                cell,
                "hr.expense_claim.submit",
                &json!({"expenseClaimId": "exp_ok"}),
                "principal:employee",
                "exp-sub",
            )
            .expect("employee submits own expense claim");
        fixture
            .invoke(
                cell,
                "hr.expense_claim.approve",
                &json!({"expenseClaimId": "exp_ok"}),
                "principal:branch_manager",
                "exp-app",
            )
            .expect("independent manager approves expense claim");
        assert_eq!(status_of(cell, "hr.ExpenseClaim", "exp_ok"), "approved");
    }

    #[test]
    fn hr_base_pack_alone_enforces_domain_physics_and_access_sod() {
        let fixture = HrVerticalFixture::load(BASE_HR_PACKS);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hr-base.sqlite");
        let mut cell = fixture.open_installed_cell(&path);
        seed_common_org_and_people(&mut cell);

        assert_labor_capacity_and_time_interval(&mut cell);
        assert_leave_link_binding_and_access_sod(&fixture, &mut cell);
        assert_attendance_record_and_reversal(&fixture, &mut cell);
        assert_payroll_run_and_expenses(&fixture, &mut cell);

        cell.store()
            .read()
            .verify_ledger()
            .expect("base HR ledger verifies");
    }

    // ---------------------------------------------------------------- Test 3: Conjunctive `hr ∧ loc-eg`

    fn assert_loc_eg_preference_profiles(cell: &mut Cell) {
        let links = [
            ("unit", "org.OrgUnit", "unit_main"),
            ("calendar", "time.Calendar", "cal_main"),
        ];
        // Non-compliant EG preference profiles refused (>48h/wk, >8h/day, <24h weekly rest).
        for (id, daily, weekly, rest) in [
            ("prof_eg_52h", 8, 52, 24),
            ("prof_eg_9h", 9, 45, 24),
            ("prof_eg_12h_rest", 8, 48, 12),
        ] {
            assert_law_refusal(
                commit_single_with_links(
                    cell,
                    "hr.PreferenceProfile",
                    id,
                    &json!({
                        "id": id, "jurisdiction_code": "EG", "label": "Illegal EG",
                        "currency": "EGP", "weekly_overtime_budget": 1500, "min_rest_hours": 10,
                        "max_daily_hours": daily, "max_weekly_hours": weekly,
                        "min_weekly_rest_hours": rest, "max_consecutive_days": 6,
                        "weekend_fairness_priority": "high", "staff_preference_priority": "medium",
                        "avoid_split_shifts": true
                    }),
                    &links,
                ),
                "loc_eg.hr.PreferenceProfile.labor_law_14_2025",
                "illegal EG preference profile must be refused by loc-eg Law 14/2025 overlay",
            );
        }

        // Compliant EG profile AND non-EG GB >48h/>8h profile BOTH admit on the same cell.
        for (id, jur, cur, daily, weekly) in [
            ("prof_eg_ok", "EG", "EGP", 8, 48),
            ("prof_gb_ok", "GB", "GBP", 10, 52),
        ] {
            commit_single_with_links(
                cell,
                "hr.PreferenceProfile",
                id,
                &json!({
                    "id": id, "jurisdiction_code": jur, "label": id,
                    "currency": cur, "weekly_overtime_budget": 1500, "min_rest_hours": 10,
                    "max_daily_hours": daily, "max_weekly_hours": weekly,
                    "min_weekly_rest_hours": 24, "max_consecutive_days": 6,
                    "weekend_fairness_priority": "high", "staff_preference_priority": "medium",
                    "avoid_split_shifts": true
                }),
                &links,
            )
            .unwrap_or_else(|e| panic!("{id} must admit: {e}"));
        }
    }

    fn assert_loc_eg_employments(cell: &mut Cell) {
        let links = [
            ("person", "hr.Person", "person_ada"),
            ("membership", "org.Membership", "mem_ada"),
            ("home_unit", "org.OrgUnit", "unit_main"),
        ];
        // Replay exact same EG 54h/wk employment admitted in base `hr`-only cell: REFUSED here!
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.Employment",
                "emp_eg_ongoing_54h",
                &eg_54h_employment_json(),
                &links,
            ),
            "loc_eg.hr.Employment.max_weekly_hours",
            "active pre-2025-09-01 EG employment at 54h/wk must be refused by loc-eg",
        );

        // Non-EG GB 54h/wk AND pre-2025-09-01-ended EG 54h/wk BOTH admit on the same cell.
        for (id, jur, cur, from, to, status) in [
            (
                "emp_gb_54h",
                "GB",
                "GBP",
                "2026-01-01",
                Value::Null,
                "active",
            ),
            (
                "emp_eg_hist_54h",
                "EG",
                "EGP",
                "2025-01-01",
                json!("2025-08-31"),
                "terminated",
            ),
        ] {
            commit_single_with_links(
                cell,
                "hr.Employment",
                id,
                &json!({
                    "id": id, "jurisdiction_code": jur, "effective_from": from, "effective_to": to,
                    "currency": cur, "base_salary": 15000, "hourly_overtime_rate": 120,
                    "max_weekly_hours": 54, "status": status
                }),
                &links,
            )
            .unwrap_or_else(|e| panic!("{id} must admit cleanly: {e}"));
        }
    }

    fn assert_loc_eg_leave_allocations(cell: &mut Cell) {
        commit_single_with_links(
            cell,
            "hr.LeaveBalance",
            "bal_eg",
            &json!({
                "id": "bal_eg", "resource_code": "annual_leave", "quantity": 21, "unit": "d",
                "currency": null, "valuation_method": null, "leave_kind": "annual", "period_year": 2026
            }),
            &[("owner", "hr.Person", "person_ada")],
        )
        .expect("leave balance commits");
        let alloc_links = [
            ("person", "hr.Person", "person_ada"),
            ("policy", "hr.LeavePolicy", "lp_std"),
            ("balance", "hr.LeaveBalance", "bal_eg"),
        ];

        // Replay exact same EG 15d standard allocation admitted in base `hr`-only cell: REFUSED!
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "hr.LeaveAllocation",
                "la_eg_std_15d",
                &eg_15d_leave_allocation_json(),
                &alloc_links,
            ),
            "loc_eg.hr.LeaveAllocation.annual_floor",
            "exact EG 15d standard allocation must be refused by Art. 124 floor",
        );

        for (id, tier, haz, months, days) in [
            (
                "la_eg_pro_tiny",
                "prorated_initial",
                true,
                json!(6),
                json!(0.01),
            ),
            (
                "la_eg_pro_4m",
                "prorated_initial",
                false,
                json!(4),
                json!(7.5),
            ),
        ] {
            assert_law_refusal(
                commit_single_with_links(
                    cell,
                    "hr.LeaveAllocation",
                    id,
                    &json!({
                        "id": id, "jurisdiction_code": "EG", "leave_kind": "annual",
                        "entitlement_tier": tier, "hazardous_or_remote": haz,
                        "qualifying_service_months": months, "starts_on": "2026-01-01",
                        "ends_on": "2026-12-31", "allocated_days": days,
                        "carry_forward_days": 0, "status": "active"
                    }),
                    &alloc_links,
                ),
                "loc_eg.hr.LeaveAllocation.annual_floor",
                "sub-floor EG prorated allocation must be refused by Art. 124 floor",
            );
        }

        commit_single_with_links(
            cell,
            "hr.LeaveAllocation",
            "la_eg_pro_haz_ok",
            &json!({
                "id": "la_eg_pro_haz_ok", "jurisdiction_code": "EG", "leave_kind": "annual",
                "entitlement_tier": "prorated_initial", "hazardous_or_remote": true,
                "qualifying_service_months": 6, "starts_on": "2026-07-01", "ends_on": "2026-12-31",
                "allocated_days": 14.5, "carry_forward_days": 0, "status": "active"
            }),
            &alloc_links,
        )
        .expect("compliant 14.5d EG hazardous prorated allocation admits");
        commit_single_with_links(
            cell,
            "hr.LeaveAllocation",
            "la_gb_10d_ok",
            &json!({
                "id": "la_gb_10d_ok", "jurisdiction_code": "GB", "leave_kind": "annual",
                "entitlement_tier": "standard", "hazardous_or_remote": false,
                "qualifying_service_months": null, "starts_on": "2026-01-01", "ends_on": "2026-12-31",
                "allocated_days": 10, "carry_forward_days": 0, "status": "active"
            }),
            &alloc_links,
        )
        .expect("GB leave allocation is unaffected by loc-eg overlay");
    }

    fn assert_loc_eg_labor_profile(cell: &mut Cell) {
        let lp_links = [("unit", "org.OrgUnit", "unit_main")];
        let make_lp = |id: &str, intermittent: bool| {
            json!({
                "id": id, "effective_from": "2025-09-01", "iana_timezone": "Africa/Cairo",
                "currency": "EGP", "intermittent_or_preparatory_work": intermittent,
                "max_regular_weekly_hours": 48, "max_daily_hours": 8,
                "max_workplace_presence_hours": 12, "min_weekly_rest_hours": 24,
                "first_year_annual_leave_days": 15, "standard_annual_leave_days": 21,
                "senior_annual_leave_days": 30, "disability_annual_leave_days": 45,
                "hazardous_remote_supplement_days": 7, "seniority_threshold_years": 10,
                "weekend_pattern": "friday_saturday"
            })
        };
        assert_law_refusal(
            commit_single_with_links(
                cell,
                "loc_eg.LaborProfile",
                "lp_12h_bad",
                &make_lp("lp_12h_bad", false),
                &lp_links,
            ),
            "loc_eg.LaborProfile.statutory_floors",
            "12h presence must refuse when intermittent_or_preparatory_work is false",
        );
        commit_single_with_links(
            cell,
            "loc_eg.LaborProfile",
            "lp_12h_ok",
            &make_lp("lp_12h_ok", true),
            &lp_links,
        )
        .expect("12h presence admits when intermittent_or_preparatory_work is true");
    }

    #[test]
    fn hr_and_loc_eg_conjunctive_overlay_enforces_law_14_2025_with_isolation() {
        let fixture = HrVerticalFixture::load(HR_WITH_EG_PACKS);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hr-loc-eg.sqlite");
        let mut cell = fixture.open_installed_cell(&path);
        seed_common_org_and_people(&mut cell);

        assert_loc_eg_preference_profiles(&mut cell);
        assert_loc_eg_employments(&mut cell);
        assert_loc_eg_leave_allocations(&mut cell);
        assert_loc_eg_labor_profile(&mut cell);

        cell.store()
            .read()
            .verify_ledger()
            .expect("hr + loc-eg ledger verifies");
    }

    // ---------------------------------------------------------------- Test 4: Core 07 `hr-import`

    fn seed_import_batch_and_candidates(cell: &mut Cell, digest: &str) {
        let mut seed = CellCommit::empty();
        seed.writes.extend([
            put_fact(
                "hr_import.SourceBatch",
                "batch_1",
                &json!({
                    "id": "batch_1", "channel": "spreadsheet_csv",
                    "document_uri": "evidence://imports/attendance.csv",
                    "document_digest": digest, "media_type": "text/csv",
                    "extractor": "norn-hr-import-worker", "extractor_version": "0.1.0",
                    "raw_excerpt": "BRANCH-01,EMP-003,2026-11-22,14:05,",
                    "captured_at": "2026-11-22T20:30:00Z", "status": "reviewing"
                }),
            ),
            put_fact(
                "hr_import.ExtractedCandidate",
                "cand_disputed",
                &json!({
                    "id": "cand_disputed", "target_kind": "attendance_event",
                    "source_record_key": "spreadsheet_csv/BRANCH-01/EMP-003/2026-11-22",
                    "payload_digest": "sha256:aaaa", "confidence_bp": 6200, "source_span": "row:4",
                    "extracted_at": "2026-11-23T08:05:00Z", "observed_at": "2026-11-22T20:30:00Z",
                    "effective_on": "2026-11-22", "dispute_state": "disputed", "status": "staged"
                }),
            ),
            put_fact(
                "hr_import.ExtractedCandidate",
                "cand_open_finding",
                &json!({
                    "id": "cand_open_finding", "target_kind": "attendance_event",
                    "source_record_key": "spreadsheet_csv/BRANCH-01/EMP-004/2026-11-22",
                    "payload_digest": "sha256:cccc", "confidence_bp": 9400, "source_span": "row:5",
                    "extracted_at": "2026-11-23T08:10:00Z", "observed_at": "2026-11-22T20:30:00Z",
                    "effective_on": "2026-11-22", "dispute_state": "clean", "status": "staged"
                }),
            ),
            put_fact(
                "hr_import.ExtractedCandidate",
                "cand_clean",
                &json!({
                    "id": "cand_clean", "target_kind": "attendance_event",
                    "source_record_key": "spreadsheet_csv/BRANCH-01/EMP-003/2026-11-22",
                    "payload_digest": "sha256:bbbb", "confidence_bp": 9850, "source_span": "row:4-fix",
                    "extracted_at": "2026-11-23T09:00:00Z", "observed_at": "2026-11-22T21:00:00Z",
                    "effective_on": "2026-11-22", "dispute_state": "clean", "status": "staged"
                }),
            ),
        ]);
        seed.links.extend([
            add_link(
                "hr_import.SourceBatch",
                "batch_1",
                "unit",
                "org.OrgUnit",
                "unit_main",
            ),
            add_link(
                "hr_import.ExtractedCandidate",
                "cand_disputed",
                "batch",
                "hr_import.SourceBatch",
                "batch_1",
            ),
            add_link(
                "hr_import.ExtractedCandidate",
                "cand_open_finding",
                "batch",
                "hr_import.SourceBatch",
                "batch_1",
            ),
            add_link(
                "hr_import.ExtractedCandidate",
                "cand_clean",
                "batch",
                "hr_import.SourceBatch",
                "batch_1",
            ),
            add_link(
                "hr_import.ExtractedCandidate",
                "cand_clean",
                "supersedes",
                "hr_import.ExtractedCandidate",
                "cand_disputed",
            ),
        ]);
        cell.commit(seed).expect("seed import batch and candidates");
    }

    fn seed_import_evidence(cell: &mut Cell, digest: &str) {
        let mut seed = CellCommit::empty();
        for (ev_id, cand_id) in [
            ("ev_disputed", "cand_disputed"),
            ("ev_open_finding", "cand_open_finding"),
            ("ev_clean", "cand_clean"),
        ] {
            seed.writes.push(put_fact(
                "hr_import.FieldEvidence",
                ev_id,
                &json!({
                    "id": ev_id, "subject_property": "clock_out_at",
                    "subject_type": "hr.AttendanceEvent", "subject_key": cand_id,
                    "document_uri": "evidence://imports/attendance.csv", "document_digest": digest,
                    "media_type": "text/csv", "page": 1, "bbox_x": 0, "bbox_y": 0.25,
                    "bbox_w": 1, "bbox_h": 0.1, "text": "row:4-fix 14:05-22:00",
                    "extractor": "norn-hr-import-worker", "extractor_version": "0.1.0",
                    "confidence_bp": 9850, "source_anchor": "row:4#bbbb", "record_digest": "sha256:ev01"
                }),
            ));
            seed.links.extend([
                add_link(
                    "hr_import.FieldEvidence",
                    ev_id,
                    "batch",
                    "hr_import.SourceBatch",
                    "batch_1",
                ),
                add_link(
                    "hr_import.FieldEvidence",
                    ev_id,
                    "candidate",
                    "hr_import.ExtractedCandidate",
                    cand_id,
                ),
            ]);
        }
        cell.commit(seed).expect("seed import evidence");
    }

    fn assert_review_record_access_denials(fixture: &HrVerticalFixture, cell: &mut Cell) {
        // Untrusted import worker cannot record a human review decision.
        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr_import.review.record",
                &json!({
                    "id": "rev_denied_worker", "candidateId": "cand_clean",
                    "decision": "approve_promotion",
                    "reviewer_user_id": "principal:hr_import_worker",
                    "receipt_digest": "sha256:rev_001",
                    "reason": "Worker attempt", "decided_at": "2026-11-23T09:10:00Z"
                }),
                "principal:hr_import_worker",
                "imp-review-worker-denied",
            ),
            "hr_import_worker must be denied hr_import.review.record",
        );

        // Reviewer cannot forge another principal's reviewer_user_id.
        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr_import.review.record",
                &json!({
                    "id": "rev_denied_impersonation", "candidateId": "cand_clean",
                    "decision": "approve_promotion",
                    "reviewer_user_id": "principal:hr_admin",
                    "receipt_digest": "sha256:rev_001",
                    "reason": "Impersonation attempt", "decided_at": "2026-11-23T09:10:00Z"
                }),
                "principal:hr_reviewer",
                "imp-review-mismatch-denied",
            ),
            "mismatched reviewer_user_id must be denied by AccessIR",
        );
    }

    fn assert_reject_review_blocks_promotion(fixture: &HrVerticalFixture, cell: &mut Cell) {
        // Record a `reject_candidate` ReviewDecision on clean staged `cand_clean` and prove
        // `hr_import.candidate.promote` refuses promotion when only a reject decision exists.
        fixture
            .invoke(
                cell,
                "hr_import.review.record",
                &json!({
                    "id": "rev_clean_reject", "candidateId": "cand_clean",
                    "decision": "reject_candidate",
                    "reviewer_user_id": "principal:hr_reviewer",
                    "receipt_digest": "sha256:rev_rej",
                    "reason": "Initial review flagged discrepancy.",
                    "decided_at": "2026-11-23T09:05:00Z"
                }),
                "principal:hr_reviewer",
                "imp-rev-clean-reject",
            )
            .expect("authorized hr_reviewer records reject_candidate review");

        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_clean", "batchId": "batch_1",
                    "evidenceId": "ev_clean", "reviewDecisionId": "rev_clean_reject"
                }),
                "principal:hr_manager",
                "imp-promote-reject-review-refused",
            ),
            "clean staged candidate with only a reject_candidate ReviewDecision cannot promote",
        );
    }

    fn record_import_reviews_via_action(fixture: &HrVerticalFixture, cell: &mut Cell) {
        assert_review_record_access_denials(fixture, cell);

        // Authenticated hr_reviewer records ReviewDecision for cand_disputed and cand_open_finding.
        for (rev_id, cand_id, receipt, key) in [
            (
                "rev_disputed",
                "cand_disputed",
                "sha256:rev_disp",
                "imp-rev-disp",
            ),
            (
                "rev_open_finding",
                "cand_open_finding",
                "sha256:rev_open",
                "imp-rev-open",
            ),
        ] {
            fixture
                .invoke(
                    cell,
                    "hr_import.review.record",
                    &json!({
                        "id": rev_id, "candidateId": cand_id,
                        "decision": "approve_promotion",
                        "reviewer_user_id": "principal:hr_reviewer",
                        "receipt_digest": receipt,
                        "reason": "Verified against source batch evidence.",
                        "decided_at": "2026-11-23T09:10:00Z"
                    }),
                    "principal:hr_reviewer",
                    key,
                )
                .unwrap_or_else(|e| panic!("authorized hr_reviewer records {rev_id}: {e:?}"));
        }

        // Unreviewed `cand_clean` CANNOT promote before its own `hr_import.review.record`:
        // neither with a missing review nor by borrowing another candidate's ReviewDecision!
        let missing_refusal = fixture
            .invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_clean", "batchId": "batch_1",
                    "evidenceId": "ev_clean", "reviewDecisionId": "rev_clean"
                }),
                "principal:hr_manager",
                "imp-promote-unreviewed-missing-refused",
            )
            .expect_err("unreviewed cand_clean must refuse before review.record");
        assert_eq!(missing_refusal.code(), "action.slice_object_missing");

        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_clean", "batchId": "batch_1",
                    "evidenceId": "ev_clean", "reviewDecisionId": "rev_disputed"
                }),
                "principal:hr_manager",
                "imp-promote-unreviewed-foreign-review-refused",
            ),
            "unreviewed cand_clean cannot borrow another candidate's ReviewDecision",
        );

        assert_reject_review_blocks_promotion(fixture, cell);

        // Now record `rev_clean` (`approve_promotion`) via `hr_import.review.record` so
        // `cand_clean` becomes promotable purely via its linked ReviewDecision!
        fixture
            .invoke(
                cell,
                "hr_import.review.record",
                &json!({
                    "id": "rev_clean", "candidateId": "cand_clean",
                    "decision": "approve_promotion",
                    "reviewer_user_id": "principal:hr_reviewer",
                    "receipt_digest": "sha256:rev_001",
                    "reason": "Verified against source batch evidence.",
                    "decided_at": "2026-11-23T09:10:00Z"
                }),
                "principal:hr_reviewer",
                "imp-rev-clean",
            )
            .expect("authorized hr_reviewer records rev_clean");
    }

    fn seed_import_findings(cell: &mut Cell) {
        let mut seed = CellCommit::empty();
        seed.writes.extend([
            put_fact(
                "hr_import.DataQualityFinding",
                "dqf_conflict",
                &json!({
                    "id": "dqf_conflict", "code": "feed_conflict", "finding_kind": "feed_conflict",
                    "severity": "error", "quarantine_disposition": "disputed",
                    "field_path": "payload_digest", "law_pin": "core.boundary.import_finding",
                    "conflicting_payload_digest": "sha256:9999",
                    "message": "Duplicate source key arrived with conflicting digest.",
                    "resolution_action": "accept_second_as_correction",
                    "resolution_receipt_digest": "sha256:res_001",
                    "resolved_by_user_id": "principal:hr_manager",
                    "resolution_reason": "Supervisor verified corrected clock-out export.",
                    "status": "open"
                }),
            ),
            put_fact(
                "hr_import.DataQualityFinding",
                "dqf_open_blocks_promo",
                &json!({
                    "id": "dqf_open_blocks_promo", "code": "feed_conflict",
                    "finding_kind": "feed_conflict", "severity": "error",
                    "quarantine_disposition": "disputed", "field_path": "payload_digest",
                    "law_pin": "core.boundary.import_finding",
                    "conflicting_payload_digest": "sha256:8888",
                    "message": "Open conflict linked to clean-marked candidate blocks promotion.",
                    "resolution_action": null, "resolution_receipt_digest": null,
                    "resolved_by_user_id": null, "resolution_reason": null, "status": "open"
                }),
            ),
        ]);
        seed.links.extend([
            add_link(
                "hr_import.DataQualityFinding",
                "dqf_conflict",
                "candidate",
                "hr_import.ExtractedCandidate",
                "cand_disputed",
            ),
            add_link(
                "hr_import.DataQualityFinding",
                "dqf_conflict",
                "superseded_by",
                "hr_import.ExtractedCandidate",
                "cand_clean",
            ),
            add_link(
                "hr_import.DataQualityFinding",
                "dqf_open_blocks_promo",
                "candidate",
                "hr_import.ExtractedCandidate",
                "cand_open_finding",
            ),
        ]);
        cell.commit(seed).expect("seed import findings");
    }

    fn assert_import_quarantine_and_guards(fixture: &HrVerticalFixture, cell: &mut Cell) {
        fixture
            .invoke(
                cell,
                "hr_import.candidate.quarantine",
                &json!({"candidateId": "cand_disputed", "batchId": "batch_1"}),
                "principal:hr_import_worker",
                "imp-quar",
            )
            .expect("import worker quarantines disputed candidate");
        assert_eq!(
            status_of(cell, "hr_import.ExtractedCandidate", "cand_disputed"),
            "quarantined"
        );

        assert_permission_denied(
            fixture.invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_clean", "batchId": "batch_1",
                    "evidenceId": "ev_clean", "reviewDecisionId": "rev_clean"
                }),
                "principal:hr_import_worker",
                "imp-worker-promote-denied",
            ),
            "import worker must never promote candidates",
        );

        // `cand_disputed` has its OWN valid FieldEvidence (`ev_disputed`) and ReviewDecision
        // (`rev_disputed`), so link preconditions pass and dispute/status guard genuinely refuses!
        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_disputed", "batchId": "batch_1",
                    "evidenceId": "ev_disputed", "reviewDecisionId": "rev_disputed"
                }),
                "principal:hr_manager",
                "imp-promote-disputed-refused",
            ),
            "disputed/quarantined candidate with valid links cannot promote",
        );

        // LAW-LINK-MANY-1: even a staged `clean` candidate with valid evidence and review approval
        // is REFUSED at commit time when an open linked DataQualityFinding exists!
        assert_action_law_refusal(
            fixture.invoke(
                cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_open_finding", "batchId": "batch_1",
                    "evidenceId": "ev_open_finding", "reviewDecisionId": "rev_open_finding"
                }),
                "principal:hr_manager",
                "imp-promote-open-finding-law-refused",
            ),
            "hr_import.ExtractedCandidate.promoted_requires_clean_review",
            "open linked finding must block promotion via LAW-LINK-MANY-1",
        );

        // Non-overlapping resolution: `hr_import.finding.resolve` refuses supersession disposition.
        assert_precondition_false(
            fixture.invoke(
                cell,
                "hr_import.finding.resolve",
                &json!({"findingId": "dqf_conflict", "candidateId": "cand_disputed"}),
                "principal:hr_manager",
                "imp-resolve-cannot-bypass-supersede",
            ),
            "finding.resolve must refuse accept_second_as_correction disposition",
        );
    }

    fn assert_reopened_cell_matches(fixture: &HrVerticalFixture, path: &Path, cell: Cell) {
        let dump = cell.store().read().dump().expect("dump");
        let head = cell
            .store()
            .read()
            .verify_ledger()
            .expect("ledger verifies");
        drop(cell);
        let reopened = Cell::open(
            path,
            fixture.model(),
            StoreOptions::default(),
            SCOPE_KEY,
            |_: &Command<JsonObject>| panic!("sync"),
        )
        .expect("reopened cell");
        assert_eq!(reopened.store().read().dump().expect("reopened dump"), dump);
        assert_eq!(
            reopened
                .store()
                .read()
                .verify_ledger()
                .expect("reopened ledger"),
            head
        );
    }

    #[test]
    fn hr_import_evidence_conflict_supersession_and_immutable_resolution() {
        let fixture = HrVerticalFixture::load(HR_IMPORT_PACKS);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hr-import.sqlite");
        let mut cell = fixture.open_installed_cell(&path);
        let digest = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        seed_common_org_and_people(&mut cell);
        seed_import_batch_and_candidates(&mut cell, digest);
        seed_import_evidence(&mut cell, digest);
        record_import_reviews_via_action(&fixture, &mut cell);
        seed_import_findings(&mut cell);
        assert_import_quarantine_and_guards(&fixture, &mut cell);

        fixture
            .invoke(
                &mut cell,
                "hr_import.finding.supersede",
                &json!({
                    "findingId": "dqf_conflict", "candidateId": "cand_disputed",
                    "replacementCandidateId": "cand_clean"
                }),
                "principal:hr_manager",
                "imp-finding-supersede",
            )
            .expect("HR manager supersedes FEED-CONFLICT finding");
        assert_eq!(
            status_of(&cell, "hr_import.DataQualityFinding", "dqf_conflict"),
            "superseded"
        );
        assert_immutable_state_write(
            &fixture,
            &cell,
            "hr_import.DataQualityFinding",
            "dqf_conflict",
            "message",
            "tampered after supersession",
            "superseded",
        );
        fixture
            .invoke(
                &mut cell,
                "hr_import.candidate.supersede",
                &json!({
                    "candidateId": "cand_disputed", "replacementCandidateId": "cand_clean",
                    "batchId": "batch_1"
                }),
                "principal:hr_manager",
                "imp-cand-supersede",
            )
            .expect("HR manager supersedes disputed candidate");
        fixture
            .invoke(
                &mut cell,
                "hr_import.candidate.promote",
                &json!({
                    "candidateId": "cand_clean", "batchId": "batch_1",
                    "evidenceId": "ev_clean", "reviewDecisionId": "rev_clean"
                }),
                "principal:hr_manager",
                "imp-cand-promote",
            )
            .expect("HR manager promotes evidence-anchored clean candidate");
        assert_eq!(
            status_of(&cell, "hr_import.ExtractedCandidate", "cand_clean"),
            "promoted"
        );
        assert_immutable_state_write(
            &fixture,
            &cell,
            "hr_import.ExtractedCandidate",
            "cand_clean",
            "source_span",
            "tampered after promotion",
            "promoted",
        );
        assert_reopened_cell_matches(&fixture, &path, cell);
    }

    // ---------------------------------------------------------------- Test 5: PII Markings & Reveal

    fn assert_gov_id_reveal_and_declassify(fixture: &HrVerticalFixture, cell: &mut Cell) {
        let catalog = marking_catalog_from_artifacts(&fixture.artifacts);
        let gov_id_policy = &catalog
            .get(&MarkingId("pii.gov_id".to_owned()))
            .unwrap()
            .mask;
        assert_eq!(
            fixture
                .principals
                .decide_reveal("principal:branch_manager", "pii.gov_id"),
            PdpDecision::Deny(DenyReason::Access)
        );
        assert_eq!(
            mask_text("29805150101234", gov_id_policy).as_deref(),
            Some("[REDACTED]1234")
        );
        assert_eq!(
            fixture
                .principals
                .decide_reveal("principal:hr_admin", "pii.gov_id"),
            PdpDecision::Allow
        );

        cell.record_reveal(
            "reveal-ada-gov-id-1",
            &MarkingReveal {
                object_type: "hr.Person".to_owned(),
                key: "person_ada".to_owned(),
                property: "national_id".to_owned(),
                version: 1,
                markings: vec!["pii.gov_id".to_owned()],
                actor: "principal:hr_admin".to_owned(),
                purpose: "compliance audit".to_owned(),
            },
        )
        .expect("audited reveal receipt commits");
        let reveal_entry = cell
            .store()
            .read()
            .ledger(0)
            .expect("ledger")
            .into_iter()
            .find(|e| {
                e.event.get("type") == Some(&JsonValue::String("marking.revealed".to_owned()))
            })
            .expect("marking.revealed entry");
        assert!(
            !serde_json::to_string(&reveal_entry.event)
                .unwrap()
                .contains("29805150101234")
        );

        cell.declassify_markings(
            "declassify-ada-gov-id-1",
            MarkingDeclassification {
                object_type: "hr.Person".to_owned(),
                key: "person_ada".to_owned(),
                property: "national_id".to_owned(),
                expected_version: 1,
                markings: vec!["pii.gov_id".to_owned()],
                actor: "principal:hr_admin".to_owned(),
                reason: "synthetic test fixture declassification".to_owned(),
            },
        )
        .expect("exact-version declassification commits");
        let current = cell
            .store()
            .read()
            .effective_markings(MAIN_WORLD, "hr.Person", "person_ada", None)
            .unwrap()
            .unwrap();
        let v1 = cell
            .store()
            .read()
            .effective_markings(MAIN_WORLD, "hr.Person", "person_ada", Some(1))
            .unwrap()
            .unwrap();
        assert_eq!(current["national_id"], Vec::<String>::new());
        assert_eq!(v1["national_id"], vec!["pii.gov_id".to_owned()]);
    }

    #[test]
    fn hr_pii_markings_persist_and_mask_in_real_cell() {
        let fixture = HrVerticalFixture::load(ALL_HR_PACKS);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hr-markings.sqlite");
        let mut cell = fixture.open_installed_cell(&path);
        seed_common_org_and_people(&mut cell);
        commit_single_with_links(
        &mut cell,
        "hr_import.SourceBatch",
        "batch_wa_1",
        &json!({
            "id": "batch_wa_1", "channel": "whatsapp_export",
            "document_uri": "evidence://chat/wa-1.txt",
            "document_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "media_type": "text/plain", "extractor": "norn-hr-import-worker",
            "extractor_version": "0.1.0", "raw_excerpt": "Shift confirmation excerpt",
            "captured_at": "2026-11-22T16:10:00Z", "status": "received"
        }),
        &[("unit", "org.OrgUnit", "unit_main")],
    )
    .expect("commit marked SourceBatch");

        let person_markings = cell
            .store()
            .read()
            .effective_markings(MAIN_WORLD, "hr.Person", "person_ada", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            person_markings["national_id"],
            vec!["pii.gov_id".to_owned()]
        );
        assert_eq!(person_markings["display_name"], Vec::<String>::new());
        let batch_markings = cell
            .store()
            .read()
            .effective_markings(MAIN_WORLD, "hr_import.SourceBatch", "batch_wa_1", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            batch_markings["raw_excerpt"],
            vec!["pii.raw_chat_media".to_owned()]
        );

        assert_gov_id_reveal_and_declassify(&fixture, &mut cell);
    }
}
