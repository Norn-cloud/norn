//! ANALYTICS-FUNCTION-INPUT-1: bind actual library reports to exact exports.

use crate::{
    host::{ForecastFailure, ForecastInput, ForecastSummary},
    temporal::{TemporalPlan, TemporalRow},
};
use norn_ir::function::{BlobRef, Digest};

pub(crate) fn failure(message: impl std::fmt::Debug) -> ForecastFailure {
    ForecastFailure {
        code: "core.analytics.function".into(),
        message: format!("{message:?}"),
    }
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
    /// Declared assumptions document, resolved by the host.
    pub assumptions_digest: Digest,
    /// Declared remedy document, resolved by the host.
    pub remedy_digest: Digest,
    /// Source cursor lag in milliseconds, resolved by the host.
    pub frontier_age_ms: u64,
    /// Source frontier freshness, resolved by the host.
    pub fresh: bool,
}

impl ForecastExport<'_> {
    /// Bind the exact host-owned input into scalar pins and one immutable blob.
    /// # Errors
    /// Oversized/noncanonical data or an unrepresentable size.
    pub fn input(&self, boundary: i64) -> Result<(ForecastInput, BlobRef), ForecastFailure> {
        if self.rows.is_empty() || self.rows.len() > 10_000 {
            return Err(failure("row bound"));
        }
        let features: Vec<_> = self
            .plan
            .features()
            .iter()
            .map(|c| (&c.property, &c.unit))
            .collect();
        let bytes = norn_ir::canonical::canonical_json_of(&(
            "norn.analytics.export/v1",
            (
                self.tenant,
                self.world,
                self.version,
                &self.source_digest,
                self.source_head,
                self.as_of,
            ),
            (
                self.plan.object_type(),
                self.plan.max_age_micros(),
                features,
                (&self.plan.outcome().property, &self.plan.outcome().unit),
            ),
            (
                self.complete,
                self.quarantine_count,
                self.fresh,
                &self.assumptions_digest,
                &self.remedy_digest,
                self.frontier_age_ms,
            ),
            self.rows,
        ))
        .map_err(failure)?;
        if bytes.len() > 4_194_304 {
            return Err(failure("input byte bound"));
        }
        let digest = Digest::of_json_document(bytes.as_slice()).map_err(failure)?;
        let input = ForecastInput {
            table_version: self.version,
            table_digest: digest.clone(),
            source_digest: self.source_digest.clone(),
            source_head: self.source_head,
            split_boundary: boundary,
        };
        Ok((
            input,
            BlobRef {
                digest,
                version: Some(self.version.to_string()),
                size_bytes: u64::try_from(bytes.len()).map_err(failure)?,
            },
        ))
    }
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "approximate unit conversion refuses nonfinite or out-of-i64 results"
)]
fn nanos(value: f64) -> Result<i64, ForecastFailure> {
    let scaled = value * 1_000_000_000.0;
    if value < 0.0 || !scaled.is_finite() {
        return Err(failure("nonfinite or negative RMSE"));
    }
    format!("{scaled:.0}").parse().map_err(failure)
}

/// Pin fixed alpha, chronological split, ontology columns/units and freshness before fitting.
/// # Errors
/// Parameters cannot be represented canonically.
pub fn parameter_digest(plan: &TemporalPlan, boundary: i64) -> Result<Digest, ForecastFailure> {
    let parameters = norn_ir::canonical::canonical_json_of(&(
        "0.05",
        boundary,
        plan.object_type(),
        plan.max_age_micros(),
        plan.features()
            .iter()
            .map(|p| (&p.property, &p.unit))
            .collect::<Vec<_>>(),
        (&plan.outcome().property, &plan.outcome().unit),
    ))
    .map_err(failure)?;
    Digest::of_json_document(parameters).map_err(failure)
}

