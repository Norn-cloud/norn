//! Bounded ontology-bound point-in-time tables for analytical Functions.
//!
//! The exporting host owns authorization and the authenticity of source receipts.
//! This module checks supplied evidence; it never confers a proof badge or access.

use std::collections::BTreeSet;

use norn_checker::{EffectiveCatalog, EffectiveKind};
use norn_ir::{PropertyType, RuntimeValue, UnitRef};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A refusal keeps untrusted observations out of an analytical table. The host
/// retains the original observation and may turn the refusal into a quality finding.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemporalError {
    /// The declaration exceeds this deliberately narrow execution profile.
    #[error("analysis.declaration: {0}")]
    Declaration(&'static str),
    /// A row fails schema, provenance, freshness or temporal checks.
    #[error("analysis.row: {0}")]
    Row(&'static str),
    /// Training would use evidence unavailable at the evaluation boundary.
    #[error("analysis.split: {0}")]
    Split(&'static str),
}

/// A single value and its bitemporal, source-owned evidence. Times are signed
/// microseconds since the Unix epoch, in UTC; no local clock is consulted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalValue {
    /// Exact ontology value. Statistical conversion is a separate, approximate step.
    pub value: RuntimeValue,
    /// Effective time of this value.
    pub valid_at: i64,
    /// When this version became known to the ledger (including corrections).
    pub recorded_at: i64,
    /// Immutable observation/receipt identity, resolved by the exporting host.
    pub source: String,
}

/// One declared object at a prediction/analysis instant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalRow {
    /// Stable ontology object identity.
    pub key: String,
    /// Feature cutoff; all feature valid and recorded times must be at or before it.
    pub at: i64,
    /// Feature values in declared column order.
    pub features: Vec<TemporalValue>,
    /// Outcome, known by the table extraction time, possibly after the feature cutoff.
    pub outcome: TemporalValue,
}

/// A numeric column resolved from the effective ontology, with units retained.
#[derive(Debug, Clone)]
pub struct NumericColumn {
    /// Direct property identity, relative to the declared object type.
    pub property: String,
    /// Unit taken from the effective property; callers cannot assert one.
    pub unit: Option<UnitRef>,
    effective: norn_checker::EffectiveProperty,
}

/// Checked declaration for a bounded table. No caller SQL or callbacks.
#[derive(Debug, Clone)]
pub struct TemporalPlan {
    object_type: String,
    features: Vec<NumericColumn>,
    outcome: NumericColumn,
    max_age_micros: i64,
}

/// Immutable admitted table. Construction checks every row; no partial table
/// is returned on failure. This is not an authorized export or a ledger receipt.
#[derive(Debug)]
pub struct TemporalTable {
    plan: TemporalPlan,
    as_of: i64,
    source_head: u64,
    rows: Vec<TemporalRow>,
}

impl TemporalPlan {
    /// Resolve direct numeric fields against the effective ontology. Nullable,
    /// computed, monetary and nonnumeric columns are refused in this first profile.
    ///
    /// # Errors
    /// Unsupported fields, duplicate columns, or invalid resource/freshness bounds.
    pub fn compile(
        catalog: &EffectiveCatalog,
        object_type: &str,
        features: &[String],
        outcome: &str,
        max_age_micros: i64,
    ) -> Result<Self, TemporalError> {
        if features.is_empty() || features.len() > 16 || max_age_micros < 0 {
            return Err(TemporalError::Declaration("column or freshness bound"));
        }
        let mut unique = BTreeSet::new();
        for name in features {
            if name == outcome || !unique.insert(name) {
                return Err(TemporalError::Declaration("duplicate or target feature"));
            }
        }
        let object = catalog
            .object_type_named(object_type)
            .and_then(Result::ok)
            .ok_or(TemporalError::Declaration("unknown object type"))?;
        let resolve = |name: &str| -> Result<NumericColumn, TemporalError> {
            let effective = object
                .properties
                .iter()
                .find(|p| p.id.as_str() == name)
                .ok_or(TemporalError::Declaration("unknown property"))?
                .clone();
            if effective.nullable || effective.computed {
                return Err(TemporalError::Declaration("nullable or computed property"));
            }
            let unit = match &effective.kind {
                EffectiveKind::Base(PropertyType::Integer {}) => None,
                EffectiveKind::Base(PropertyType::Decimal {
                    unit,
                    currency_source: None,
                    ..
                }) => unit.clone(),
                EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => {
                    return Err(TemporalError::Declaration(
                        "non-numeric or monetary property",
                    ));
                }
            };
            Ok(NumericColumn {
                property: name.to_owned(),
                unit,
                effective,
            })
        };
        Ok(Self {
            object_type: object_type.to_owned(),
            features: features
                .iter()
                .map(|name| resolve(name))
                .collect::<Result<_, _>>()?,
            outcome: resolve(outcome)?,
            max_age_micros,
        })
    }

    /// Declared object type, for the host's existing export authorization checks.
    #[must_use]
    pub fn object_type(&self) -> &str {
        &self.object_type
    }

