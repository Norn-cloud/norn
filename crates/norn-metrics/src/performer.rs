//! ANALYTICS-FUNCTION-2: native measured execution produces recorded Function evidence.
use crate::{
    host::{ForecastFailure, ForecastInput, ForecastSummary},
    producer::{ForecastExport, failure, prepare_forecast},
};
use norn_ir::{
    RuntimeValue,
    function::{
        CacheOrigin, ExecutionProvenance, ExhaustedResource, FailureOutcome, FunctionInvocation,
        FunctionOutcome, FunctionReceipt, ResourceUse, SuccessOutcome, output_manifest_digest,
    },
};
#[cfg(target_os = "linux")]
fn measured<T>(
    work: impl FnOnce() -> Result<(T, u64), ForecastFailure>,
) -> Result<(T, ResourceUse), ForecastFailure> {
    use nix::sys::{
        resource::{UsageWho, getrusage},
        time::TimeValLike,
    };
    let before = getrusage(UsageWho::RUSAGE_THREAD).map_err(failure)?;
    let start = std::time::Instant::now();
    let (result, output_bytes) = work()?;
    let wall_millis = u64::try_from(start.elapsed().as_millis()).map_err(failure)?;
    let after = getrusage(UsageWho::RUSAGE_THREAD).map_err(failure)?;
    let cpu = after
        .user_time()
        .num_microseconds()
        .checked_add(after.system_time().num_microseconds())
        .and_then(|v| v.checked_sub(before.user_time().num_microseconds()))
        .and_then(|v| v.checked_sub(before.system_time().num_microseconds()))
        .ok_or_else(|| failure("CPU meter overflow"))?;
    let peak = getrusage(UsageWho::RUSAGE_SELF).map_err(failure)?.max_rss();
    Ok((
        result,
        ResourceUse {
            cpu_millis: u64::try_from(cpu).map_err(failure)? / 1000,
            wall_millis,
            peak_memory_bytes: u64::try_from(peak)
                .map_err(failure)?
                .checked_mul(1024)
                .ok_or_else(|| failure("RSS meter overflow"))?,
            output_bytes,
            fuel: None,
        },
    ))
}

#[cfg(not(target_os = "linux"))]
fn measured<T>(
    _: impl FnOnce() -> Result<(T, u64), ForecastFailure>,
) -> Result<(T, ResourceUse), ForecastFailure> {
    Err(failure("native resource meter unavailable"))
}

fn encoded_outcome(result: Result<ForecastSummary, ForecastFailure>)
    -> Result<(FunctionOutcome, u64), ForecastFailure> {
    Ok(match result {
        Ok(summary) => {
            let summary = norn_ir::runtime::encode(&summary)
                .map_err(failure)?
                .as_object()
                .cloned()
                .ok_or_else(|| failure("summary object"))?;
            let output_bytes = u64::try_from(
                norn_ir::canonical::canonical_json_of(&summary)
                    .map_err(failure)?
                    .len(),
            )
            .map_err(failure)?;
            (FunctionOutcome::Success(SuccessOutcome {
                output_manifest: output_manifest_digest(&[], &summary).map_err(failure)?,
                outputs: vec![],
                summary,
            }), output_bytes)
        }
        Err(error) => {
            let detail = norn_ir::runtime::encode(&error)
                .map_err(failure)?
                .as_object()
                .cloned()
                .ok_or_else(|| failure("error object"))?;
            let output_bytes = u64::try_from(
                norn_ir::canonical::canonical_json_of(&detail)
                    .map_err(failure)?
                    .len(),
            )
            .map_err(failure)?;
            (FunctionOutcome::Failure(FailureOutcome {
                code: error.code,
                detail,
            }), output_bytes)
        }
    })
}

