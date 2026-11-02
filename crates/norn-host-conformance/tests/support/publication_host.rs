//! Real native receipt setup; no publication effects are applied here.
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "synthetic host fixture"
)]
use super::{
    policy::{Checks, docs, export, proposal},
    source,
};
use norn_cell::{CommitBatch, EffectRow, FactWriteRow, OutboxCommand, SqliteCell};
use norn_ir::{
    RuntimeObject, RuntimeValue as Value,
    function::{Digest, FunctionInvocation, FunctionReceipt},
};
use norn_kernel::result_reconciliation::{Completion, DispatchedCommand, ResultChecks};
use norn_kernel::{
    CellHost, Command, CommandId, FreshnessPolicy, Generation, IdempotencyKey, LogicalTime,
};
use norn_metrics::producer::ForecastExport;
pub(crate) type Cell = SqliteCell<norn_kernel::FactEvent, RuntimeObject, RuntimeObject>;
pub(crate) struct Host {
    pub(crate) cell: Cell,
    pub(crate) receipt: FunctionReceipt,
    pub(crate) reference: Digest,
    pub(crate) prepared: norn_metrics::host::ForecastSummary,
    pub(crate) model: norn_cell::Model,
}
fn dispatch(bulk: bool, fresh: bool) -> (source::SourceWorld, Checks, DispatchedCommand) {
    let mut world = source::world(bulk, Some(proposal));
    assert_eq!(world.evidence.len(), 128);
    let export = export(&world, fresh);
    let (input, blob) = export
        .input(source::micros(source::SPLIT))
        .expect("bound input");

    let mut values = proposal(&world.plan);
    values.extend(norn_ir::runtime_object! {"table_version"=>input.table_version.to_string(),"table_digest"=>input.table_digest.0.clone(),"source_digest"=>input.source_digest.0.clone(),"split_at_ms"=>input.split_boundary.div_euclid(1000)}.as_object().expect("proposal pins").clone());
    let checks = Checks(
        norn_checker::function::admit_function_manifest(
            &norn_metrics::host::manifest().expect("manifest"),
        )
        .expect("admit Function"),
    );
    let now = source::millis(source::BULK_AT);
    let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../../fixtures/function/invocation.effectful.json"
    ))
    .expect("generic dispatch oracle");
    invocation.tenant = source::SCOPE.into();
    invocation.manifest = checks.0.digest.clone();
    invocation.deadline_millis = now.checked_add(60_000).expect("deadline");
    invocation.retry.max_attempts = 1;
    invocation.provenance_commit = world.head;
    invocation.inputs = norn_ir::runtime::encode(&input)
        .expect("inputs")
        .as_object()
        .expect("object")
        .clone();
    invocation.blobs = [("table".into(), blob)].into();
    invocation.destination = Some(norn_ir::function::Destination {
        object_type: "core.analytics.Publication".into(),
        key: "forecast".into(),
        expected_owner_version: 2,
    });
    invocation.freshness = norn_ir::function::FreshnessMode::DestinationCas;
    let dispatched = DispatchedCommand::pin(invocation.clone(), checks.0.execution_profile.clone())
        .expect("dispatch");
    let command=Command { id:CommandId(invocation.command_id.clone()),generation:Generation(1),idempotency_key:IdempotencyKey("forecast".into()),freshness:FreshnessPolicy::AppendOnly,deadline:None,payload:norn_ir::runtime_object!{"type"=>"norn.function/v1","invocation"=>norn_ir::runtime::encode(&invocation).expect("invocation")}.as_object().expect("envelope").clone() };
    world.store.commit(&CommitBatch { world:"main".into(),scope:source::SCOPE.into(),logical_time:now,writes:vec![
        FactWriteRow::Put{object_type:"core.analytics.Publication".into(),key:"forecast".into(),values},
        FactWriteRow::Put{object_type:"norn.FunctionCommand".into(),key:command.id.0.clone(),values:norn_ir::runtime_object!{"state"=>norn_ir::runtime::encode(&norn_kernel::result_reconciliation::CommandState::dispatched(&dispatched)).expect("state")}.as_object().expect("state object").clone()},
    ],effects:vec![EffectRow::Enqueue{command:OutboxCommand {command_id:command.id.0.clone(),generation:1,idempotency_key:command.idempotency_key.0.clone(),deadline:None,command:norn_ir::runtime::encode(&command).expect("command")}}],..CommitBatch::default() }).expect("pin and dispatch");
    (world, checks, dispatched)
}
fn native_cell(world: source::SourceWorld, fresh: bool) -> Cell {
    let now = source::millis(source::BULK_AT);
    let source::SourceWorld {
        store,
        rows,
        plan,
        head,
        ..
    } = world;
    let mut cell: SqliteCell<norn_kernel::FactEvent, RuntimeObject, RuntimeObject> =
        SqliteCell::new(
            store,
            source::SCOPE,
            move |command: &Command<RuntimeObject>| {
                let actual: FunctionInvocation =
                    norn_ir::runtime::decode(command.payload.get("invocation").expect("envelope"))
                        .expect("invocation");
                let export = ForecastExport {
                    tenant: source::SCOPE,
                    world: "main",
                    plan: &plan,
                    rows: &rows,
                    version: head,
                    source_digest: Digest::from_hex(source::TABLE_SHA),
                    source_head: head,
                    as_of: source::micros(source::BULK_AT),
                    complete: true,
                    quarantine_count: 0,
                    assumptions_digest: docs(),
                    remedy_digest: docs(),
                    frontier_age_ms: 0,
                    fresh,
                };
                norn_ir::runtime::encode(
                    &norn_metrics::performer::perform_forecast(
                        &actual,
                        &export,
                        "synthetic-native-host",
                        now,
                    )
                    .expect("real perform"),
                )
                .expect("receipt")
                .as_object()
                .expect("object")
                .clone()
            },
        )
        .expect("cell");
    cell.advance_to(LogicalTime::from_millis(now));
    cell
}
pub(crate) fn retained_host(bulk: bool, fresh: bool) -> Host {
    let (world, checks, dispatched) = dispatch(bulk, fresh);
    let model = source::model(&source::artifacts());
    let mut cell = native_cell(world, fresh);
    let performed = cell.perform().expect("perform").expect("result");
    let receipt: FunctionReceipt =
        norn_ir::runtime::decode(&Value::Object(performed.input)).expect("receipt");
    let norn_ir::function::FunctionOutcome::Success(success) = &receipt.outcome else {
        panic!("native Function did not complete: {:?}", receipt.outcome)
    };
    let prepared: norn_metrics::host::ForecastSummary =
        norn_ir::runtime::decode(&Value::Object(success.summary.clone()))
            .expect("actual typed Function result");
    let command_id = CommandId(dispatched.invocation.command_id.clone());
    let reference = cell
        .retain_function_receipt(
            &dispatched,
            &Completion {
                receipt: &receipt,
                command: &command_id,
                generation: Generation(1),
                authenticated_executor: Some("synthetic-native-host"),
            },
            &ResultChecks {
                authorizer: &checks,
                validator: &checks,
            },
        )
        .expect("authenticate and retain actual receipt");
    Host {
        cell,
        receipt,
        reference,
        prepared,
        model,
    }
}
