//! Preregistered policy and actual Cedar-backed reviewer authority.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "synthetic policy fixture"
)]
use super::source;
use norn_authz::{
    AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PrincipalInput,
    ResourceInput, ScopeKey,
};
use norn_ir::{
    RuntimeObject, RuntimeValue as Value,
    function::{Digest, FunctionReceipt},
};
use norn_kernel::ActionAuthorizer;
use norn_kernel::result_reconciliation::{
    DispatchedCommand, InvalidReceipt, ReceiptValidator, ResultAuthorizer, ResultAuthzRequest,
    ResultDenial,
};
use norn_metrics::producer::ForecastExport;
use std::collections::BTreeMap;
const REGISTRATION: &[u8] = br#"{"scope":"synthetic descriptive forecast","thresholdGrams":1,"independence":"not established for business observations","confounding":"not identified by this synthetic recipe","diagnosticRemedy":"descriptive only; withhold inference","causalClaim":false}"#;
pub(crate) fn docs() -> Digest {
    Digest::of_json_document(REGISTRATION).expect("declared before fitting")
}
pub(crate) fn export(world: &source::SourceWorld, fresh: bool) -> ForecastExport<'_> {
    ForecastExport {
        tenant: source::SCOPE,
        world: "main",
        plan: &world.plan,
        rows: &world.rows,
        version: world.head,
        source_digest: Digest::from_hex(source::TABLE_SHA),
        source_head: world.head,
        as_of: source::micros(source::BULK_AT),
        complete: true,
        quarantine_count: 0,
        assumptions_digest: docs(),
        remedy_digest: docs(),
        frontier_age_ms: 0,
        fresh,
    }
}
pub(crate) fn proposal(plan: &norn_metrics::temporal::TemporalPlan) -> RuntimeObject {
    let mut values = RuntimeObject::new();
    for field in [
        "id",
        "status",
        "table_version",
        "table_digest",
        "source_digest",
        "model_digest",
        "parameters_digest",
        "assumptions_digest",
        "remedy_digest",
        "mode",
        "receipt_digest",
        "result_table_version",
        "result_table_digest",
        "result_source_digest",
        "result_model_digest",
        "result_parameters_digest",
        "result_assumptions_digest",
        "result_remedy_digest",
        "error_unit",
        "result_error_unit",
    ] {
        values.insert(field.into(), Value::from(""));
    }
    for field in [
        "split_at_ms",
        "result_split_at_ms",
        "quarantined_count",
        "frontier_age_ms",
        "max_frontier_age_ms",
        "late_feature_count",
        "latest_training_label_ms",
        "training_count",
        "holdout_count",
        "non_forward_outcome_count",
    ] {
        values.insert(field.into(), Value::from(0));
    }
    for field in [
        "complete",
        "fresh",
        "temporal_leakage",
        "diagnostics_passed",
    ] {
        values.insert(field.into(), Value::from(false));
    }
    values.extend(norn_ir::runtime_object! {"id"=>"forecast","status"=>"proposed","mode"=>"descriptive","holdout_error_nanos"=>Value::Null,"holdout_error_limit_nanos"=>1_000_000_000i64,"error_unit"=>"g","parameters_digest"=>norn_metrics::producer::parameter_digest(plan,source::micros(source::SPLIT)).expect("preregister parameters before holdout").0,"source_digest"=>format!("sha256:{}",source::TABLE_SHA),"model_digest"=>norn_metrics::host::implementation_digest().expect("configured method before observing holdout").0,"split_at_ms"=>i64::try_from(source::millis(source::SPLIT)).expect("split"),"assumptions_digest"=>docs().0,"remedy_digest"=>docs().0}.as_object().expect("policy").clone());
    values
}
pub(crate) fn payload(id: &str) -> serde_json::Value {
    let artifact = source::artifacts()
        .into_iter()
        .find(|a| a.artifact_id == id)
        .expect("declared artifact");
    serde_json::to_value(artifact).expect("envelope")["payload"].clone()
}
pub(crate) fn reviewer(
    action: &norn_ir::ActionRoot,
    model: &norn_cell::Model,
    authorized: bool,
) -> impl ActionAuthorizer {
    let types: Vec<_> = model.object_types().cloned().collect();
    let interfaces: Vec<_> = model.interfaces().cloned().collect();
    let refuse: norn_ir::ActionRoot =
        serde_json::from_value(payload("core.analytics.refuse.action")).expect("declared REFUSE");
    let projection =
        norn_projections::project_cedar_schema(&types, &interfaces, &[action.clone(), refuse]);
    let schema =
        AuthzSchema::from_projected_cedarschema(&projection.schema).expect("projected schema");
    let access: norn_authz::access::AccessIr =
        serde_json::from_value(payload("core.analytics.publication.access.policy"))
            .expect("declared AccessIR");
    let policy = norn_authz::compile(&schema, &access).expect("admitted reviewer policy");
    let scope = ScopeKey {
        org: "org_demo".into(),
        environment: Some("dev".into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    };
    let grant = CapabilityGrant {
        subject_id: "reviewer".into(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(scope.clone()),
        capabilities: vec![action.action_id.clone()],
        binding: None,
    };
    let pdp = Pdp::new(policy, &[grant]).expect("real Cedar PDP");
    norn_kernel::authz::PdpActionAuthorizer::new(
        pdp,
        move |request: &norn_kernel::ActionAuthzRequest<'_>| {
            Ok(AccessRequest {
                principal: PrincipalInput {
                    subject_id: request.actor.into(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    roles: if authorized {
                        vec!["AnalyticsReviewer".into()]
                    } else {
                        vec![]
                    },
                    attributes: BTreeMap::new(),
                },
                scope: Some(scope.clone()),
                capability: request.action.action_id.clone(),
                action_id: request.action.action_id.clone(),
                resource: ResourceInput {
                    object_type: request.target.object_type.as_str().into(),
                    id: request.target.key.clone(),
                    scope: scope.clone(),
                    attributes: BTreeMap::new(),
                },
                context: BTreeMap::new(),
                binding: None,
                delegation: Delegation::Direct,
            })
        },
    )
}
pub(crate) struct Checks(pub(crate) norn_checker::function::AdmittedFunction);
impl ResultAuthorizer for Checks {
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        if request.tenant == source::SCOPE && request.manifest == &self.0.digest {
            Ok(())
        } else {
            Err(ResultDenial {
                reason: "test.host_scope".into(),
            })
        }
    }
}
impl ReceiptValidator for Checks {
    fn validate(
        &self,
        dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        let findings = norn_checker::function::check_function_receipt(
            receipt,
            &dispatched.invocation,
            &self.0,
        );
        if findings.is_empty() {
            Ok(())
        } else {
            Err(InvalidReceipt {
                reason: format!("{findings:?}"),
            })
        }
    }
}

pub(crate) fn admitted_publish() -> norn_ir::ActionRoot {
    let artifacts = source::artifacts();
    let diagnostics = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::default(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        diagnostics.is_empty(),
        "admit the actual typed declarations: {diagnostics:?}"
    );
    let action: norn_ir::ActionRoot =
        serde_json::from_value(payload("core.analytics.publish.action")).expect("declared PUBLISH");
    action
}
