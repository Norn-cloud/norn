//! MANIFEST-LAWS-1 (#1381): the laws and invariants of a dataset run set-based.
//!
//! Every invariant and governing law of the dataset's type becomes one
//! [`RuleCheck`]. Its condition and predicate are lowered from the admitted
//! ZEN source through `norn_expr::row_expression`, the VIEW-ENGINE-1 row
//! subset, into a typed [`RowPredicate`] that the engine evaluates over the
//! whole dataset in one pass. The engine decides a row only where its SQL
//! and the per-row evaluator provably agree. That is the case when every
//! value the rule reads is present, every number stays within the range the
//! per-row decimal holds exactly, and the predicate is in the typed subset
//! ([`RowPredicate::kind`]). Every other row is residue. A rule outside the
//! subset makes every row residue. The cell decides residue rows with the
//! per-row path's own evaluator ([`CellStore::residue_findings`]), so the held-back set
//! and the findings are the per-row path's by construction.
//!
//! The held-back rule is the per-row one (DATA-QUARANTINE,
//! `snapshot_observe::finding_quarantines`). A row that fails a rule that
//! holds is held back as a set. Clean rows are trusted in the same commit.

use std::collections::BTreeMap;

use norn_expr::{RowArithmetic, RowComparison, RowExpr};
use norn_ir::{
    DataQualityRuleRef, DataQualitySeverity, LawModality, LawResponse, LawSeverity, ObjectTypeName,
    RuntimeValue,
};
use norn_kernel::{FactKey, TypeCatalog};

use crate::analytics::{
    ColumnKind, ResidueRow, RowFinding, RowPredicate, RuleCheck, TableSchema, Value,
};
use crate::engine::SqlEngine;
use crate::model::Model;
use crate::{CellError, CellStore};

fn refused(detail: impl Into<String>) -> CellError {
    CellError::DatasetRefused {
        code: "bulk.unsupported_law",
        detail: detail.into(),
    }
}

/// The rule identity findings carry, as the engine and the manifest name it.
///
/// # Errors
/// The reference cannot be canonicalized.
pub fn rule_key(rule_ref: &DataQualityRuleRef) -> Result<String, CellError> {
    let bytes = norn_ir::canonical::canonical_json_of(rule_ref)
        .map_err(|error| CellError::Encoding(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| CellError::Encoding(error.to_string()))
}

/// The catalog the per-row observation path evaluates `model` with.
fn catalog(model: &Model) -> TypeCatalog<'_> {
    TypeCatalog::new(model.object_types())
        .with_interfaces(model.interfaces())
        .with_property_types(model.property_types())
        .with_governing_laws(model.law_artifacts())
}

/// Lower one admitted ZEN source over `self`, or `None` outside the subset.
fn lower_source(source: &str, table: &TableSchema) -> Option<RowPredicate> {
    let tree = norn_expr::row_expression(source, "self").ok()??;
    let lowered = lower(&tree)?;
    lowered.kind(table)?;
    Some(lowered)
}

