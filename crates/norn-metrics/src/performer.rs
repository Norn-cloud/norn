//! ANALYTICS-FUNCTION-2: native measured execution produces recorded Function evidence.
use norn_ir::{RuntimeValue, function::{CacheOrigin, ExecutionProvenance, ExhaustedResource,
    FailureOutcome, FunctionInvocation, FunctionOutcome, FunctionReceipt, ResourceUse,
    SuccessOutcome, output_manifest_digest}};
use crate::{host::{ForecastFailure, ForecastInput, ForecastSummary}, producer::{ForecastExport, failure, prepare_forecast}};
#[cfg(target_os = "linux")]
fn measured(work: impl FnOnce() -> Result<ForecastSummary, ForecastFailure>)
    -> Result<(Result<ForecastSummary, ForecastFailure>, ResourceUse), ForecastFailure> {
    use nix::sys::{resource::{getrusage, UsageWho}, time::TimeValLike};
    let before = getrusage(UsageWho::RUSAGE_THREAD).map_err(failure)?;
    let start = std::time::Instant::now();
    let result = work();
    let wall_millis = u64::try_from(start.elapsed().as_millis()).map_err(failure)?;
    let after = getrusage(UsageWho::RUSAGE_THREAD).map_err(failure)?;
    let cpu = after.user_time().num_microseconds().checked_add(after.system_time().num_microseconds())
        .and_then(|v| v.checked_sub(before.user_time().num_microseconds()))
        .and_then(|v| v.checked_sub(before.system_time().num_microseconds())).ok_or_else(|| failure("CPU meter overflow"))?;
    let peak = getrusage(UsageWho::RUSAGE_SELF).map_err(failure)?.max_rss();
    Ok((result, ResourceUse { cpu_millis: u64::try_from(cpu).map_err(failure)? / 1000,
        wall_millis, peak_memory_bytes: u64::try_from(peak).map_err(failure)?.checked_mul(1024)
            .ok_or_else(|| failure("RSS meter overflow"))?, output_bytes: 0, fuel: None }))
}

#[cfg(not(target_os = "linux"))]
fn measured(_: impl FnOnce() -> Result<ForecastSummary, ForecastFailure>)
    -> Result<(Result<ForecastSummary, ForecastFailure>, ResourceUse), ForecastFailure> {
    Err(failure("native resource meter unavailable"))
}

/// Execute the real forecast and return its bound receipt for CellHost::perform.
/// ANALYTICS-FUNCTION-2: metadata comes from a host-resolved export, statistics
/// only from norn-metrics, and accounting from the native meter. RSS is the
/// conservative process peak (Linux KiB), rather than isolated model allocation.
/// Late evidence returns an assessment with no fabricated model or score.
/// # Errors
/// Unavailable meter, invalid scope/dispatch/table pins, or receipt encoding.
pub fn perform_forecast(invocation: &FunctionInvocation, export: &ForecastExport<'_>,
    executor: &str, now_millis: u64) -> Result<FunctionReceipt, ForecastFailure> {
    let admitted = norn_checker::function::admit_function_manifest(&crate::host::manifest().map_err(failure)?).map_err(failure)?;
    if invocation.tenant != export.tenant || invocation.world != "main" || export.world != "main"
        || invocation.deadline_millis < now_millis || executor.trim().is_empty()
        || export.source_head > invocation.provenance_commit
        || !norn_checker::function::check_function_invocation(invocation, &admitted).is_empty() {
        return Err(failure("scope or dispatch"));
    }
    let input: ForecastInput = norn_ir::runtime::decode(&RuntimeValue::Object(invocation.inputs.clone())).map_err(failure)?;
    if input.split_boundary.rem_euclid(1000) != 0 { return Err(failure("cell clock split must align to milliseconds")); }
    let (expected, blob) = export.input(input.split_boundary)?;
    if input != expected || invocation.blobs.len() != 1 || invocation.blobs.get("table") != Some(&blob) {
        return Err(failure("table version/digest or split pin"));
    }
    let (result, mut resources) = measured(|| prepare_forecast(export, &input))?;
    let outcome = match result {
        Ok(summary) => {
            let summary = norn_ir::runtime::encode(&summary).map_err(failure)?.as_object().cloned().ok_or_else(|| failure("summary object"))?;
            resources.output_bytes = u64::try_from(norn_ir::canonical::canonical_json_of(&summary).map_err(failure)?.len()).map_err(failure)?;
            FunctionOutcome::Success(SuccessOutcome { output_manifest: output_manifest_digest(&[], &summary).map_err(failure)?, outputs: vec![], summary })
        }
        Err(error) => {
            let detail = norn_ir::runtime::encode(&error).map_err(failure)?.as_object().cloned().ok_or_else(|| failure("error object"))?;
            resources.output_bytes = u64::try_from(norn_ir::canonical::canonical_json_of(&detail).map_err(failure)?.len()).map_err(failure)?;
            FunctionOutcome::Failure(FailureOutcome { code: error.code, detail })
        }
    };
    let bounds = &admitted.manifest.bounds;
    let exhausted = [(resources.cpu_millis, bounds.max_cpu_millis, ExhaustedResource::Cpu),
        (resources.wall_millis, bounds.max_wall_millis, ExhaustedResource::Wall),
        (resources.peak_memory_bytes, bounds.max_memory_bytes, ExhaustedResource::Memory),
        (resources.output_bytes, bounds.max_output_bytes, ExhaustedResource::Output)]
        .into_iter().find(|(used, bound, _)| bound.is_none_or(|limit| *used > limit));
    let outcome = exhausted.map_or(outcome, |(_, _, resource)| FunctionOutcome::ResourceExhausted { resource });
    let receipt = FunctionReceipt { command_id: invocation.command_id.clone(), run: invocation.run.clone(), attempt: 1,
        dispatch: invocation.content_digest().map_err(failure)?, manifest: admitted.digest,
        execution_profile: admitted.execution_profile, input_identity: invocation.input_identity().map_err(failure)?,
        dependency_token: invocation.dependency.digest().map_err(failure)?, outcome,
        provenance: ExecutionProvenance { executor: executor.into(), placement: "native-host".into(), runtime: admitted.manifest.runtime.digest },
        resources, cache: CacheOrigin::Miss {} };
    Ok(receipt)
}
