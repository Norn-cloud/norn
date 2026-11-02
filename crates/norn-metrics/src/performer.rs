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

fn encoded_outcome(
    result: Result<ForecastSummary, ForecastFailure>,
) -> Result<(FunctionOutcome, u64), ForecastFailure> {
    let (code, value) = match result {
        Ok(summary) => (None, norn_ir::runtime::encode(&summary).map_err(failure)?),
        Err(error) => {
            let value = norn_ir::runtime::encode(&error).map_err(failure)?;
            (Some(error.code), value)
        }
    };
    let object = value
        .as_object()
        .cloned()
        .ok_or_else(|| failure("result object"))?;
    let bytes = u64::try_from(
        norn_ir::canonical::canonical_json_of(&object)
            .map_err(failure)?
            .len(),
    )
    .map_err(failure)?;
    let outcome = match code {
        None => FunctionOutcome::Success(SuccessOutcome {
            output_manifest: output_manifest_digest(&[], &object).map_err(failure)?,
            outputs: vec![],
            summary: object,
        }),
        Some(code) => FunctionOutcome::Failure(FailureOutcome {
            code,
            detail: object,
        }),
    };
    Ok((outcome, bytes))
}

fn prepare(
    invocation: &FunctionInvocation,
    export: &ForecastExport<'_>,
    executor: &str,
    now_millis: u64,
) -> Result<
    (
        (norn_checker::function::AdmittedFunction, FunctionOutcome),
        u64,
    ),
    ForecastFailure,
> {
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

/// ANALYTICS-FUNCTION-2: real metered evidence for `CellHost::perform`.
/// RSS is the Linux process peak; invalid temporal evidence leaves scores absent.
/// Oversized reports yield `ResourceExhausted`, without a successful summary.
/// # Errors
/// Invalid scope/dispatch/table pins, metering, or encoding.
pub fn perform_forecast(
    invocation: &FunctionInvocation,
    export: &ForecastExport<'_>,
    executor: &str,
    now_millis: u64,
) -> Result<FunctionReceipt, ForecastFailure> {
    let ((admitted, outcome), resources) =
        measured(|| prepare(invocation, export, executor, now_millis))?;
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

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::expect_used, reason = "bounded native resource scenario")]
mod tests {
    use super::*;
    fn export<'a>(
        plan: &'a crate::temporal::TemporalPlan,
        rows: &'a [crate::temporal::TemporalRow],
        digest: &norn_ir::function::Digest,
        version: u64,
    ) -> ForecastExport<'a> {
        let tenant = "tenant_acme";
        ForecastExport {
            tenant,
            world: "main",
            plan,
            rows,
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
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]
        #[test]
        fn oversized_real_report_returns_a_valid_exhaustion_receipt(holdouts in 160usize..192) {
            let (plan, mut rows) = crate::producer::tests::fixture();
            let held_out = rows.last().expect("holdout").clone();
            for i in 0..holdouts {
                let mut row = held_out.clone();
                row.key = format!("{}-{i}", "x".repeat(240));
                rows.push(row);
            }
            let digest = crate::host::implementation_digest().expect("code");
            let export = export(&plan, &rows, &digest, 1);
            let admitted = norn_checker::function::admit_function_manifest(&crate::host::manifest().expect("manifest")).expect("admission");
            let (input, blob) = export.input(6_000_000).expect("bounded input");
            let mut invocation: FunctionInvocation = serde_json::from_str(include_str!("../../../fixtures/function/invocation.effectful.json")).expect("invocation");
            invocation.manifest = admitted.digest.clone(); invocation.retry.max_attempts = 1; invocation.provenance_commit = 1;
            invocation.inputs = norn_ir::runtime::encode(&input).expect("input").as_object().expect("object").clone();
            invocation.blobs = [("table".into(), blob)].into();
            let result = perform_forecast(&invocation, &export, "unit-host", 10_000).expect("measured attempt");
            proptest::prop_assert!(matches!(result.outcome, FunctionOutcome::ResourceExhausted { resource: ExhaustedResource::Output }), "oversized report must exhaust output");
            proptest::prop_assert!(result.resources.output_bytes > norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES);
            proptest::prop_assert!(norn_checker::function::check_function_receipt(&result, &invocation, &admitted).is_empty());
        }
    }
}