fn lower(tree: &RowExpr) -> Option<RowPredicate> {
    use RowPredicate as P;
    let pair = |a: &RowExpr, b: &RowExpr| Some((Box::new(lower(a)?), Box::new(lower(b)?)));
    Some(match tree {
        RowExpr::Null => P::Null,
        RowExpr::Bool(flag) => P::Bool(*flag),
        RowExpr::Number(number) => P::Number(number.as_str().to_owned()),
        RowExpr::Text(text) => P::Text(text.clone()),
        RowExpr::Column(name) => P::Column(name.clone()),
        RowExpr::Not(inner) => P::Not(Box::new(lower(inner)?)),
        RowExpr::Negate(inner) => P::Negate(Box::new(lower(inner)?)),
        RowExpr::Arithmetic(op, a, b) => {
            let (a, b) = pair(a, b)?;
            match op {
                RowArithmetic::Add => P::Add(a, b),
                RowArithmetic::Subtract => P::Subtract(a, b),
                RowArithmetic::Multiply => P::Multiply(a, b),
            }
        }
        RowExpr::Compare(op, a, b) => {
            let (a, b) = pair(a, b)?;
            match op {
                RowComparison::Equal => P::Equal(a, b),
                RowComparison::NotEqual => P::NotEqual(a, b),
                RowComparison::Less => P::Less(a, b),
                RowComparison::LessOrEqual => P::LessOrEqual(a, b),
                RowComparison::Greater => P::Greater(a, b),
                RowComparison::GreaterOrEqual => P::GreaterOrEqual(a, b),
            }
        }
        RowExpr::And(a, b) => {
            let (a, b) = pair(a, b)?;
            P::And(a, b)
        }
        RowExpr::Or(a, b) => {
            let (a, b) = pair(a, b)?;
            P::Or(a, b)
        }
        // `x ?? y` reads `y` only where `x` is null, and a row with a null
        // read is residue, so over every row the engine decides it is `x`.
        RowExpr::Coalesce(a, b) => {
            if matches!(a.as_ref(), RowExpr::Null) {
                lower(b)?
            } else {
                lower(a)?
            }
        }
        RowExpr::Conditional(c, a, b) => P::Conditional(
            Box::new(lower(c)?),
            Box::new(lower(a)?),
            Box::new(lower(b)?),
        ),
    })
}