fn prepare(invocation: &FunctionInvocation, export: &ForecastExport<'_>, executor: &str, now_millis: u64)
    -> Result<((norn_checker::function::AdmittedFunction, FunctionOutcome), u64), ForecastFailure> {
    let admitted =
        norn_checker::function::admit_function_manifest(&crate::host::manifest().map_err(failure)?)
            .map_err(failure)?;
    if invocation.tenant != export.tenant
        || invocation.world != "main"
        || export.world != "main"
        || invocation.deadline_millis < now_millis
        || executor.trim().is_empty()
        || export.source_head > invocation.provenance_commit
        || !norn_checker::function::check_function_invocation(invocation, &admitted).is_empty()
    {
        return Err(failure("scope or dispatch"));
    }
    let input: ForecastInput =
        norn_ir::runtime::decode(&RuntimeValue::Object(invocation.inputs.clone()))
            .map_err(failure)?;
    if input.split_boundary.rem_euclid(1000) != 0 {
        return Err(failure("cell clock split must align to milliseconds"));
    }
    let (expected, blob) = export.input(input.split_boundary)?;
    if input != expected
        || invocation.blobs.len() != 1
        || invocation.blobs.get("table") != Some(&blob)
    {
        return Err(failure("table version/digest or split pin"));
    }
    let (outcome, output_bytes) = encoded_outcome(prepare_forecast(export, &input))?;
    Ok(((admitted, outcome), output_bytes))
}

/// Execute the real forecast and return its bound receipt for `CellHost::perform`.
/// ANALYTICS-FUNCTION-2: metadata comes from a host-resolved export, statistics
/// only from norn-metrics, and accounting from the native meter. RSS is the
/// conservative process peak (Linux KiB), rather than isolated model allocation.
/// Late evidence returns an assessment with no fabricated model or score.
/// # Errors
/// Unavailable meter, invalid scope/dispatch/table pins, or receipt encoding.
pub fn perform_forecast(
    invocation: &FunctionInvocation,
    export: &ForecastExport<'_>,
    executor: &str,
    now_millis: u64,
) -> Result<FunctionReceipt, ForecastFailure> {
    let ((admitted, outcome), resources) = measured(|| prepare(invocation, export, executor, now_millis))?;
    let bounds = &admitted.manifest.bounds;
    let exhausted = [
        (
            resources.cpu_millis,
            bounds.max_cpu_millis,
            ExhaustedResource::Cpu,
        ),
        (
            resources.wall_millis,
            bounds.max_wall_millis,
            ExhaustedResource::Wall,
        ),
        (
            resources.peak_memory_bytes,
            bounds.max_memory_bytes,
            ExhaustedResource::Memory,
        ),
        (
            resources.output_bytes,
            bounds.max_output_bytes,
            ExhaustedResource::Output,
        ),
    ]
    .into_iter()
    .find(|(used, bound, _)| bound.is_none_or(|limit| *used > limit));
    let outcome = exhausted.map_or(outcome, |(_, _, resource)| {
        FunctionOutcome::ResourceExhausted { resource }
    });
    let receipt = FunctionReceipt {
        command_id: invocation.command_id.clone(),
        run: invocation.run.clone(),
        attempt: 1,
        dispatch: invocation.content_digest().map_err(failure)?,
        manifest: admitted.digest,
        execution_profile: admitted.execution_profile,
        input_identity: invocation.input_identity().map_err(failure)?,
        dependency_token: invocation.dependency.digest().map_err(failure)?,
        outcome,
        provenance: ExecutionProvenance {
            executor: executor.into(),
            placement: "native-host".into(),
            runtime: admitted.manifest.runtime.digest,
        },
        resources,
        cache: CacheOrigin::Miss {},
    };
    Ok(receipt)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "real native perform-path correspondence scenario"
)]
mod tests {
    use super::*;
    use norn_cell::{
        CellStore, CommitBatch, EffectRow, Model, OutboxCommand, SqliteCell, StoreOptions,
    };
    use norn_kernel::{CellHost, Command, CommandId, FreshnessPolicy, Generation, IdempotencyKey};
    fn export<'a>(plan: &'a crate::temporal::TemporalPlan, rows: &'a [crate::temporal::TemporalRow], digest: &norn_ir::function::Digest, version: u64) -> ForecastExport<'a> {
        let tenant = "tenant_acme";
        ForecastExport {
            tenant,
            world: "main",
            plan: &plan,
            rows: &rows,
            version,
            source_digest: digest.clone(),
            source_head: 1,
            as_of: 8_000_000,
            complete: true,
            quarantine_count: 0,
            fresh: true,
            assumptions_digest: digest.clone(),
            remedy_digest: digest.clone(),
            frontier_age_ms: 0,
        }
    }
    fn enqueue(invocation: &FunctionInvocation) -> CellStore {
        let model = Model::from_definitions(&[], &[]).expect("model");
        let mut store = CellStore::open(std::path::Path::new(":memory:"), model, StoreOptions::default()).expect("store");
        let command = Command {
            id: CommandId(invocation.command_id.clone()),
            generation: Generation(invocation.run.generation),
            idempotency_key: IdempotencyKey("native-forecast".into()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: norn_ir::runtime_object! { "type" => "norn.function/v1", "invocation" => norn_ir::runtime::encode(&invocation).expect("invocation") },
        };
        store
            .commit(&CommitBatch {
                world: "main".into(),
                scope: invocation.tenant.clone(),
                logical_time: 10_000,
                effects: vec![EffectRow::Enqueue {
                    command: OutboxCommand {
                        command_id: command.id.0.clone(),
                        generation: command.generation.0,
                        idempotency_key: command.idempotency_key.0.clone(),
                        deadline: None,
                        command: norn_ir::runtime::encode(&command).expect("command"),
                    },
                }],
                ..CommitBatch::default()
            })
            .expect("enqueue");
        store
    }
    proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn native_report_is_recorded_by_perform_and_wrong_pins_refuse(version in 1u64..1_000_000) {
        let (plan, rows) = crate::producer::tests::fixture();
        let digest = crate::host::implementation_digest().expect("code");
        let tenant = "tenant_acme";
        let admitted = norn_checker::function::admit_function_manifest(
            &crate::host::manifest().expect("manifest"),
        )
        .expect("admit");
        let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
            "../../../fixtures/function/invocation.effectful.json"
        ))
        .expect("dispatch fixture");
        let (input, blob) = export(&plan, &rows, &digest, version).input(6_000_000).expect("export");
        invocation.manifest = admitted.digest.clone();
        invocation.retry.max_attempts = 1;
        invocation.provenance_commit = 1;
        invocation.inputs = norn_ir::runtime::encode(&input)
            .expect("input")
            .as_object()
            .expect("object")
            .clone();
        invocation.blobs = [("table".into(), blob)].into();
        let mut wrong = invocation.clone();
        wrong.tenant = "another-tenant".into();
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        wrong = invocation.clone();
        wrong.inputs.insert("tableVersion".into(), RuntimeValue::from(version.saturating_add(1)));
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        wrong = invocation.clone();
        wrong.blobs.clear();
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        let store = enqueue(&invocation);
        let mut cell: SqliteCell<RuntimeValue, RuntimeValue, FunctionReceipt> =
            SqliteCell::new(store, tenant, move |command: &Command<RuntimeValue>| {
                let actual: FunctionInvocation = norn_ir::runtime::decode(
                    command.payload.get("invocation").expect("host envelope"),
                )
                .expect("invocation");
                let export = export(&plan, &rows, &digest, version);
                perform_forecast(&actual, &export, "unit-host", 10_000).expect("real execution")
            })
            .expect("cell");
        let performed = cell.perform().expect("perform").expect("result");
        assert!(
            norn_checker::function::check_function_receipt(
                &performed.input,
                &invocation,
                &admitted
            )
            .is_empty()
        );
        let recorded: FunctionReceipt = norn_ir::runtime::decode(
            &cell
                .store()
                .read()
                .head_command()
                .expect("head")
                .expect("command")
                .result
                .expect("persisted result"),
        )
        .expect("receipt");
        assert_eq!(recorded, performed.input);
        assert!(recorded.resources.peak_memory_bytes > 0 && recorded.resources.fuel.is_none());
        let FunctionOutcome::Success(success) = recorded.outcome else {
            panic!("native forecast success")
        };
        let summary: ForecastSummary =
            norn_ir::runtime::decode(&RuntimeValue::Object(success.summary))
                .expect("typed summary");
        assert_eq!(summary.rmse_nanos, Some(800_000_000));
        assert_eq!(summary.table_digest, input.table_digest);
        assert_eq!(summary.table_version, version.to_string());
        assert!(cell.perform().expect("recorded retry").is_some());
    }
}
}