    /// Ontology-bound columns, in input order.
    #[must_use]
    pub fn features(&self) -> &[NumericColumn] {
        &self.features
    }

    /// Ontology-bound outcome.
    #[must_use]
    pub fn outcome(&self) -> &NumericColumn {
        &self.outcome
    }

    /// Admit a complete supplied snapshot with both event-time and knowledge-time
    /// checks. A backdated correction recorded after prediction time is refused.
    /// Freshness is checked per feature, relative to the row cutoff, never "now".
    ///
    /// # Errors
    /// Empty/oversized tables, missing provenance, duplicate grain, schema errors,
    /// future knowledge, target-before-feature time, or stale feature versions.
    pub fn materialize(
        &self,
        as_of: i64,
        source_head: u64,
        rows: Vec<TemporalRow>,
    ) -> Result<TemporalTable, TemporalError> {
        if rows.is_empty() || rows.len() > 10_000 || source_head == 0 {
            return Err(TemporalError::Row("table bound or missing source head"));
        }
        let mut grain = BTreeSet::new();
        let mut retained_bytes = 0_usize;
        for row in &rows {
            if row.key.len() > 256 {
                return Err(TemporalError::Row("object identity byte bound"));
            }
            retained_bytes = retained_bytes.saturating_add(row.key.len());
            if row.key.trim().is_empty() || !grain.insert((&row.key, row.at)) {
                return Err(TemporalError::Row("missing or duplicate object/time grain"));
            }
            if row.at > as_of || row.features.len() != self.features.len() {
                return Err(TemporalError::Row("cutoff or row width"));
            }
            check_value(&self.outcome, &row.outcome)?;
            if row.outcome.valid_at < row.at
                || row.outcome.recorded_at > as_of
                || row.outcome.valid_at > row.outcome.recorded_at
            {
                return Err(TemporalError::Row("outcome not yet observed"));
            }
            for (column, value) in self.features.iter().zip(&row.features) {
                check_value(column, value)?;
                if value.valid_at > row.at || value.recorded_at > row.at {
                    return Err(TemporalError::Row("feature leakage"));
                }
                if row
                    .at
                    .checked_sub(value.valid_at)
                    .is_none_or(|age| age > self.max_age_micros)
                {
                    return Err(TemporalError::Row("stale feature"));
                }
            }
            for value in row.features.iter().chain(std::iter::once(&row.outcome)) {
                // Validated numeric values have at most 29 digits plus sign and
                // decimal point. Reserve 64 bytes each without serialization.
                retained_bytes = retained_bytes
                    .saturating_add(value.source.len())
                    .saturating_add(64);
            }
            if retained_bytes > 1_048_576 {
                return Err(TemporalError::Row("table payload byte bound"));
            }
        }
        Ok(TemporalTable {
            plan: self.clone(),
            as_of,
            source_head,
            rows,
        })
    }
}

fn check_value(column: &NumericColumn, value: &TemporalValue) -> Result<(), TemporalError> {
    if value.source.trim().is_empty() || value.source.len() > 512 {
        return Err(TemporalError::Row("missing source receipt"));
    }
    column
        .effective
        .check_runtime_value(&value.value)
        .map_err(|_error| TemporalError::Row("ontology value mismatch"))
}

impl TemporalTable {
    /// The declaration used for admission.
    #[must_use]
    pub fn plan(&self) -> &TemporalPlan {
        &self.plan
    }

    /// Snapshot's knowledge cutoff.
    #[must_use]
    pub fn as_of(&self) -> i64 {
        self.as_of
    }

    /// Claimed ledger cursor; the host must authenticate it before labelling evidence.
    #[must_use]
    pub fn source_head(&self) -> u64 {
        self.source_head
    }

    /// Admitted rows including exact values and all source references.
    #[must_use]
    pub fn rows(&self) -> &[TemporalRow] {
        &self.rows
    }

    /// A chronological holdout. Training labels must have been known strictly
    /// before the boundary. Object/time grain is unique, but repeated objects
    /// across time are allowed for forecasting and are not independent samples.
    ///
    /// # Errors
    /// Empty partitions, labels crossing the boundary, or non-forward outcomes.
    pub fn split(
        &self,
        boundary: i64,
    ) -> Result<(Vec<&TemporalRow>, Vec<&TemporalRow>), TemporalError> {
        let (train, test): (Vec<_>, Vec<_>) = self.rows.iter().partition(|r| r.at < boundary);
        if train.is_empty() || test.is_empty() {
            return Err(TemporalError::Split("empty partition"));
        }
        for row in &train {
            if row.outcome.recorded_at >= boundary || row.outcome.valid_at <= row.at {
                return Err(TemporalError::Split(
                    "training label crosses boundary or is not forward",
                ));
            }
        }
        if test.iter().any(|r| r.outcome.valid_at <= r.at) {
            return Err(TemporalError::Split("test outcome is not forward"));
        }
        Ok((train, test))
    }
}