fn assessment(
    export: &ForecastExport<'_>,
    input: &ForecastInput,
    implementation: Digest,
) -> Result<ForecastSummary, ForecastFailure> {
    let late_features = export
        .rows
        .iter()
        .flat_map(|row| row.features.iter().map(move |v| (row.at, v)))
        .filter(|(at, v)| v.recorded_at > *at || v.valid_at > *at)
        .count();
    let training_rows = export
        .rows
        .iter()
        .filter(|row| row.at < input.split_boundary)
        .count();
    let late_training_labels = export
        .rows
        .iter()
        .filter(|row| {
            row.at < input.split_boundary
                && (row.outcome.recorded_at >= input.split_boundary
                    || row.outcome.valid_at >= input.split_boundary)
        })
        .count();
    let latest = export
        .rows
        .iter()
        .filter(|row| row.at < input.split_boundary)
        .map(|row| row.outcome.recorded_at)
        .max()
        .ok_or_else(|| failure("empty training partition"))?;
    let latest_ms = latest.div_euclid(1000);
    let error_unit = match export.plan.outcome().unit.as_ref() {
        None => "1".into(),
        Some(unit) if unit.system == norn_ir::UnitSystem::Ucum => unit.code.clone(),
        Some(_) => return Err(failure("only UCUM outcome units supported by this adapter")),
    };
    Ok(ForecastSummary {
        table_version: input.table_version.to_string(),
        table_digest: input.table_digest.clone(),
        source_digest: input.source_digest.clone(),
        source_head: input.source_head,
        split_boundary: input.split_boundary,
        implementation_digest: implementation,
        alpha: "0.05".into(),
        training_rows: u32::try_from(training_rows).map_err(failure)?,
        holdout_rows: u32::try_from(export.rows.len().saturating_sub(training_rows))
            .map_err(failure)?,
        late_features: u32::try_from(late_features).map_err(failure)?,
        late_training_labels: u32::try_from(late_training_labels).map_err(failure)?,
        temporal_leakage: late_features != 0 || late_training_labels != 0,
        complete: export.complete,
        quarantine_count: export.quarantine_count,
        fresh: export.fresh,
        parameters_digest: parameter_digest(export.plan, input.split_boundary)?,
        assumptions_digest: export.assumptions_digest.clone(),
        remedy_digest: export.remedy_digest.clone(),
        split_at_ms: input.split_boundary.div_euclid(1000),
        latest_training_label_ms: latest_ms,
        non_forward_outcome_count: u32::try_from(
            export
                .rows
                .iter()
                .filter(|r| r.outcome.valid_at <= r.at)
                .count(),
        )
        .map_err(failure)?,
        frontier_age_ms: export.frontier_age_ms,
        diagnostics_passed: false,
        error_unit,
        unit: export.plan.outcome().unit.clone(),
        rmse_nanos: None,
        baseline_rmse_nanos: None,
        report: None,
    })
}

