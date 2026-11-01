//! ANALYTICS-FUNCTION-2: actual library execution produces a measured receipt.

use norn_ir::{RuntimeValue, function::{BlobRef, CacheOrigin, Digest, ExecutionProvenance,
    FailureOutcome, FunctionInvocation, FunctionOutcome, FunctionReceipt, ResourceUse,
    SuccessOutcome, output_manifest_digest}};
use crate::{host::{ForecastFailure, ForecastInput, ForecastSummary}, temporal::{TemporalPlan, TemporalRow}};

fn failure(message: impl std::fmt::Debug) -> ForecastFailure {
    ForecastFailure { code: "core.analytics.function".into(), message: format!("{message:?}") }
}

/// Host-owned table and quality metadata. The host must resolve its references
/// through authenticated exports; this type cannot be deserialized from input.
/// Neither construction nor statistical execution grants publication authority.
pub struct ForecastExport<'a> {
    /// Authorized source tenant.
    pub tenant: &'a str,
    /// Authorized source world; only main can perform.
    pub world: &'a str,
    /// Ontology-bound numeric columns.
    pub plan: &'a TemporalPlan,
    /// Exact resolved rows, including authenticated knowledge times/sources.
    pub rows: &'a [TemporalRow],
    /// Immutable export version.
    pub version: u64,
    /// Source population digest.
    pub source_digest: Digest,
    /// Export cursor.
    pub source_head: u64,
    /// Export knowledge cutoff, in signed UTC microseconds.
    pub as_of: i64,
    /// Source coverage, resolved by the host.
    pub complete: bool,
    /// Source quarantine count, resolved by the host.
    pub quarantine_count: u32,
    /// Source frontier freshness, resolved by the host.
    pub fresh: bool,
}

impl ForecastExport<'_> {
    /// Bind the exact host-owned input into scalar pins and one immutable blob.
    /// # Errors
    /// Oversized/noncanonical data or an unrepresentable size.
    pub fn input(&self, boundary: i64) -> Result<(ForecastInput, BlobRef), ForecastFailure> {
        if self.rows.is_empty() || self.rows.len() > 10_000 { return Err(failure("row bound")); }
        let features: Vec<_> = self.plan.features().iter().map(|c| (&c.property, &c.unit)).collect();
        let bytes = norn_ir::canonical::canonical_json_of(&(
            self.tenant, self.world, self.version, &self.source_digest, self.source_head, self.as_of,
            self.plan.object_type(), self.plan.max_age_micros(), features, (&self.plan.outcome().property, &self.plan.outcome().unit),
            self.rows, self.complete, self.quarantine_count, self.fresh,
        )).map_err(failure)?;
        if bytes.len() > 4_194_304 { return Err(failure("input byte bound")); }
        let digest = Digest::of_json_document(bytes.as_slice()).map_err(failure)?;
        let input = ForecastInput { table_version: self.version, table_digest: digest.clone(),
            source_digest: self.source_digest.clone(), source_head: self.source_head, split_boundary: boundary };
        Ok((input, BlobRef { digest, version: Some(self.version.to_string()),
            size_bytes: u64::try_from(bytes.len()).map_err(failure)? }))
    }
}

#[allow(clippy::arithmetic_side_effects, reason = "approximate unit conversion refuses nonfinite or out-of-i64 results")]
fn nanos(value: f64) -> Result<i64, ForecastFailure> {
    let scaled = value * 1_000_000_000.0;
    if value < 0.0 || !scaled.is_finite() { return Err(failure("nonfinite or negative RMSE")); }
    format!("{scaled:.0}").parse().map_err(failure)
}

fn compute(export: &ForecastExport<'_>, input: &ForecastInput, implementation: Digest) -> Result<ForecastSummary, ForecastFailure> {
    let late_features = export.rows.iter().flat_map(|row| row.features.iter().map(move |v| (row.at, v)))
        .filter(|(at, v)| v.recorded_at > *at || v.valid_at > *at).count();
    let training_rows = export.rows.iter().filter(|row| row.at < input.split_boundary).count();
    let late_training_labels = export.rows.iter().filter(|row| row.at < input.split_boundary
        && (row.outcome.recorded_at >= input.split_boundary || row.outcome.valid_at >= input.split_boundary)).count();
    let mut summary = ForecastSummary {
        table_version: input.table_version, table_digest: input.table_digest.clone(), source_digest: input.source_digest.clone(),
        source_head: input.source_head, split_boundary: input.split_boundary, implementation_digest: implementation, alpha: "0.05".into(),
        training_rows: u32::try_from(training_rows).map_err(failure)?,
        holdout_rows: u32::try_from(export.rows.len().saturating_sub(training_rows)).map_err(failure)?,
        late_features: u32::try_from(late_features).map_err(failure)?,
        late_training_labels: u32::try_from(late_training_labels).map_err(failure)?,
        temporal_leakage: late_features != 0 || late_training_labels != 0,
        complete: export.complete, quarantine_count: export.quarantine_count, fresh: export.fresh,
        rmse_nanos: None, baseline_rmse_nanos: None, report: None,
    };
    if !summary.temporal_leakage {
        let table = export.plan.materialize(export.as_of, export.source_head, export.rows.to_vec()).map_err(failure)?;
        let report = crate::analysis::forecast(&table, input.split_boundary).map_err(failure)?;
        summary.rmse_nanos = Some(nanos(report.rmse)?);
        summary.baseline_rmse_nanos = Some(nanos(report.baseline_rmse)?);
        summary.report = Some(norn_ir::runtime::encode(&report).map_err(failure)?);
    }
    Ok(summary)
}

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
    let (expected, blob) = export.input(input.split_boundary)?;
    if input != expected || invocation.blobs.len() != 1 || invocation.blobs.get("table") != Some(&blob) {
        return Err(failure("table version/digest or split pin"));
    }
    let (result, mut resources) = measured(|| compute(export, &input, admitted.manifest.implementation.digest.clone()))?;
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
    let receipt = FunctionReceipt { command_id: invocation.command_id.clone(), run: invocation.run.clone(), attempt: 1,
        dispatch: invocation.content_digest().map_err(failure)?, manifest: admitted.digest,
        execution_profile: admitted.execution_profile, input_identity: invocation.input_identity().map_err(failure)?,
        dependency_token: invocation.dependency.digest().map_err(failure)?, outcome,
        provenance: ExecutionProvenance { executor: executor.into(), placement: "native-host".into(), runtime: admitted.manifest.runtime.digest },
        resources, cache: CacheOrigin::Miss {} };
    Ok(receipt)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::cast_precision_loss, clippy::arithmetic_side_effects, reason = "bounded report-unit property")]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn nonfinite_negative_and_unrepresentable_scores_refuse() {
        for value in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] { assert!(nanos(value).is_err()); }
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn representable_report_units_round_trip(n in 0i32..1_000_000_000) {
            prop_assert_eq!(nanos(f64::from(n) / 1_000_000_000.0).expect("nanos"), i64::from(n));
        }
    }
}
