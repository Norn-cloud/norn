//! ANALYSIS-PUBLISH-1: real recorded Function evidence enters the governed action.
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::print_stdout, missing_docs, reason = "synthetic publication acceptance")]
#[path = "support/scrap_world.rs"] mod source;
use std::collections::BTreeMap;
use norn_authz::{AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PrincipalInput, ResourceInput, ScopeKey};
use norn_cell::{CommitBatch, EffectRow, FactWriteRow, OutboxCommand, SqliteCell};
use norn_ir::{RuntimeObject, RuntimeValue as Value, function::{Digest,FunctionInvocation,FunctionReceipt}};
use norn_kernel::{ActionAuthorizer,ActionCall,ActionOutcome,CellHost,Command,CommandId,DecisionTurn,FactKey,FreshnessPolicy,Generation,IdempotencyKey,LogicalTime,TypeCatalog,World,commit_action};
use norn_kernel::result_reconciliation::{Completion,DispatchedCommand,InvalidReceipt,ReceiptValidator,ResultAuthorizer,ResultAuthzRequest,ResultChecks,ResultDenial};
use norn_metrics::{producer::ForecastExport};
const REGISTRATION: &[u8] = br#"{"scope":"synthetic descriptive forecast","thresholdGrams":1}"#;
fn docs() -> Digest { Digest::of_json_document(REGISTRATION).expect("declared before fitting") }
fn export(world: &source::SourceWorld) -> ForecastExport<'_> {
    ForecastExport { tenant:source::SCOPE,world:"main",plan:&world.plan,rows:&world.rows,version:world.head,source_digest:Digest::from_hex(source::TABLE_SHA),source_head:world.head,as_of:source::micros(source::BULK_AT),complete:true,quarantine_count:0,assumptions_digest:docs(),remedy_digest:docs(),frontier_age_ms:0,fresh:true }
}
fn proposal() -> RuntimeObject {
    let mut values = RuntimeObject::new();
    for field in ["id","status","table_version","table_digest","source_digest","model_digest","parameters_digest","assumptions_digest","remedy_digest","mode","receipt_digest","result_table_version","result_table_digest","result_source_digest","result_model_digest","result_parameters_digest","result_assumptions_digest","result_remedy_digest","error_unit","result_error_unit"] { values.insert(field.into(),Value::from("")); }
    for field in ["split_at_ms","result_split_at_ms","quarantined_count","frontier_age_ms","max_frontier_age_ms","late_feature_count","latest_training_label_ms","training_count","holdout_count","non_forward_outcome_count"] { values.insert(field.into(),Value::from(0)); }
    for field in ["complete","temporal_leakage","diagnostics_passed"] { values.insert(field.into(),Value::from(false)); }
    values.extend(norn_ir::runtime_object! {"id"=>"forecast","status"=>"proposed","mode"=>"descriptive","holdout_error_nanos"=>Value::Null,"holdout_error_limit_nanos"=>1_000_000_000i64,"error_unit"=>"g","assumptions_digest"=>docs().0.clone(),"remedy_digest"=>docs().0}.as_object().expect("policy").clone());
    values
}
fn payload(id: &str) -> serde_json::Value {
    let artifact = source::artifacts().into_iter().find(|a| a.artifact_id==id).expect("declared artifact");
    serde_json::to_value(artifact).expect("envelope")["payload"].clone()
}
fn reviewer(action: &norn_ir::ActionRoot, model: &norn_cell::Model, authorized: bool) -> impl ActionAuthorizer {
    let types:Vec<_> = model.object_types().cloned().collect(); let interfaces:Vec<_> = model.interfaces().cloned().collect();
    let projection = norn_projections::project_cedar_schema(&types,&interfaces,std::slice::from_ref(action));
    let schema = AuthzSchema::from_projected_cedarschema(&projection.schema).expect("projected schema");
    let access:norn_authz::access::AccessIr = serde_json::from_value(payload("core.analytics.publication.access.policy")).expect("declared AccessIR");
    let policy = norn_authz::compile(&schema,&access).expect("admitted reviewer policy");
    let scope = ScopeKey { org:"org_demo".into(),environment:Some("dev".into()),tenant:None,workspace:None,business_unit:None };
    let grant=CapabilityGrant { subject_id:"reviewer".into(),actor_kind:ActorKind::User,agent_session_id:None,scope:Some(scope.clone()),capabilities:vec![action.action_id.clone()],binding:None };
    let pdp=Pdp::new(policy,&[grant]).expect("real Cedar PDP");
    norn_kernel::authz::PdpActionAuthorizer::new(pdp,move |request: &norn_kernel::ActionAuthzRequest<'_>| Ok(AccessRequest {
        principal:PrincipalInput { subject_id:request.actor.into(),actor_kind:ActorKind::User,agent_session_id:None,roles:if authorized {vec!["AnalyticsReviewer".into()]} else {vec![]},attributes:BTreeMap::new() },
        scope:Some(scope.clone()),capability:request.action.action_id.clone(),action_id:request.action.action_id.clone(),
        resource:ResourceInput { object_type:request.target.object_type.as_str().into(),id:request.target.key.clone(),scope:scope.clone(),attributes:BTreeMap::new() },context:BTreeMap::new(),binding:None,delegation:Delegation::Direct,
    }))
}
struct Checks(norn_checker::function::AdmittedFunction);
impl ResultAuthorizer for Checks {
    fn authorize(&self, request:&ResultAuthzRequest<'_>) -> Result<(),ResultDenial> {
        if request.tenant==source::SCOPE && request.manifest==&self.0.digest { Ok(()) } else { Err(ResultDenial{reason:"test.host_scope".into()}) }
    }
}
impl ReceiptValidator for Checks {
    fn validate(&self, dispatched:&DispatchedCommand, receipt:&FunctionReceipt) -> Result<(),InvalidReceipt> {
        let findings=norn_checker::function::check_function_receipt(receipt,&dispatched.invocation,&self.0);
        if findings.is_empty() {Ok(())} else {Err(InvalidReceipt{reason:format!("{findings:?}")})}
    }
}
#[test]
fn publish_timed_world_and_refuse_bulk_without_partial_effects() {
    let artifacts=source::artifacts();
    assert!(norn_checker::check_artifact_set(&artifacts,&norn_checker::EntityArtifactSetContext{registry:&Default::default(),base:&[],tenant:None}).is_empty(),"admit the actual typed declarations");
    let action:norn_ir::ActionRoot=serde_json::from_value(payload("core.analytics.publish.action")).expect("declared PUBLISH");
    for bulk in [false,true] {
        let mut world=source::world(bulk,Some(proposal()));
        let export=export(&world); let (input,blob)=export.input(source::micros(source::SPLIT)).expect("bound input");
        let prepared=norn_metrics::producer::prepare_forecast(&export,&input).expect("library assessment");
        let mut values=proposal();
        values.extend(norn_ir::runtime_object! {"table_version"=>input.table_version.to_string(),"table_digest"=>input.table_digest.0.clone(),"source_digest"=>input.source_digest.0.clone(),"split_at_ms"=>prepared.split_at_ms,"model_digest"=>prepared.implementation_digest.0.clone(),"parameters_digest"=>prepared.parameters_digest.0.clone()}.as_object().expect("proposal pins").clone());
        let checks=Checks(norn_checker::function::admit_function_manifest(&norn_metrics::host::manifest().expect("manifest")).expect("admit Function"));
        let now=source::millis(source::BULK_AT);
        let mut invocation:FunctionInvocation=serde_json::from_str(include_str!("../../../fixtures/function/invocation.effectful.json")).expect("generic dispatch oracle");
        invocation.tenant=source::SCOPE.into();invocation.manifest=checks.0.digest.clone();invocation.deadline_millis=now.checked_add(60_000).expect("deadline");invocation.retry.max_attempts=1;invocation.provenance_commit=world.head;
        invocation.inputs=norn_ir::runtime::encode(&input).expect("inputs").as_object().expect("object").clone();invocation.blobs=[("table".into(),blob)].into();
        invocation.destination=Some(norn_ir::function::Destination{object_type:"core.analytics.Publication".into(),key:"forecast".into(),expected_owner_version:2});invocation.freshness=norn_ir::function::FreshnessMode::DestinationCas;
        let dispatched=DispatchedCommand::pin(invocation.clone(),checks.0.execution_profile.clone()).expect("dispatch");
        let command=Command { id:CommandId(invocation.command_id.clone()),generation:Generation(1),idempotency_key:IdempotencyKey("forecast".into()),freshness:FreshnessPolicy::AppendOnly,deadline:None,payload:norn_ir::runtime_object!{"type"=>"norn.function/v1","invocation"=>norn_ir::runtime::encode(&invocation).expect("invocation")}.as_object().expect("envelope").clone() };
        world.store.commit(&CommitBatch { world:"main".into(),scope:source::SCOPE.into(),logical_time:now,writes:vec![
            FactWriteRow::Put{object_type:"core.analytics.Publication".into(),key:"forecast".into(),values},
            FactWriteRow::Put{object_type:"norn.FunctionCommand".into(),key:command.id.0.clone(),values:norn_ir::runtime_object!{"state"=>norn_ir::runtime::encode(&norn_kernel::result_reconciliation::CommandState::dispatched(&dispatched)).expect("state")}.as_object().expect("state object").clone()},
        ],effects:vec![EffectRow::Enqueue{command:OutboxCommand {command_id:command.id.0.clone(),generation:1,idempotency_key:command.idempotency_key.0.clone(),deadline:None,command:norn_ir::runtime::encode(&command).expect("command")}}],..CommitBatch::default() }).expect("pin and dispatch");
        let model=source::model(&artifacts);let source::SourceWorld {store,rows,plan,head,..}=world;
        let mut cell:SqliteCell<norn_kernel::FactEvent,RuntimeObject,RuntimeObject>=SqliteCell::new(store,source::SCOPE,move |command:&Command<RuntimeObject>| {
            let actual:FunctionInvocation=norn_ir::runtime::decode(command.payload.get("invocation").expect("envelope")).expect("invocation");
            let export=ForecastExport {tenant:source::SCOPE,world:"main",plan:&plan,rows:&rows,version:head,source_digest:Digest::from_hex(source::TABLE_SHA),source_head:head,as_of:source::micros(source::BULK_AT),complete:true,quarantine_count:0,assumptions_digest:docs(),remedy_digest:docs(),frontier_age_ms:0,fresh:true};
            norn_ir::runtime::encode(&norn_metrics::performer::perform_forecast(&actual,&export,"synthetic-native-host",now).expect("real perform")).expect("receipt").as_object().expect("object").clone()
        }).expect("cell");cell.advance_to(LogicalTime::from_millis(now));
        let performed=cell.perform().expect("perform").expect("result");let receipt:FunctionReceipt=norn_ir::runtime::decode(&Value::Object(performed.input)).expect("receipt");
        let command_id=CommandId(invocation.command_id.clone());
        let reference=cell.retain_function_receipt(&dispatched,&Completion {receipt:&receipt,command:&command_id,generation:Generation(1),authenticated_executor:Some("synthetic-native-host")},&ResultChecks {authorizer:&checks,validator:&checks}).expect("authenticate and retain actual receipt");
        let catalog=TypeCatalog::new(model.object_types());
        let request=ActionCall {action:&action,action_artifact_id:"core.analytics.publish.action",action_version:"0.1.0",scope:BTreeMap::from([("orgId".into(),"org_demo".into()),("environmentId".into(),"dev".into())]),actor:"reviewer".into(),input:norn_ir::runtime_object!{"id"=>"forecast","receipt"=>reference.0.clone()}.as_object().expect("reference input").clone(),idempotency_key:"publish".into(),object_type:None,turn:DecisionTurn{id:"publish".into(),now:LogicalTime::from_millis(now),world:World::Main,consumes:None}};
        let before=cell.store().verified_head().expect("head");
        assert!(commit_action(&mut cell,&catalog,&request,&reviewer(&action,&model,false)).is_err(),"reviewer role is mandatory");
        assert_eq!(cell.store().verified_head().expect("unchanged"),before);
        let outcome=commit_action(&mut cell,&catalog,&request,&reviewer(&action,&model,true));
        if bulk {
            let detail=format!("{:?}",outcome.expect_err("bulk refuses"));
            assert!(detail.contains("core.projection.publication_guard") && detail.contains("temporal_leakage"),"{detail}");
            assert_eq!(cell.store().verified_head().expect("no partial effect"),before);
            assert!(cell.resolve_accepted_receipt(&norn_kernel::receipt_binding::ReceiptReference::parse(&Value::from(reference.0.clone())).expect("reference"),&receipt.manifest).expect("unconsumed").is_some());
        } else {
            assert!(matches!(outcome.expect("governed publication"),ActionOutcome::Committed{..}));
            let published=cell.load(&FactKey::new("core.analytics.Publication","forecast")).expect("publication").values.expect("live");
            assert_eq!(published["status"],Value::from("published"));assert_eq!(published["receipt_digest"],Value::from(reference.0));
            assert_eq!(published["training_count"],Value::from(48));assert_eq!(published["holdout_count"],Value::from(16));
            println!("completed governed publication: {:?}",prepared.rmse_nanos);
        }
    }
}
