//! Single-currency clearing benchmark (#1878).
//!
//! One USD clearing book, one cell, one fixed quantity template
//! (`usd_clearing_debit`), and one pinned ISO 20022 `pacs.008.001.10` payment
//! subset profile authored in `fixtures/packs/single-currency-clearing/clearing.ts`
//! and compiled into `fixtures/packs/single-currency-clearing/artifacts/`.
//!
//! Covers:
//! - Static admission & analyzers (`structural`): Ring-0 admission with REA
//!   core base (`core.Hold`, `core.LedgerEffect`, `core.EconomicEvent`),
//!   `RED-TEAM-1` (asserting completeness plus the three control classes), and
//!   `LAW-HONESTY-1`.
//! - Compiled Cedar `AccessIr` enforcement (`runtime-enforced`): role and scope
//!   denials, pack-wide transition guards (`transition_guards(actions.values())`),
//!   and maker-checker self-approval refusal via `independentOf` even when the
//!   principal holds both `maker` and `checker` roles.
//! - Payment-to-ledger binding & REA settlement path (`runtime-enforced`):
//!   `ReserveFunds` enforces `input.amount_minor === slice.payment.amount_minor`,
//!   `SettlePayment` enforces `input.reservation_id === slice.payment.reservation_id`,
//!   single-turn atomic `acceptReceipt` + `quantityEffect: post_reservation`,
//!   and runtime `clearing.LiquidityHold` + `clearing.ClearingEntry` creation
//!   (including non-USD entry law refusal).
//! - Adversarial clearing suite (`runtime-enforced`): AML `hit`, failed
//!   `FunctionOutcome::Failure` receipt, wrong run, wrong generation, revoked
//!   command authority, `synchronousRule` + `acceptReceipt`, isolated replay of
//!   a consumed receipt against a fresh reserved payment, single-entry stale
//!   balance version, both hold-expiry vs capture race orderings (with the
//!   screening receipt kept live), and orderly disk reopen + idempotent retry —
//!   asserting exact refusal variants and byte-for-byte unchanged DB state.
//!
//! Honest evidence boundary: maker-checker control, Cedar role/scope enforcement,
//! payment-to-debit binding, REA hold/entry recording, and receipt-backed
//! reservation capture integrity over a pinned `pacs.008.001.10` subset; makes
//! no claim of full ISO 20022 XSD conformance, external message mapping,
//! crash-before-fsync recovery, or organizational SOX 404 audit coverage.
#[cfg(feature = "native")]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "single-currency clearing benchmark fixtures and multi-step scenario assertions"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use norn_cell::function_receipt::{
        FUNCTION_COMMAND_TYPE, FUNCTION_PAYLOAD_TYPE, ReceiptIntakeError,
    };
    use norn_cell::{CellError, MAIN_WORLD, Model, SqliteCell, StoreOptions};
    use norn_checker::law_honesty::{LawEnforcementStatus, classify_law_honesty};
    use norn_checker::red_team::analyze_red_team;
    use norn_checker::{EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt};
    use norn_ir::access::AccessIr;
    use norn_ir::function::{
        CacheOrigin, Digest, ExecutionProvenance, FailureOutcome, FunctionInvocation,
        FunctionOutcome, FunctionReceipt, ResourceUse, SuccessOutcome, output_manifest_digest,
    };
    use norn_ir::quantity_ledger::{
        LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerFundingPolicy, LedgerLayerIr, LedgerLegIr,
        LedgerOntologyArtifactPin, LedgerPackIr, LedgerParameterIr, LedgerParameterValue,
        LedgerTemplateIr, LedgerUnitIr, LinearAmountIr, LinearTermIr, QUANTITY_CELL_VERSION,
        QuantityOperation, QuantityRequest,
    };
    use norn_ir::{
        ActionRoot, ActionRuleRef, ArtifactEnvelope, ArtifactKind, DiagnosticSeverity,
        InterfaceDef, ObjectTypeDef, PropertyTypeDef, ResourceLimitProfile, RuntimeValue,
    };
    use norn_kernel::authz::{
        AccessRequest as PdpRequest, ActorKind, AuthzSchema, Binding, CapabilityGrant, Pdp,
        PdpDecision, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal,
        compile_all_projected,
    };
    use norn_kernel::cell_host::{
        FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue,
    };
    use norn_kernel::pack_mbt_support::cedar_projection::{
        project_cedar_schema_with_catalog, project_host_access_schema,
    };
    use norn_kernel::quantity::{QuantityError, QuantityKey};
    use norn_kernel::receipt_binding::ReceiptRefusal;
    use norn_kernel::result_reconciliation::{
        CommandState, Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator,
        ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial,
    };
    use norn_kernel::{
        ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
        ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit,
        CellHost as _, Command, DecisionRefusal, DecisionTurn, Effect, FactEvent, Generation,
        IdempotencyKey, LogicalTime, TransitionGuard, World, commit_action, transition_guards,
    };
    use serde_json::{Value as Json, json};

    type Sqlite = SqliteCell<FactEvent, JsonObject, JsonObject>;

    const ORG: &str = "org_clear";
    const OTHER_ORG: &str = "org_other";
    const ENV: &str = "dev";
    const SCOPE: &str = "org_clear/dev";
    const READ_ACTION: &str = "norn.read";
    const PAYMENT: &str = "clearing.PaymentInstruction";
    const HOLD: &str = "clearing.LiquidityHold";
    const ENTRY: &str = "clearing.ClearingEntry";
    const LEDGER_ID: &str = "clearing_usd";
    const EXECUTOR: &str = "pool.aml-screening";
    const AUTHORITY_ID: &str = "svc.aml-screening";
    const MANIFEST_DIGEST: &str =
        "sha256:4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a";
    const T0: u64 = 1_793_702_400_000;
    const TTL_MS: u64 = 300_000;

    struct ClearingPack {
        base_raw: Vec<RuntimeValue>,
        pack_raw: Vec<RuntimeValue>,
        envelopes: Vec<ArtifactEnvelope>,
        object_types: Vec<ObjectTypeDef>,
        property_types: Vec<PropertyTypeDef>,
        interfaces: Vec<InterfaceDef>,
        actions: BTreeMap<String, ActionRoot>,
        guards: Vec<TransitionGuard>,
        laws: Vec<ArtifactEnvelope>,
        pdp: Arc<Pdp>,
    }

    fn scope_key(org: &str, env: &str) -> ScopeKey {
        ScopeKey {
            org: org.into(),
            environment: Some(env.into()),
            tenant: None,
            workspace: None,
            business_unit: None,
        }
    }

    fn scope_key_from_map(scope: &BTreeMap<String, String>) -> ScopeKey {
        ScopeKey {
            org: scope.get("orgId").cloned().unwrap_or_default(),
            environment: scope.get("environmentId").cloned(),
            tenant: None,
            workspace: None,
            business_unit: None,
        }
    }

    fn principal_roles(actor: &str) -> Option<Vec<String>> {
        match actor {
            "maker_1" => Some(vec!["maker".into()]),
            "checker_1" => Some(vec!["checker".into()]),
            "maker_checker" => Some(vec!["maker".into(), "checker".into()]),
            "op_1" => Some(vec!["clearing_operator".into()]),
            _ => None,
        }
    }

    fn build_clearing_pdp(
        object_types: &[ObjectTypeDef],
        action_roots: &[ActionRoot],
        policies: &[AccessIr],
    ) -> Arc<Pdp> {
        let projection = project_cedar_schema_with_catalog(object_types, &[], &[], action_roots);
        let host_schema = project_host_access_schema(object_types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
                .expect("projected Cedar schema");
        let policy_set = compile_all_projected(&schema, policies, &projection)
            .expect("compile AccessIr to Cedar");
        let home = scope_key(ORG, ENV);
        let all_caps: Vec<String> = action_roots
            .iter()
            .map(|a| a.action_id.clone())
            .chain([READ_ACTION.to_owned()])
            .collect();
        let grants: Vec<CapabilityGrant> = ["maker_1", "checker_1", "maker_checker", "op_1"]
            .into_iter()
            .map(|subject| CapabilityGrant {
                subject_id: subject.to_owned(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(home.clone()),
                capabilities: all_caps.clone(),
                binding: None,
            })
            .collect();
        Arc::new(Pdp::new(policy_set, &grants).expect("build Cedar PDP"))
    }

    fn load_clearing_pack() -> ClearingPack {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let core_dir = manifest_dir.join("../../registry/core/artifacts");
        let base_files = [
            "std.value_types.entity.json",
            "core.value_types.entity.json",
            "core.Agent.interface.json",
            "core.Resource.interface.json",
            "core.Commitment.interface.json",
            "core.Hold.interface.json",
            "core.EconomicEvent.interface.json",
            "core.LedgerEffect.interface.json",
        ];
        let mut base_raw = Vec::with_capacity(base_files.len());
        let mut property_types = Vec::new();
        let mut interfaces = Vec::new();
        for file in &base_files {
            let raw = fs::read_to_string(core_dir.join(file)).expect("read core base artifact");
            let val: Json = serde_json::from_str(&raw).expect("parse core base json");
            if let Some(pts) = val.pointer("/payload/propertyTypes") {
                let decoded: Vec<PropertyTypeDef> =
                    serde_json::from_value(pts.clone()).expect("PropertyTypeDef list");
                property_types.extend(decoded);
            }
            if let Some(iface) = val.pointer("/payload/interface") {
                interfaces.push(serde_json::from_value(iface.clone()).expect("InterfaceDef"));
            }
            base_raw.push(RuntimeValue::from_json(val).expect("runtime value"));
        }

        let pack_dir = manifest_dir.join("../../fixtures/packs/single-currency-clearing/artifacts");
        let mut paths: Vec<PathBuf> = fs::read_dir(&pack_dir)
            .expect("clearing artifacts dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        paths.sort();

        let mut pack_raw = Vec::new();
        let mut envelopes = Vec::new();
        let mut object_types = Vec::new();
        let mut actions = BTreeMap::new();
        let mut laws = Vec::new();
        let mut policies = Vec::new();
        for path in paths {
            let raw = fs::read_to_string(&path).expect("read artifact");
            let val: Json = serde_json::from_str(&raw).expect("parse artifact json");
            pack_raw.push(RuntimeValue::from_json(val.clone()).expect("runtime value"));
            let env: ArtifactEnvelope =
                serde_json::from_value(val.clone()).expect("decode envelope");
            let payload = val.get("payload").cloned().unwrap_or(Json::Null);
            match env.kind() {
                ArtifactKind::Entity => {
                    if let Some(ot) = payload.get("objectType") {
                        object_types
                            .push(serde_json::from_value(ot.clone()).expect("ObjectTypeDef"));
                    }
                }
                ArtifactKind::Action => {
                    let root: ActionRoot = serde_json::from_value(payload).expect("ActionRoot");
                    actions.insert(root.action_id.clone(), root);
                }
                ArtifactKind::Law => laws.push(env.clone()),
                ArtifactKind::Policy => {
                    let policy: AccessIr = serde_json::from_value(payload).expect("AccessIr");
                    policies.push(policy);
                }
                ArtifactKind::Relation
                | ArtifactKind::Rule
                | ArtifactKind::Connector
                | ArtifactKind::Widget
                | ArtifactKind::Customization
                | ArtifactKind::Setting
                | ArtifactKind::BusinessPack
                | ArtifactKind::Derived
                | ArtifactKind::SourceSchema
                | ArtifactKind::Mapping
                | ArtifactKind::DefinedConcept
                | ArtifactKind::Labels
                | ArtifactKind::Model => {}
            }
            envelopes.push(env);
        }

        let action_roots: Vec<ActionRoot> = actions.values().cloned().collect();
        let guards = transition_guards(actions.values());
        let pdp = build_clearing_pdp(&object_types, &action_roots, &policies);

        ClearingPack {
            base_raw,
            pack_raw,
            envelopes,
            object_types,
            property_types,
            interfaces,
            actions,
            guards,
            laws,
            pdp,
        }
    }

    fn usd_ledger_pack() -> LedgerPackIr {
        let make_template = |id: &str, from: &str, to: &str| LedgerTemplateIr {
            template_id: id.into(),
            parameters: vec![LedgerParameterIr {
                parameter_id: "amount".into(),
                scale: 2,
                min_unscaled: "1".into(),
                max_unscaled: i128::MAX.to_string(),
            }],
            legs: [("out", from, "-1"), ("in", to, "1")]
                .into_iter()
                .map(|(leg_id, account_id, coeff)| LedgerLegIr {
                    leg_id: leg_id.into(),
                    account_id: account_id.into(),
                    unit_id: "USD".into(),
                    layer: "settlement".into(),
                    amount: LinearAmountIr {
                        result_scale: 2,
                        constant_unscaled: "0".into(),
                        constant_scale: 2,
                        terms: vec![LinearTermIr {
                            parameter_id: "amount".into(),
                            coefficient_unscaled: coeff.into(),
                            coefficient_scale: 0,
                        }],
                    },
                })
                .collect(),
        };
        LedgerPackIr {
            ir_version: LEDGER_PACK_IR_VERSION.into(),
            pack_id: "clearing.usd.book".into(),
            ontology_artifact: LedgerOntologyArtifactPin {
                artifact_id: "core.units".into(),
                artifact_version: "1.0.0".into(),
                artifact_sha256: "b".repeat(64),
            },
            units: vec![LedgerUnitIr {
                unit_id: "USD".into(),
                unit_term_id: "currency.usd".into(),
                quantity_kind_term_id: "kind.currency".into(),
                precision: 2,
            }],
            accounts: ["issuance", "debtor_usd", "creditor_usd"]
                .into_iter()
                .map(|id| LedgerAccountIr {
                    account_id: id.into(),
                    allowed_unit_ids: vec!["USD".into()],
                    funding: LedgerFundingPolicy::Gross,
                })
                .collect(),
            layers: vec![LedgerLayerIr {
                layer_id: "settlement".into(),
            }],
            templates: vec![
                make_template("fund_debtor", "issuance", "debtor_usd"),
                make_template("usd_clearing_debit", "debtor_usd", "creditor_usd"),
            ],
        }
    }

    /// Cedar-backed authorizer governing every action and slice read exclusively
    /// through the compiled `clearing.usd.access.policy` PDP.
    struct ClearingPdpAuthz {
        pdp: Arc<Pdp>,
    }

    impl ClearingPdpAuthz {
        fn decide(
            &self,
            actor: &str,
            sk: ScopeKey,
            action_id: &str,
            object_type: &str,
            id: &str,
        ) -> bool {
            let Some(roles) = principal_roles(actor) else {
                return false;
            };
            let principal =
                VerifiedPrincipal::new(actor, ActorKind::User, None, roles, BTreeMap::new());
            let pdp_req = PdpRequest::new(
                &principal,
                Some(sk.clone()),
                action_id,
                action_id,
                ResourceInput {
                    object_type: object_type.to_owned(),
                    id: id.to_owned(),
                    scope: sk,
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedRead),
            );
            self.pdp.decide(&pdp_req) == PdpDecision::Allow
        }
    }

    impl ActionAuthorizer for ClearingPdpAuthz {
        fn authorize(&self, req: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            let sk = scope_key_from_map(req.scope);
            if self.decide(
                req.actor,
                sk,
                &req.action.action_id,
                req.target.object_type.0.as_str(),
                &req.target.key,
            ) {
                Ok(())
            } else {
                Err(ActionDenial::Forbidden {
                    reason: "cedar clearing policy denied".into(),
                })
            }
        }

        fn authorize_read(&self, req: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
            let sk = scope_key_from_map(req.scope);
            if self.decide(
                req.actor,
                sk,
                READ_ACTION,
                req.target.object_type.0.as_str(),
                &req.target.key,
            ) {
                Ok(())
            } else {
                Err(ActionDenial::Invisible)
            }
        }

        fn authorize_read_type(
            &self,
            req: &ActionReadTypeAuthzRequest<'_>,
        ) -> Result<(), ActionDenial> {
            let sk = scope_key_from_map(req.scope);
            if self.decide(
                req.actor,
                sk,
                READ_ACTION,
                req.object_type.0.as_str(),
                req.object_type.0.as_str(),
            ) {
                Ok(())
            } else {
                Err(ActionDenial::Invisible)
            }
        }
    }

    struct ScreeningContract;
    impl ResultAuthorizer for ScreeningContract {
        fn authorize(&self, req: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
            if req.authority.id == AUTHORITY_ID {
                Ok(())
            } else {
                Err(ResultDenial {
                    reason: "screening authority revoked".into(),
                })
            }
        }
    }
    impl ReceiptValidator for ScreeningContract {
        fn validate(
            &self,
            _: &DispatchedCommand,
            receipt: &FunctionReceipt,
        ) -> Result<(), InvalidReceipt> {
            if receipt.attempt == 1 && matches!(receipt.outcome, FunctionOutcome::Success(_)) {
                Ok(())
            } else {
                Err(InvalidReceipt {
                    reason: "invalid screening receipt".into(),
                })
            }
        }
    }

    struct RevokedAuthority;
    impl ResultAuthorizer for RevokedAuthority {
        fn authorize(&self, _: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
            Err(ResultDenial {
                reason: "screening authority revoked".into(),
            })
        }
    }

    fn sha(ch: char) -> Digest {
        Digest(format!("sha256:{}", ch.to_string().repeat(64)))
    }

    fn screening_dispatch(payment_id: &str, generation: u64) -> DispatchedCommand {
        // Keep the screening command deadline well past the hold TTL (T0 + 2_000 + TTL_MS)
        // so the hold-expiry test exercises hold expiry rather than screening receipt expiry.
        let invocation: FunctionInvocation = serde_json::from_value(json!({
        "tenant": SCOPE,
        "world": "main",
        "authority": { "id": AUTHORITY_ID, "kind": "service" },
        "commandId": format!("{payment_id}/screen/g{generation}"),
        "run": { "id": payment_id, "generation": generation },
        "manifest": MANIFEST_DIGEST,
        "inputs": { "paymentId": payment_id },
        "blobs": {},
        "provenanceCommit": 1,
        "dependency": { "facts": [], "predicates": [], "collections": [] },
        "freshness": "relevant_state",
        "destination": null,
        "deadlineMillis": T0 + TTL_MS * 4,
        "retry": { "maxAttempts": 2, "maxCostUnits": 100 },
        "fencing": { "cancellationGeneration": 0, "token": format!("fence-{payment_id}-{generation}") }
    }))
    .expect("invocation");
        DispatchedCommand::pin(invocation, sha('7')).expect("pin dispatch")
    }

    fn screening_receipt_with_outcome(
        dispatched: &DispatchedCommand,
        outcome: FunctionOutcome,
    ) -> FunctionReceipt {
        FunctionReceipt {
            command_id: dispatched.invocation.command_id.clone(),
            run: dispatched.invocation.run.clone(),
            attempt: 1,
            dispatch: dispatched.dispatch.clone(),
            manifest: dispatched.invocation.manifest.clone(),
            execution_profile: dispatched.execution_profile.clone(),
            input_identity: dispatched.input_identity.clone(),
            dependency_token: dispatched.dependency_token.clone(),
            outcome,
            provenance: ExecutionProvenance {
                executor: EXECUTOR.into(),
                placement: "aml-pool/node-1".into(),
                runtime: sha('5'),
            },
            resources: ResourceUse {
                cpu_millis: 2,
                wall_millis: 5,
                peak_memory_bytes: 0,
                output_bytes: 0,
                fuel: None,
            },
            cache: CacheOrigin::Miss {},
        }
    }

    fn screening_receipt(dispatched: &DispatchedCommand, decision: &str) -> FunctionReceipt {
        let summary = BTreeMap::from([(
            "decision".to_owned(),
            RuntimeValue::String(decision.to_owned()),
        )]);
        let outcome = FunctionOutcome::Success(SuccessOutcome {
            output_manifest: output_manifest_digest(&[], &summary).expect("manifest"),
            outputs: Vec::new(),
            summary,
        });
        screening_receipt_with_outcome(dispatched, outcome)
    }

    fn failed_screening_receipt(dispatched: &DispatchedCommand) -> FunctionReceipt {
        let outcome = FunctionOutcome::Failure(FailureOutcome {
            code: "aml.provider_error".into(),
            detail: BTreeMap::from([(
                "message".to_owned(),
                RuntimeValue::String("upstream sanctions provider error".into()),
            )]),
        });
        screening_receipt_with_outcome(dispatched, outcome)
    }

    fn encode_receipt(receipt: &FunctionReceipt) -> JsonObject {
        let JsonValue::Object(map) = norn_ir::runtime::encode(receipt).expect("encode receipt")
        else {
            panic!("receipt must encode as JSON object");
        };
        map
    }

    fn clearing_model(pack: &ClearingPack) -> Model {
        let mut defs = pack.object_types.clone();
        defs.extend(norn_cell::kernel_record_types());
        defs.push(
            serde_json::from_value(json!({
                "name": FUNCTION_COMMAND_TYPE,
                "properties": [{"id": "state", "type": {"kind": "json-ext"}}]
            }))
            .expect("function command type"),
        );
        Model::from_definitions_with_interfaces(&defs, &pack.property_types, &pack.interfaces)
            .expect("model")
            .with_governing_laws(&pack.laws)
            .expect("governing laws")
    }

    fn open_funded_cell(path: &Path, model: &Model, receipts: &[&FunctionReceipt]) -> Sqlite {
        let answers: BTreeMap<String, JsonObject> = receipts
            .iter()
            .map(|r| (r.command_id.clone(), encode_receipt(r)))
            .collect();
        let mut cell = Sqlite::open(
            path,
            model.clone(),
            StoreOptions::default(),
            SCOPE,
            move |cmd: &Command<JsonObject>| {
                answers
                    .get(&cmd.id.0)
                    .cloned()
                    .unwrap_or_else(|| panic!("missing receipt answer for {}", cmd.id.0))
            },
        )
        .expect("open SqliteCell");
        cell.set_result_authorizer(Some(Box::new(ScreeningContract)));

        let define_req = QuantityRequest {
            version: QUANTITY_CELL_VERSION.into(),
            ledger_id: LEDGER_ID.into(),
            balance_versions: Vec::new(),
            operation: QuantityOperation::Define {
                account_limit: 64,
                pack: usd_ledger_pack(),
                unrestricted_accounts: vec!["issuance".into()],
            },
        };
        cell.commit_quantity(&define_req, "define-usd-book")
            .expect("define USD clearing book");

        let mut fund_req = QuantityRequest {
            version: QUANTITY_CELL_VERSION.into(),
            ledger_id: LEDGER_ID.into(),
            balance_versions: Vec::new(),
            operation: QuantityOperation::Post {
                posting_id: "FUND-1".into(),
                template_id: "fund_debtor".into(),
                parameters: vec![LedgerParameterValue {
                    parameter_id: "amount".into(),
                    scale: 2,
                    unscaled: "1000000".into(),
                }],
            },
        };
        fund_req.balance_versions = cell
            .store()
            .read()
            .quantity_balance_versions(MAIN_WORLD, &fund_req)
            .expect("fund balance versions");
        cell.commit_quantity(&fund_req, "fund-debtor-10k")
            .expect("fund debtor $10,000.00");

        // Seed REA `core.Agent` participants and `core.Resource` liquidity book for required links.
        let obj = |v: Json| -> JsonObject { serde_json::from_value(v).expect("object") };
        let mut seed = CellCommit::empty();
        seed.writes = vec![
            FactWrite {
                key: FactKey::new("clearing.Participant", "BANK_DEBTOR"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: obj(json!({
                        "id": "BANK_DEBTOR",
                        "display_name": "Norn Debtor Bank",
                        "kind": "organization",
                        "bic": "NORNUS33XXX"
                    })),
                },
            },
            FactWrite {
                key: FactKey::new("clearing.Participant", "BANK_CREDITOR"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: obj(json!({
                        "id": "BANK_CREDITOR",
                        "display_name": "Clearing Creditor Bank",
                        "kind": "organization",
                        "bic": "CLRGUS33XXX"
                    })),
                },
            },
            FactWrite {
                key: FactKey::new("clearing.LiquidityBook", "BOOK_USD"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: obj(json!({
                        "id": "BOOK_USD",
                        "resource_code": "USD_CLEARING",
                        "quantity": "10000.00",
                        "unit": null,
                        "currency": "USD",
                        "valuation_method": null
                    })),
                },
            },
        ];
        cell.commit(seed).expect("seed REA participants and book");
        cell
    }

    fn balance_versions_for(cell: &Sqlite, operation: QuantityOperation) -> Json {
        let probe = QuantityRequest {
            version: QUANTITY_CELL_VERSION.into(),
            ledger_id: LEDGER_ID.into(),
            balance_versions: Vec::new(),
            operation,
        };
        let versions = cell
            .store()
            .read()
            .quantity_balance_versions(MAIN_WORLD, &probe)
            .expect("compute exact quantity balance versions");
        serde_json::to_value(versions).expect("encode balance versions")
    }

    fn reserve_versions(cell: &Sqlite, res_id: &str, amount_minor: &str) -> Json {
        balance_versions_for(
            cell,
            QuantityOperation::Reserve {
                reservation_id: res_id.into(),
                template_id: "usd_clearing_debit".into(),
                parameters: vec![LedgerParameterValue {
                    parameter_id: "amount".into(),
                    scale: 2,
                    unscaled: amount_minor.into(),
                }],
                expires_at: T0 + 2_000 + TTL_MS,
            },
        )
    }

    fn settle_versions(cell: &Sqlite, res_id: &str, posting_id: &str) -> Json {
        balance_versions_for(
            cell,
            QuantityOperation::PostReservation {
                reservation_id: res_id.into(),
                posting_id: posting_id.into(),
                resolution: norn_ir::quantity_ledger::QuantityResolution::All,
            },
        )
    }

    fn usd_balance_minor(cell: &Sqlite, account: &str) -> i128 {
        let key = QuantityKey {
            account: account.into(),
            unit: "USD".into(),
            layer: "settlement".into(),
        };
        cell.store()
            .read()
            .quantity_balance(MAIN_WORLD, LEDGER_ID, &key)
            .expect("read balance")
            .posted
            .parse()
            .expect("exact integer posted balance")
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test harness forwards explicit scope, actor, clock, and idempotency key"
    )]
    fn call_clearing_in_scope(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        action: &ActionRoot,
        actor: &str,
        org_id: &str,
        idem: &str,
        now_ms: u64,
        input: Json,
    ) -> Result<ActionOutcome, Box<ActionRefusal<CellError>>> {
        cell.advance_to(LogicalTime::from_millis(now_ms));
        let catalog = model
            .type_catalog()
            .with_governing_laws(model.law_artifacts())
            .with_transition_guards(&pack.guards);
        let Ok(parsed) = serde_json::from_value(input) else {
            return Err(Box::new(ActionRefusal::InvalidInput {
                field: "input".into(),
            }));
        };
        let req = ActionCall {
            action,
            action_artifact_id: &format!("{}.action", action.action_id),
            action_version: "0.1.0",
            scope: BTreeMap::from([
                ("orgId".to_owned(), org_id.to_owned()),
                ("environmentId".to_owned(), ENV.to_owned()),
            ]),
            actor: actor.to_owned(),
            approved_intent: None,
            input: parsed,
            idempotency_key: idem.to_owned(),
            object_type: None,
            turn: DecisionTurn {
                id: idem.to_owned(),
                now: LogicalTime::from_millis(now_ms),
                world: World::Main,
                consumes: None,
            },
            trace: None,
        };
        let authz = ClearingPdpAuthz {
            pdp: Arc::clone(&pack.pdp),
        };
        commit_action(cell, &catalog, &req, &authz).map_err(Box::new)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test harness forwards explicit actor, clock, and idempotency key"
    )]
    fn call_clearing(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        action: &ActionRoot,
        actor: &str,
        idem: &str,
        now_ms: u64,
        input: Json,
    ) -> Result<ActionOutcome, Box<ActionRefusal<CellError>>> {
        call_clearing_in_scope(cell, model, pack, action, actor, ORG, idem, now_ms, input)
    }

    fn enqueue_and_perform_screening(cell: &mut Sqlite, dispatched: &DispatchedCommand) {
        let state = norn_ir::runtime::encode(&CommandState::dispatched(dispatched)).expect("state");
        let invocation = norn_ir::runtime::encode(&dispatched.invocation).expect("invocation");
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(FUNCTION_COMMAND_TYPE, &dispatched.invocation.command_id),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([("state".to_owned(), state)]),
                },
            }],
            effects: vec![Effect::Perform(Command {
                id: dispatched.command_id(),
                generation: dispatched.generation(),
                idempotency_key: IdempotencyKey(format!("{}/1", dispatched.invocation.command_id)),
                freshness: dispatched.freshness().expect("freshness"),
                deadline: None,
                payload: JsonObject::from([
                    (
                        "type".to_owned(),
                        JsonValue::String(FUNCTION_PAYLOAD_TYPE.into()),
                    ),
                    ("invocation".to_owned(), invocation),
                ]),
            })],
            ..CellCommit::empty()
        })
        .expect("enqueue screening command");
        assert!(cell.perform().expect("perform").is_some());
    }

    fn perform_and_retain_screening(
        cell: &mut Sqlite,
        dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Digest {
        enqueue_and_perform_screening(cell, dispatched);
        let command = dispatched.command_id();
        cell.retain_function_receipt(
            dispatched,
            &Completion {
                receipt,
                command: &command,
                generation: dispatched.generation(),
                authenticated_executor: Some(EXECUTOR),
            },
            &ResultChecks {
                authorizer: &ScreeningContract,
                validator: &ScreeningContract,
            },
        )
        .expect("retain authenticated screening receipt")
    }

    fn payment_input(id: &str, amount_str: &str, amount_minor: &str) -> Json {
        json!({
            "id": id,
            "message_profile": "pacs.008.001.10",
            "msg_id": format!("PACS008-{id}"),
            "end_to_end_id": format!("E2E-{id}"),
            "debtor_bic": "NORNUS33XXX",
            "creditor_bic": "CLRGUS33XXX",
            "provider_id": "BANK_DEBTOR",
            "receiver_id": "BANK_CREDITOR",
            "resource_id": "BOOK_USD",
            "action": "transfer_ownership",
            "occurred_at": "2026-11-28T05:00:00Z",
            "quantity": amount_str,
            "amount_minor": amount_minor,
            "unit": null,
            "currency": "USD"
        })
    }

    fn initiate_payment_as(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        actor: &str,
        id: &str,
        amount_str: &str,
        amount_minor: &str,
    ) {
        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.initiate"],
            actor,
            &format!("init-{id}"),
            T0,
            payment_input(id, amount_str, amount_minor),
        )
        .expect("initiate payment");
    }

    fn initiate_approve_and_reserve(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        payment_id: &str,
        res_id: &str,
        amount_str: &str,
        amount_minor: &str,
    ) {
        initiate_payment_as(
            cell,
            model,
            pack,
            "maker_1",
            payment_id,
            amount_str,
            amount_minor,
        );

        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.approve"],
            "checker_1",
            &format!("appr-{payment_id}"),
            T0 + 1_000,
            json!({ "id": payment_id }),
        )
        .expect("independent checker approves payment");

        let versions = reserve_versions(cell, res_id, amount_minor);
        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.reserve"],
            "op_1",
            &format!("res-{payment_id}"),
            T0 + 2_000,
            json!({
                "id": payment_id,
                "reservation_id": res_id,
                "amount_minor": amount_minor,
                "balance_versions": versions
            }),
        )
        .expect("reserve USD liquidity");
    }

    fn settle_input(
        id: &str,
        receipt: &str,
        res_id: &str,
        posting_id: &str,
        versions: &Json,
    ) -> Json {
        json!({
            "id": id,
            "screening_receipt": receipt,
            "reservation_id": res_id,
            "posting_id": posting_id,
            "balance_versions": versions
        })
    }

    #[test]
    fn clearing_pack_passes_admission_red_team_and_law_honesty() {
        let pack = load_clearing_pack();
        let mut all = pack.base_raw.clone();
        all.extend(pack.pack_raw.clone());
        let registry = BTreeSet::new();
        let ctx = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let sources = ReceiptSources::new(&all, &[]).expect("receipt sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &ctx,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("Ring-0 admission");
        let errors: Vec<_> = checked
            .diagnostics
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "expected 0 Ring-0 errors, got {errors:#?}"
        );
        assert!(checked.receipt.admitted);

        // RED-TEAM-1 (structural): complete evaluation (0 decode/evaluation errors) and
        // 0 findings across the three control classes (scope confinement, unauthenticated
        // settlements, self-ratifiable transitions). Descriptive identifier/routing fields
        // (e.g. debtor_bic, creditor_bic, end_to_end_id) are intentionally unconstrained
        // by laws and therefore outside the three asserted control classes.
        let report = analyze_red_team(&pack.envelopes);
        assert!(report.is_complete(), "{report:#?}");
        assert!(report.decode_errors.is_empty(), "{report:#?}");
        assert!(report.evaluation_errors.is_empty(), "{report:#?}");
        assert!(report.scope_confinement.is_empty(), "{report:#?}");
        assert!(report.unauthenticated_settlements.is_empty(), "{report:#?}");
        assert!(report.self_ratifiable_transitions.is_empty(), "{report:#?}");

        // LAW-HONESTY-1 (structural): all 3 clearing laws are runtime-enforced.
        let honesty = classify_law_honesty(&pack.envelopes);
        assert_eq!(honesty.len(), 3);
        for entry in &honesty {
            assert_eq!(entry.enforcement, LawEnforcementStatus::Enforced);
        }
    }

    fn assert_reserve_binding_and_record_hold(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
    ) {
        // Blocker witness 1: ReserveFunds refuses an amount_minor different from the
        // approved payment's immutable amount_minor, leaving DB state untouched.
        let before_bad_reserve = cell.store().read().dump().expect("dump before bad reserve");
        let wrong_vers = reserve_versions(cell, "RES-BAD-AMT", "999999");
        let bad_amt_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.reserve"],
            "op_1",
            "res-pmt-1-wrong-amt",
            T0 + 1_500,
            json!({
                "id": "PMT-1",
                "reservation_id": "RES-BAD-AMT",
                "amount_minor": "999999",
                "balance_versions": wrong_vers
            }),
        )
        .expect_err("reserve must refuse mismatched amount_minor");
        assert!(
            matches!(*bad_amt_err, ActionRefusal::PreconditionFailed { .. }),
            "expected PreconditionFailed on amount_minor mismatch, got {bad_amt_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after bad reserve"),
            before_bad_reserve
        );

        let res_vers = reserve_versions(cell, "RES-1", "250000");
        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.reserve"],
            "op_1",
            "res-pmt-1",
            T0 + 2_000,
            json!({
                "id": "PMT-1",
                "reservation_id": "RES-1",
                "amount_minor": "250000",
                "balance_versions": res_vers
            }),
        )
        .expect("reserve matching amount_minor");

        // Exercise REA `clearing.LiquidityHold` (`core.Hold`) bound to the reserved payment.
        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.hold.record"],
            "op_1",
            "hold-res-1",
            T0 + 2_500,
            json!({
                "id": "RES-1",
                "payment_id": "PMT-1",
                "resource_id": "BOOK_USD",
                "holder_id": "BANK_DEBTOR",
                "quantity": "2500.00",
                "unit": null,
                "currency": "USD",
                "expires_at": "2026-11-28T05:05:02Z"
            }),
        )
        .expect("record REA LiquidityHold");
        let hold_row = cell.load(&FactKey::new(HOLD, "RES-1")).expect("load hold");
        assert_eq!(
            hold_row
                .values
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(RuntimeValue::as_str),
            Some("open")
        );
    }

    fn assert_settle_binding_and_commit(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        digest: &Digest,
    ) -> Json {
        // Blocker witness 2: SettlePayment refuses a caller-supplied reservation_id
        // that disagrees with the payment's stored reservation_id.
        let before_bad_settle = cell.store().read().dump().expect("dump before bad settle");
        let versions = settle_versions(cell, "RES-1", "POST-1");
        let bad_res_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-pmt-1-wrong-res",
            T0 + 4_500,
            settle_input("PMT-1", &digest.0, "RES-OTHER", "POST-1", &versions),
        )
        .expect_err("settle must refuse mismatched reservation_id");
        assert!(
            matches!(*bad_res_err, ActionRefusal::PreconditionFailed { .. }),
            "expected PreconditionFailed on reservation_id mismatch, got {bad_res_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after bad settle"),
            before_bad_settle
        );

        let outcome = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-pmt-1",
            T0 + 5_000,
            settle_input("PMT-1", &digest.0, "RES-1", "POST-1", &versions),
        )
        .expect("single-turn atomic settle with acceptReceipt + post_reservation");
        assert!(matches!(outcome, ActionOutcome::Committed { .. }));

        let row = cell.load(&FactKey::new(PAYMENT, "PMT-1")).expect("load");
        let vals = row.values.expect("PMT-1 values");
        assert_eq!(
            vals.get("status").and_then(RuntimeValue::as_str),
            Some("settled")
        );
        assert_eq!(
            vals.get("message_profile").and_then(RuntimeValue::as_str),
            Some("pacs.008.001.10")
        );
        assert_eq!(
            vals.get("maker_id").and_then(RuntimeValue::as_str),
            Some("maker_1")
        );
        assert_eq!(
            vals.get("screening_decision")
                .and_then(RuntimeValue::as_str),
            Some("clear")
        );
        assert_eq!(
            vals.get("screening_digest").and_then(RuntimeValue::as_str),
            Some(digest.0.as_str())
        );
        assert_eq!(
            vals.get("posting_id").and_then(RuntimeValue::as_str),
            Some("POST-1")
        );
        assert_eq!(usd_balance_minor(cell, "debtor_usd"), 750_000);
        assert_eq!(usd_balance_minor(cell, "creditor_usd"), 250_000);
        versions
    }

    fn assert_rea_entry_law_and_capture_wins_race(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        versions: &Json,
    ) {
        // Exercise REA `clearing.ClearingEntry` (`core.LedgerEffect`) and its
        // governing USD-only law (`clearing.entry.single_currency_usd`).
        let before_eur = cell.store().read().dump().expect("dump before EUR entry");
        let eur_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.entry.record"],
            "op_1",
            "entry-eur-bad",
            T0 + 5_200,
            json!({
                "id": "ENT-BAD-EUR",
                "event_id": "PMT-1",
                "account_code": "CREDITOR_USD",
                "amount": "2500.00",
                "currency": "EUR"
            }),
        )
        .expect_err("non-USD ClearingEntry must refuse under clearing.entry.single_currency_usd");
        assert!(
            matches!(
                *eur_err,
                ActionRefusal::Decision(DecisionRefusal::Law { .. })
            ),
            "expected Law refusal on EUR ClearingEntry, got {eur_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after EUR entry"),
            before_eur
        );

        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.entry.record"],
            "op_1",
            "entry-usd-1",
            T0 + 5_300,
            json!({
                "id": "ENT-1",
                "event_id": "PMT-1",
                "account_code": "CREDITOR_USD",
                "amount": "2500.00",
                "currency": "USD"
            }),
        )
        .expect("record USD ClearingEntry");
        let entry_row = cell
            .load(&FactKey::new(ENTRY, "ENT-1"))
            .expect("load entry");
        assert_eq!(
            entry_row
                .values
                .as_ref()
                .and_then(|v| v.get("currency"))
                .and_then(RuntimeValue::as_str),
            Some("USD")
        );

        // Hold-expiry race Ordering B (capture wins): after capture succeeded at T0+5_000,
        // attempting to expire RES-1 past the hold TTL refuses with QuantityError::Terminal.
        cell.advance_to(LogicalTime::from_millis(T0 + 2_000 + TTL_MS + 1));
        let mut expire_req = QuantityRequest {
            version: QUANTITY_CELL_VERSION.into(),
            ledger_id: LEDGER_ID.into(),
            balance_versions: Vec::new(),
            operation: QuantityOperation::Expire {
                reservation_id: "RES-1".into(),
            },
        };
        let ver_err = cell
            .store()
            .read()
            .quantity_balance_versions(MAIN_WORLD, &expire_req)
            .expect_err("captured reservation is terminal and has no live expiry versions");
        assert!(
            matches!(ver_err, CellError::Quantity(QuantityError::Terminal)),
            "expected QuantityError::Terminal on captured reservation versions, got {ver_err:?}"
        );
        expire_req.balance_versions =
            serde_json::from_value(versions.clone()).expect("decode settle versions");
        let expire_after_capture = cell
            .commit_quantity(&expire_req, "expire-captured-res-1")
            .expect_err("expiring an already-captured reservation must refuse");
        assert!(
            matches!(
                expire_after_capture,
                CellError::Quantity(QuantityError::Terminal)
            ),
            "expected QuantityError::Terminal when expiring captured reservation, got {expire_after_capture:?}"
        );
        assert_eq!(usd_balance_minor(cell, "debtor_usd"), 750_000);
        assert_eq!(usd_balance_minor(cell, "creditor_usd"), 250_000);
    }

    fn assert_reopen_retry_and_isolated_receipt_replay(
        path: &Path,
        model: &Model,
        pack: &ClearingPack,
        digest: &Digest,
        settle_vers: &Json,
    ) {
        // Orderly disk reopen & idempotent retry (not a crash-before-fsync fault test).
        let mut reopened =
            Sqlite::open(path, model.clone(), StoreOptions::default(), SCOPE, |_| {
                JsonObject::new()
            })
            .expect("reopen disk cell");
        reopened.set_result_authorizer(Some(Box::new(ScreeningContract)));
        reopened
            .store()
            .read()
            .verify_ledger()
            .expect("ledger intact");

        let replayed = call_clearing(
            &mut reopened,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-pmt-1",
            T0 + 6_000,
            settle_input("PMT-1", &digest.0, "RES-1", "POST-1", settle_vers),
        )
        .expect("idempotent retry after disk reopen replays");
        assert!(matches!(replayed, ActionOutcome::Replayed { .. }));
        assert_eq!(usd_balance_minor(&reopened, "debtor_usd"), 750_000);

        // Isolated receipt replay against a second fresh reserved payment (PMT-1B / RES-1B)
        // with live balance versions so only receipt consumption can refuse it.
        initiate_approve_and_reserve(
            &mut reopened,
            model,
            pack,
            "PMT-1B",
            "RES-1B",
            "100.00",
            "10000",
        );
        let before_replay = reopened
            .store()
            .read()
            .dump()
            .expect("dump before receipt replay");
        let fresh_vers = settle_versions(&reopened, "RES-1B", "POST-1B-REPLAY");
        let replay_err = call_clearing(
            &mut reopened,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-pmt-1b-replay-receipt",
            T0 + 7_000,
            settle_input("PMT-1B", &digest.0, "RES-1B", "POST-1B-REPLAY", &fresh_vers),
        )
        .expect_err("replayed screening receipt on fresh payment must refuse");
        assert!(
            matches!(
                *replay_err,
                ActionRefusal::ReceiptRefused {
                    reason: ReceiptRefusal::Replayed
                }
            ),
            "expected ReceiptRefusal::Replayed on fresh reserved payment, got {replay_err:?}"
        );
        assert_eq!(
            reopened
                .store()
                .read()
                .dump()
                .expect("dump after receipt replay"),
            before_replay
        );
        assert_eq!(usd_balance_minor(&reopened, "debtor_usd"), 750_000);
    }

    #[test]
    fn clearing_valid_settlement_binds_payment_exercises_rea_and_reopens_cleanly() {
        let pack = load_clearing_pack();
        let model = clearing_model(&pack);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("clearing.sqlite");

        let dispatched = screening_dispatch("PMT-1", 1);
        let receipt = screening_receipt(&dispatched, "clear");
        let (digest, settle_vers) = {
            let mut cell = open_funded_cell(&path, &model, &[&receipt]);
            initiate_payment_as(
                &mut cell, &model, &pack, "maker_1", "PMT-1", "2500.00", "250000",
            );
            call_clearing(
                &mut cell,
                &model,
                &pack,
                &pack.actions["clearing.payment.approve"],
                "checker_1",
                "appr-pmt-1",
                T0 + 1_000,
                json!({ "id": "PMT-1" }),
            )
            .expect("checker approves PMT-1");
            assert_reserve_binding_and_record_hold(&mut cell, &model, &pack);
            let digest = perform_and_retain_screening(&mut cell, &dispatched, &receipt);
            let versions = assert_settle_binding_and_commit(&mut cell, &model, &pack, &digest);
            assert_rea_entry_law_and_capture_wins_race(&mut cell, &model, &pack, &versions);
            (digest, versions)
        };
        assert_reopen_retry_and_isolated_receipt_replay(
            &path,
            &model,
            &pack,
            &digest,
            &settle_vers,
        );
    }

    fn assert_cedar_role_and_scope_denials(cell: &mut Sqlite, model: &Model, pack: &ClearingPack) {
        let before_role = cell.store().read().dump().expect("dump before role deny");
        let checker_init_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.initiate"],
            "checker_1",
            "cedar-checker-init",
            T0,
            payment_input("PMT-DENY-1", "100.00", "10000"),
        )
        .expect_err("checker role must not initiate payments under Cedar policy");
        assert!(
            matches!(*checker_init_err, ActionRefusal::PermissionDenied { .. }),
            "{checker_init_err:?}"
        );
        let wrong_scope_err = call_clearing_in_scope(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.initiate"],
            "maker_1",
            OTHER_ORG,
            "cedar-wrong-scope-init",
            T0,
            payment_input("PMT-DENY-2", "100.00", "10000"),
        )
        .expect_err(
            "out-of-scope org must be denied by Cedar policy (existence-hiding on slice reads)",
        );
        assert!(
            matches!(*wrong_scope_err, ActionRefusal::TargetNotFound),
            "expected existence-hiding TargetNotFound on cross-org slice denial, got {wrong_scope_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after role deny"),
            before_role
        );
    }

    fn assert_synchronous_rule_and_revoked_authority(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
        digest: &Digest,
    ) {
        // Synchronous rule-writer + acceptReceipt refuses with UnsupportedEffect.
        let before_sync_writer = cell.store().read().dump().expect("dump before rule writer");
        let mut rule_settle = pack.actions["clearing.payment.settle"].clone();
        rule_settle.synchronous_rule = Some(ActionRuleRef {
            artifact_id: "clearing.dummy.rule".into(),
            version: "0.1.0".into(),
        });
        let versions = settle_versions(cell, "RES-2", "POST-2-RULE");
        let rule_err = call_clearing(
            cell,
            model,
            pack,
            &rule_settle,
            "op_1",
            "settle-with-rule",
            T0 + 3_000,
            settle_input("PMT-2", &digest.0, "RES-2", "POST-2-RULE", &versions),
        )
        .expect_err("acceptReceipt + synchronousRule must refuse");
        assert!(
            matches!(*rule_err, ActionRefusal::UnsupportedEffect { .. }),
            "{rule_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after rule writer"),
            before_sync_writer
        );

        // Command-authority recheck refusal on settlement returns ReceiptRefusal::NotAccepted.
        cell.set_result_authorizer(Some(Box::new(RevokedAuthority)));
        let before_revoked = cell.store().read().dump().expect("dump before revoked");
        let versions = settle_versions(cell, "RES-2", "POST-2-REV");
        let revoked_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-revoked",
            T0 + 4_000,
            settle_input("PMT-2", &digest.0, "RES-2", "POST-2-REV", &versions),
        )
        .expect_err("revoked command authority must refuse receipt consumption");
        assert!(
            matches!(
                *revoked_err,
                ActionRefusal::ReceiptRefused {
                    reason: ReceiptRefusal::NotAccepted
                }
            ),
            "expected ReceiptRefusal::NotAccepted on revoked authority, got {revoked_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after revoked"),
            before_revoked
        );
        assert_eq!(usd_balance_minor(cell, "debtor_usd"), 1_000_000);
    }

    fn assert_maker_reserve_denied_then_operator_reserves(
        cell: &mut Sqlite,
        model: &Model,
        pack: &ClearingPack,
    ) {
        let versions = reserve_versions(cell, "RES-2", "100000");
        let maker_res_err = call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.reserve"],
            "maker_1",
            "cedar-maker-res",
            T0 + 1_500,
            json!({
                "id": "PMT-2",
                "reservation_id": "RES-2",
                "amount_minor": "100000",
                "balance_versions": versions
            }),
        )
        .expect_err("maker_1 lacks clearing_operator role in Cedar policy");
        assert!(
            matches!(*maker_res_err, ActionRefusal::PermissionDenied { .. }),
            "{maker_res_err:?}"
        );

        call_clearing(
            cell,
            model,
            pack,
            &pack.actions["clearing.payment.reserve"],
            "op_1",
            "res-pmt-2",
            T0 + 2_000,
            json!({
                "id": "PMT-2",
                "reservation_id": "RES-2",
                "amount_minor": "100000",
                "balance_versions": versions
            }),
        )
        .expect("reserve");
    }

    #[test]
    fn clearing_cedar_policy_self_approval_revoked_authority_and_synchronous_rule() {
        let pack = load_clearing_pack();
        let model = clearing_model(&pack);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("clearing-authz.sqlite");

        let dispatched = screening_dispatch("PMT-2", 1);
        let receipt = screening_receipt(&dispatched, "clear");
        let mut cell = open_funded_cell(&path, &model, &[&receipt]);

        assert_cedar_role_and_scope_denials(&mut cell, &model, &pack);

        // Self-approval refused by `independentOf: ["maker_id"]` (ACTOR-INDEPENDENT-1)
        // even when `maker_checker` holds BOTH `maker` and `checker` roles in Cedar.
        initiate_payment_as(
            &mut cell,
            &model,
            &pack,
            "maker_checker",
            "PMT-2",
            "1000.00",
            "100000",
        );
        let before_self = cell.store().read().dump().expect("dump before self-appr");
        let self_err = call_clearing(
            &mut cell,
            &model,
            &pack,
            &pack.actions["clearing.payment.approve"],
            "maker_checker",
            "self-appr-pmt-2",
            T0 + 500,
            json!({ "id": "PMT-2" }),
        )
        .expect_err("dual-role maker_checker must not self-approve own payment");
        assert!(
            matches!(*self_err, ActionRefusal::PermissionDenied { .. }),
            "{self_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after self-appr"),
            before_self
        );

        let maker_appr_err = call_clearing(
            &mut cell,
            &model,
            &pack,
            &pack.actions["clearing.payment.approve"],
            "maker_1",
            "cedar-maker-appr",
            T0 + 600,
            json!({ "id": "PMT-2" }),
        )
        .expect_err("maker_1 lacks checker role in Cedar policy");
        assert!(
            matches!(*maker_appr_err, ActionRefusal::PermissionDenied { .. }),
            "{maker_appr_err:?}"
        );

        call_clearing(
            &mut cell,
            &model,
            &pack,
            &pack.actions["clearing.payment.approve"],
            "checker_1",
            "appr-pmt-2",
            T0 + 1_000,
            json!({ "id": "PMT-2" }),
        )
        .expect("independent checker approves");

        assert_maker_reserve_denied_then_operator_reserves(&mut cell, &model, &pack);
        let digest = perform_and_retain_screening(&mut cell, &dispatched, &receipt);
        assert_synchronous_rule_and_revoked_authority(&mut cell, &model, &pack, &digest);
    }

    #[derive(Clone, Copy)]
    enum AdversarialCase {
        AmlHit,
        WrongRun,
        StaleBalance,
        ExpiredHold,
    }

    fn assert_expected_adversarial_err(case: AdversarialCase, err: &ActionRefusal<CellError>) {
        match case {
            AdversarialCase::AmlHit => assert!(
                matches!(*err, ActionRefusal::Decision(DecisionRefusal::Law { .. })),
                "expected Law refusal on AML hit, got {err:?}"
            ),
            AdversarialCase::WrongRun => assert!(
                matches!(
                    *err,
                    ActionRefusal::ReceiptRefused {
                        reason: ReceiptRefusal::RunMismatch
                    }
                ),
                "expected ReceiptRefusal::RunMismatch on wrong run, got {err:?}"
            ),
            AdversarialCase::StaleBalance => assert!(
                matches!(
                    *err,
                    ActionRefusal::Decision(DecisionRefusal::Host(CellError::Quantity(
                        QuantityError::StaleVersion { .. }
                    )))
                ),
                "expected QuantityError::StaleVersion on single-entry bump, got {err:?}"
            ),
            AdversarialCase::ExpiredHold => assert!(
                matches!(
                    *err,
                    ActionRefusal::Decision(DecisionRefusal::Host(CellError::Quantity(
                        QuantityError::Expired
                    )))
                ),
                "expected QuantityError::Expired (not receipt expiry) on expired hold, got {err:?}"
            ),
        }
    }

    fn assert_refused_settlement_leaves_state_untouched(
        pack: &ClearingPack,
        model: &Model,
        idem: &str,
        case: AdversarialCase,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("clearing-{idem}.sqlite"));
        let (receipt_payment_id, decision, now_ms) = match case {
            AdversarialCase::AmlHit => ("PMT-3", "hit", T0 + 4_000),
            AdversarialCase::WrongRun => ("PMT-OTHER", "clear", T0 + 5_000),
            AdversarialCase::StaleBalance => ("PMT-3", "clear", T0 + 6_000),
            AdversarialCase::ExpiredHold => ("PMT-3", "clear", T0 + 2_000 + TTL_MS + 1),
        };
        let disp = screening_dispatch(receipt_payment_id, 1);
        let rec = screening_receipt(&disp, decision);

        let mut cell = open_funded_cell(&path, model, &[&rec]);
        initiate_approve_and_reserve(&mut cell, model, pack, "PMT-3", "RES-3", "500.00", "50000");
        let digest = perform_and_retain_screening(&mut cell, &disp, &rec);

        let before = cell.store().read().dump().expect("dump before");
        let posting_id = format!("POST-{idem}");
        let mut versions = settle_versions(&cell, "RES-3", &posting_id);
        if matches!(case, AdversarialCase::StaleBalance) {
            let arr = versions
                .as_array_mut()
                .expect("balance_versions JSON array");
            assert!(
                arr.len() >= 2,
                "expected complete multi-account balance_versions set, got {arr:?}"
            );
            let current = arr[0]["version"].as_u64().expect("u64 version");
            arr[0]["version"] = json!(current.saturating_add(1));
        }

        let err = call_clearing(
            &mut cell,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            idem,
            now_ms,
            settle_input("PMT-3", &digest.0, "RES-3", &posting_id, &versions),
        )
        .expect_err("{idem} must refuse");
        assert_expected_adversarial_err(case, &err);

        assert_eq!(cell.store().read().dump().expect("dump after"), before);
        assert_eq!(usd_balance_minor(&cell, "debtor_usd"), 1_000_000);
        assert_eq!(usd_balance_minor(&cell, "creditor_usd"), 0);
        let row = cell
            .load(&FactKey::new(PAYMENT, "PMT-3"))
            .expect("load PMT-3");
        assert_eq!(
            row.values
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(RuntimeValue::as_str),
            Some("reserved")
        );

        // Hold-expiry race Ordering A (expiry wins): after expired capture refuses above,
        // explicit QuantityOperation::Expire succeeds at the same timestamp.
        if matches!(case, AdversarialCase::ExpiredHold) {
            let mut expire_req = QuantityRequest {
                version: QUANTITY_CELL_VERSION.into(),
                ledger_id: LEDGER_ID.into(),
                balance_versions: Vec::new(),
                operation: QuantityOperation::Expire {
                    reservation_id: "RES-3".into(),
                },
            };
            expire_req.balance_versions = cell
                .store()
                .read()
                .quantity_balance_versions(MAIN_WORLD, &expire_req)
                .expect("expire balance versions");
            cell.commit_quantity(&expire_req, "expire-res-3-after-ttl")
                .expect("expired hold must release cleanly via QuantityOperation::Expire");
            assert_eq!(usd_balance_minor(&cell, "debtor_usd"), 1_000_000);
            assert_eq!(usd_balance_minor(&cell, "creditor_usd"), 0);
        }
    }

    fn assert_failed_outcome_refuses_intake(pack: &ClearingPack, model: &Model) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("clearing-failed-outcome.sqlite");
        let disp = screening_dispatch("PMT-4", 1);
        let failed = failed_screening_receipt(&disp);
        let failed_digest = failed.content_digest().expect("failed receipt digest");

        let mut cell = open_funded_cell(&path, model, &[&failed]);
        initiate_approve_and_reserve(&mut cell, model, pack, "PMT-4", "RES-4", "500.00", "50000");
        enqueue_and_perform_screening(&mut cell, &disp);

        let before = cell
            .store()
            .read()
            .dump()
            .expect("dump before failed intake");
        let command = disp.command_id();
        let intake_err = cell
            .retain_function_receipt(
                &disp,
                &Completion {
                    receipt: &failed,
                    command: &command,
                    generation: disp.generation(),
                    authenticated_executor: Some(EXECUTOR),
                },
                &ResultChecks {
                    authorizer: &ScreeningContract,
                    validator: &ScreeningContract,
                },
            )
            .expect_err("FunctionOutcome::Failure receipt must refuse intake");
        assert!(
            matches!(intake_err, ReceiptIntakeError::Refused { .. }),
            "expected ReceiptIntakeError::Refused on Failure outcome, got {intake_err:?}"
        );
        let versions = settle_versions(&cell, "RES-4", "POST-4-FAIL");
        let settle_err = call_clearing(
            &mut cell,
            model,
            pack,
            &pack.actions["clearing.payment.settle"],
            "op_1",
            "settle-failed-outcome",
            T0 + 4_000,
            settle_input("PMT-4", &failed_digest.0, "RES-4", "POST-4-FAIL", &versions),
        )
        .expect_err("unretained failed receipt must refuse settlement");
        assert!(
            matches!(
                *settle_err,
                ActionRefusal::ReceiptRefused {
                    reason: ReceiptRefusal::Unknown
                }
            ),
            "expected ReceiptRefusal::Unknown for refused Failure receipt, got {settle_err:?}"
        );
        assert_eq!(
            cell.store()
                .read()
                .dump()
                .expect("dump after failed intake"),
            before
        );
    }

    fn assert_wrong_generation_refuses_intake(pack: &ClearingPack, model: &Model) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("clearing-wrong-gen.sqlite");
        let disp = screening_dispatch("PMT-5", 1);
        let rec = screening_receipt(&disp, "clear");

        let mut cell = open_funded_cell(&path, model, &[&rec]);
        initiate_approve_and_reserve(&mut cell, model, pack, "PMT-5", "RES-5", "500.00", "50000");
        enqueue_and_perform_screening(&mut cell, &disp);

        let before = cell.store().read().dump().expect("dump before wrong gen");
        let command = disp.command_id();
        let gen_err = cell
            .retain_function_receipt(
                &disp,
                &Completion {
                    receipt: &rec,
                    command: &command,
                    generation: Generation(2),
                    authenticated_executor: Some(EXECUTOR),
                },
                &ResultChecks {
                    authorizer: &ScreeningContract,
                    validator: &ScreeningContract,
                },
            )
            .expect_err("wrong generation completion must refuse intake");
        assert!(
            matches!(
                gen_err,
                ReceiptIntakeError::DispatchMismatch
                    | ReceiptIntakeError::NotPending
                    | ReceiptIntakeError::Refused { .. }
            ),
            "expected intake generation refusal, got {gen_err:?}"
        );
        assert_eq!(
            cell.store().read().dump().expect("dump after wrong gen"),
            before
        );
    }

    #[test]
    fn clearing_adversarial_settlement_and_intake_refusals_leave_state_untouched() {
        let pack = load_clearing_pack();
        let model = clearing_model(&pack);

        for (idem, case) in [
            ("settle-hit", AdversarialCase::AmlHit),
            ("settle-wrong-run", AdversarialCase::WrongRun),
            ("settle-stale-bal", AdversarialCase::StaleBalance),
            ("settle-expired-hold", AdversarialCase::ExpiredHold),
        ] {
            assert_refused_settlement_leaves_state_untouched(&pack, &model, idem, case);
        }
        assert_failed_outcome_refuses_intake(&pack, &model);
        assert_wrong_generation_refuses_intake(&pack, &model);
    }
}