const fn severity_of(law: LawSeverity) -> DataQualitySeverity {
    match law {
        LawSeverity::Error => DataQualitySeverity::Error,
        LawSeverity::Warning => DataQualitySeverity::Warning,
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// MANIFEST-LAWS-1: every invariant and governing law of `object_type`,
    /// as the engine checks it over a dataset of `table`.
    ///
    /// # Errors
    /// `bulk.unsupported_law` for a law the per-row evaluator refuses or one
    /// that reads a link (a dataset row has none), or an unknown type.
    pub fn dataset_checks(
        &self,
        object_type: &str,
        table: &TableSchema,
    ) -> Result<Vec<RuleCheck>, CellError> {
        let model = self.model();
        let catalog = catalog(model);
        let name = ObjectTypeName(object_type.to_owned());
        let invariants = catalog
            .effective_invariants(&name)
            .map_err(|missing| refused(format!("interface {} is missing", missing.as_str())))?;
        let quarantines = |rule_ref: &DataQualityRuleRef, severity| {
            norn_kernel::snapshot_observe::finding_quarantines(&catalog, rule_ref, severity)
                .map_err(|error| CellError::Encoding(error.to_string()))
        };
        let mut checks = Vec::new();
        for (owner, invariant) in &invariants {
            let version = catalog
                .invariant_owner_version(owner)
                .map_err(|error| CellError::Encoding(error.to_string()))?
                .ok_or_else(|| refused("an invariant owner is not retained"))?;
            let rule_ref = DataQualityRuleRef::Invariant {
                owner: owner.clone(),
                invariant_id: invariant.id.clone(),
                version,
            };
            let severity = match invariant.classification {
                norn_ir::ConstraintClass::Integrity => DataQualitySeverity::Error,
                norn_ir::ConstraintClass::Quality => DataQualitySeverity::Warning,
            };
            checks.push(RuleCheck {
                rule: rule_key(&rule_ref)?,
                label: format!("invariant:{}", invariant.id.as_str()),
                holds: quarantines(&rule_ref, severity)?,
                condition: None,
                predicate: lower_source(invariant.expression.as_str(), table),
                expect: true,
            });
        }
        let laws = catalog
            .governing_laws(&name)
            .map_err(|error| refused(error.to_string()))?;
        for artifact in laws {
            let (law, rule_ref) = norn_kernel::governing_laws::law_rule(&catalog, &name, artifact)
                .map_err(|error| refused(error.to_string()))?;
            let (condition, predicate) = match &law.response {
                LawResponse::Predicate { expression } => {
                    let condition = match &law.condition {
                        Some(condition) => lower_source(condition.as_str(), table).map(Some),
                        None => Some(None),
                    };
                    match condition {
                        Some(condition) => (condition, lower_source(expression.as_str(), table)),
                        None => (None, None),
                    }
                }
                LawResponse::Invariant { invariant_id, .. } => (
                    None,
                    invariants
                        .iter()
                        .find(|(_, invariant)| invariant.id.as_str() == invariant_id)
                        .and_then(|(_, invariant)| {
                            lower_source(invariant.expression.as_str(), table)
                        }),
                ),
                LawResponse::Action { .. }
                | LawResponse::Transition { .. }
                | LawResponse::TranslationPreserves { .. } => {
                    return Err(refused(format!("{}: unsupported law semantics", law.id)));
                }
            };
            let severity = severity_of(law.severity);
            checks.push(RuleCheck {
                rule: rule_key(&rule_ref)?,
                label: format!("law:{}", law.id),
                holds: quarantines(&rule_ref, severity)?,
                condition,
                predicate,
                expect: matches!(law.modality, LawModality::Always {}),
            });
        }
        Ok(checks)
    }

    /// MANIFEST-LAWS-1: the findings of residue rows, by the per-row
    /// observation path's own evaluation of each row's values
    /// (`snapshot_observe::observation_rule_findings`).
    ///
    /// # Errors
    /// A row the per-row path cannot evaluate, which refuses the dataset.
    pub fn residue_findings(
        &self,
        object_type: &str,
        table: &TableSchema,
        rows: &[ResidueRow],
    ) -> Result<Vec<RowFinding>, CellError> {
        let model = self.model();
        let catalog = catalog(model);
        let labels: BTreeMap<String, String> = self
            .dataset_checks(object_type, table)?
            .into_iter()
            .map(|check| (check.rule, check.label))
            .collect();
        let mut findings = Vec::new();
        for row in rows {
            let values = row_values(table, &row.values)?;
            let key = FactKey::new(object_type, row.row.to_string());
            for found in
                norn_kernel::snapshot_observe::observation_rule_findings(&catalog, &key, &values)
                    .map_err(|error| refused(error.to_string()))?
            {
                let rule = rule_key(&found.rule_ref)?;
                findings.push(RowFinding {
                    row: row.row,
                    label: labels.get(&rule).cloned().unwrap_or_else(|| rule.clone()),
                    rule,
                    holds: found.quarantines,
                });
            }
        }
        Ok(findings)
    }
}

/// A residue row's values as the per-row path stores them.
fn row_values(
    table: &TableSchema,
    values: &BTreeMap<String, Value>,
) -> Result<norn_ir::RuntimeObject, CellError> {
    let mut object = norn_ir::RuntimeObject::new();
    for column in &table.columns {
        let Some(value) = values.get(&column.id) else {
            continue;
        };
        let runtime = match (column.kind, value) {
            (ColumnKind::Integer | ColumnKind::Decimal { .. }, Value::Number(text)) => {
                RuntimeValue::from(
                    norn_ir::Decimal::parse(text)
                        .map_err(|error| CellError::Encoding(error.to_string()))?,
                )
            }
            (ColumnKind::Boolean, Value::Bool(flag)) => RuntimeValue::from(*flag),
            (ColumnKind::Text, Value::Text(text))
            | (ColumnKind::Timestamp | ColumnKind::Date, Value::Instant { text, .. }) => {
                RuntimeValue::from(text.as_str())
            }
            (ColumnKind::Opaque, Value::Json(text)) => RuntimeValue::from_json_text(text)
                .map_err(|error| CellError::Encoding(error.to_string()))?,
            _ => {
                return Err(CellError::Encoding(format!(
                    "a residue value of {} does not match its column",
                    column.id
                )));
            }
        };
        object.insert(column.id.clone(), runtime);
    }
    Ok(object)
}