// Statistical binary64 outputs are approximate evidence, not exact business decimals.
fn report_value(report: &impl serde::Serialize) -> Result<norn_ir::RuntimeValue, ForecastFailure> {
    fn tag(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Number(number) if number.is_f64() => {
                *value = serde_json::json!({ "encoding": "ieee754-binary64", "value": number.to_string() });
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    tag(item);
                }
            }
            serde_json::Value::Object(fields) => {
                for item in fields.values_mut() {
                    tag(item);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    let mut value = serde_json::to_value(report).map_err(failure)?;
    tag(&mut value);
    norn_ir::RuntimeValue::from_json_text(serde_json::to_string(&value).map_err(failure)?)
        .map_err(failure)
}

/// Bind exact export pins and prepare actual library evidence behind the host Function.
/// ANALYTICS-FUNCTION-INPUT-1: invalid quality/time evidence keeps model scores absent.
/// # Errors
/// Mismatched export pins, unsupported clock/unit profile or actual library refusal.
pub fn prepare_forecast(
    export: &ForecastExport<'_>,
    input: &ForecastInput,
) -> Result<ForecastSummary, ForecastFailure> {
    if input != &export.input(input.split_boundary)?.0
        || input.split_boundary.rem_euclid(1000) != 0
        || export
            .rows
            .iter()
            .flat_map(|r| r.features.iter().chain(std::iter::once(&r.outcome)))
            .any(|v| v.recorded_at < 0 || v.recorded_at.rem_euclid(1000) != 0)
    {
        return Err(failure("export pins or cell millisecond clock profile"));
    }
    let mut summary = assessment(
        export,
        input,
        crate::host::implementation_digest().map_err(failure)?,
    )?;
    if !summary.temporal_leakage
        && summary.non_forward_outcome_count == 0
        && summary.complete
        && summary.quarantine_count == 0
        && summary.fresh
    {
        let table = export
            .plan
            .materialize(export.as_of, export.source_head, export.rows.to_vec())
            .map_err(failure)?;
        let report = crate::analysis::forecast(&table, input.split_boundary).map_err(failure)?;
        summary.rmse_nanos = Some(nanos(report.rmse)?);
        summary.baseline_rmse_nanos = Some(nanos(report.baseline_rmse)?);
        summary.diagnostics_passed = report.fit.diagnostics.iter().all(|d| d.passed);
        summary.report = Some(report_value(&report)?);
    }
    Ok(summary)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "bounded report-unit property"
)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn nonfinite_negative_and_unrepresentable_scores_refuse() {
        for value in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] {
            assert!(nanos(value).is_err());
        }
    }
    pub(crate) fn fixture() -> (TemporalPlan, Vec<TemporalRow>) {
        use crate::temporal::{TemporalRow, TemporalValue};
        let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("typed fixtures");
        let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_slice(
            &std::fs::read(std::path::Path::new(&root).join("metric-demand.json"))
                .expect("fixture"),
        )
        .expect("artifacts");
        let catalog = norn_checker::EffectiveCatalog::from_artifacts(&artifacts);
        let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0)
            .expect("plan");
        let value = |n: i32, at| TemporalValue {
            value: norn_ir::RuntimeValue::from_json_text(n.to_string()).expect("number"),
            valid_at: at,
            recorded_at: at,
            source: format!("unit:{n}:{at}"),
        };
        let rows: Vec<_> = [4, 4, 8, 8, 12, 14, 16]
            .into_iter()
            .enumerate()
            .map(|(i, y)| {
                let x = i32::try_from(i).expect("index") + 1;
                let at = i64::from(x) * 1_000_000;
                TemporalRow {
                    key: format!("unit-{i}"),
                    at,
                    features: vec![value(x, at)],
                    outcome: value(y, at + 1000),
                }
            })
            .collect();
        (plan, rows)
    }
    #[test]
    fn actual_report_and_late_evidence_assessment_are_distinct() {
        let (plan, rows) = fixture();
        let digest = crate::host::implementation_digest().expect("source");
        let mut export = ForecastExport {
            tenant: "unit",
            world: "main",
            plan: &plan,
            rows: &rows,
            version: 1,
            source_digest: digest.clone(),
            source_head: 42,
            as_of: 8_000_000,
            complete: true,
            quarantine_count: 0,
            fresh: true,
            assumptions_digest: digest.clone(),
            remedy_digest: digest,
            frontier_age_ms: 0,
        };
        let (input, _) = export.input(6_000_000).expect("input");
        let report = prepare_forecast(&export, &input).expect("report");
        assert!(report.report.is_some());
        assert_eq!(report.rmse_nanos, Some(800_000_000));
        assert_eq!((report.training_rows, report.holdout_rows), (5, 2));
        let mut late = rows.clone();
        for row in &mut late {
            for feature in &mut row.features {
                feature.recorded_at = 8_000_000;
            }
            row.outcome.recorded_at = 8_000_000;
        }
        export.rows = &late;
        let (input, _) = export.input(6_000_000).expect("late input");
        let refused = prepare_forecast(&export, &input).expect("assessment");
        assert!(refused.temporal_leakage);
        assert_eq!(
            (refused.late_features, refused.late_training_labels),
            (7, 5)
        );
        assert!(refused.report.is_none() && refused.rmse_nanos.is_none());
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn approximate_evidence_preserves_binary64_bits(bits in any::<u64>()) {
            let value = f64::from_bits(bits);
            prop_assume!(value.is_finite());
            let encoded = report_value(&value).expect("approximate evidence");
            let decoded = encoded.as_object().and_then(|fields| fields.get("value")).and_then(norn_ir::RuntimeValue::as_str).expect("tagged binary64").parse::<f64>().expect("roundtrip float");
            prop_assert_eq!(decoded.to_bits(),value.to_bits());
        }
        #[test]
        fn representable_report_units_round_trip(n in 0i32..1_000_000_000) {
            prop_assert_eq!(nanos(f64::from(n) / 1_000_000_000.0).expect("nanos"), i64::from(n));
        }
    }
}
