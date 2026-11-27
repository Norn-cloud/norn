//! MANIFEST-LAWS-1 (#1381): the laws and invariants of a dataset run set-based.
//!
//! Every invariant and governing law of the dataset's type becomes one
//! [`RuleCheck`]. Its condition and predicate are lowered from the admitted
//! ZEN source through `governing_laws::law_row_tree`, the VIEW-ENGINE-1 row
//! subset, into a typed [`RowPredicate`] that the engine evaluates over the
//! whole dataset in one pass. The engine decides a row only where its SQL
//! and the per-row evaluator provably agree. That is the case when every
//! value the rule reads is present, every number stays within the range the
//! per-row decimal holds exactly, every text is in NFC (the form the per-row
//! evaluator compares by), and the predicate is in the typed subset
//! ([`RowPredicate::kind`]). Every other row is residue. A rule outside the
//! subset makes every row residue. The cell decides residue rows with the
//! per-row path's own evaluator ([`CellStore::residue_findings`]), so the held-back set
//! and the findings are the per-row path's by construction.
//!
//! The held-back rule is the per-row one (DATA-QUARANTINE,
//! `snapshot_observe::finding_quarantines`). A row that fails a rule that
//! holds is held back as a set. Clean rows are trusted in the same commit.

use std::collections::BTreeMap;

use norn_checker::transform::{RowArithmetic, RowComparison, RowExpr};
use norn_ir::{
    DataQualityRuleRef, DataQualitySeverity, LawModality, LawResponse, LawSeverity, ObjectTypeName,
    RuntimeValue,
};
use norn_kernel::{FactKey, TypeCatalog};

use crate::analytics::{
    ColumnKind, RULE_FUEL, ResidueRow, RowFinding, RowPredicate, RuleCheck, TableSchema, Value,
    rule_fuel,
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

/// Lower one admitted ZEN source over `self` and count its row-tree nodes
/// plus referenced column key bytes (MANIFEST-LAWS-1 S2) in a single parse.
fn lower_and_count(source: &str, table: &TableSchema) -> (Option<RowPredicate>, u64, u64) {
    let Some(tree) = norn_kernel::governing_laws::law_row_tree(source) else {
        return (None, 0, 0);
    };
    let nodes = tree_nodes(&tree);
    let key_bytes = column_key_bytes(&tree);
    let predicate = lower(&tree)
        .filter(|lowered| lowered.kind(table) == Some(crate::analytics::PredicateKind::Bool));
    (predicate, nodes, key_bytes)
}

/// Sum UTF-8 byte lengths of every `Column(name)` read in `tree`, bounding
/// ZEN's `Fetch` key-byte charge when a member receiver is parenthesized
/// (e.g. `(self).long_property_name`).
fn column_key_bytes(tree: &RowExpr) -> u64 {
    match tree {
        RowExpr::Null | RowExpr::Bool(_) | RowExpr::Number(_) | RowExpr::Text(_) => 0,
        RowExpr::Column(name) => u64::try_from(name.len()).unwrap_or(u64::MAX),
        RowExpr::Not(inner)
        | RowExpr::Negate(inner)
        | RowExpr::CalendarDate(inner)
        | RowExpr::Timestamp(inner) => column_key_bytes(inner),
        RowExpr::Arithmetic(_, a, b)
        | RowExpr::Compare(_, a, b)
        | RowExpr::And(a, b)
        | RowExpr::Or(a, b)
        | RowExpr::Coalesce(a, b) => column_key_bytes(a).saturating_add(column_key_bytes(b)),
        RowExpr::Conditional(c, a, b) => column_key_bytes(c)
            .saturating_add(column_key_bytes(a))
            .saturating_add(column_key_bytes(b)),
    }
}

fn tree_nodes(tree: &RowExpr) -> u64 {
    let children = match tree {
        RowExpr::Null
        | RowExpr::Bool(_)
        | RowExpr::Number(_)
        | RowExpr::Text(_)
        | RowExpr::Column(_) => 0,
        RowExpr::Not(inner)
        | RowExpr::Negate(inner)
        | RowExpr::CalendarDate(inner)
        | RowExpr::Timestamp(inner) => tree_nodes(inner),
        RowExpr::Arithmetic(_, a, b)
        | RowExpr::Compare(_, a, b)
        | RowExpr::And(a, b)
        | RowExpr::Or(a, b)
        | RowExpr::Coalesce(a, b) => tree_nodes(a).saturating_add(tree_nodes(b)),
        RowExpr::Conditional(c, a, b) => tree_nodes(c)
            .saturating_add(tree_nodes(a))
            .saturating_add(tree_nodes(b)),
    };
    children.saturating_add(1)
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
        RowExpr::Coalesce(a, b) => {
            let (a, b) = pair(a, b)?;
            P::Coalesce(a, b)
        }
        RowExpr::Conditional(c, a, b) => P::Conditional(
            Box::new(lower(c)?),
            Box::new(lower(a)?),
            Box::new(lower(b)?),
        ),
        RowExpr::CalendarDate(inner) => P::Date(Box::new(lower(inner)?)),
        RowExpr::Timestamp(inner) => P::Timestamp(Box::new(lower(inner)?)),
    })
}

const fn severity_of(law: LawSeverity) -> DataQualitySeverity {
    match law {
        LawSeverity::Error => DataQualitySeverity::Error,
        LawSeverity::Warning => DataQualitySeverity::Warning,
    }
}

/// Unified set-based evaluation plan for one governing law or invariant over a
/// dataset table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPlan {
    /// Optional activation condition, folding any row-tree `when` condition
    /// and `unless` exception tree into `when AND NOT lifted(exceptions)`.
    pub condition: Option<RowPredicate>,
    /// Lowered Boolean response predicate, or `None` when any required piece
    /// falls outside the exact SQL row subset (all rows settle per-row).
    pub predicate: Option<RowPredicate>,
    /// MANIFEST-LAWS-1 (S2): static fuel upper bound covering the rule, its
    /// nodes, and eager exception-condition evaluation.
    pub fuel: u64,
}

fn disjoin(mut items: Vec<RowPredicate>) -> Option<RowPredicate> {
    let mut iter = items.drain(..);
    let first = iter.next()?;
    Some(iter.fold(first, |acc, item| {
        RowPredicate::Or(Box::new(acc), Box::new(item))
    }))
}

fn lifted_under(
    parent: &str,
    exceptions: &[norn_ir::LawException],
    conditions: &[RowPredicate],
    children: &BTreeMap<&str, Vec<usize>>,
    depth: usize,
) -> Option<RowPredicate> {
    if depth > exceptions.len() {
        return None;
    }
    let mut branches = Vec::new();
    for &index in children.get(parent).into_iter().flatten() {
        let exception = exceptions.get(index)?;
        let cond = conditions.get(index)?.clone();
        let branch = match lifted_under(
            exception.id.as_str(),
            exceptions,
            conditions,
            children,
            depth.saturating_add(1),
        ) {
            Some(overridden) => RowPredicate::And(
                Box::new(cond),
                Box::new(RowPredicate::Not(Box::new(overridden))),
            ),
            None => cond,
        };
        branches.push(branch);
    }
    disjoin(branches)
}

/// Fold an admitted, acyclic exception tree rooted at `law_id` into a single
/// Boolean [`RowPredicate`] computing `lifted(law_id)` (exact match with
/// `norn_checker::law_exceptions::fold`).
fn fold_exception_tree(
    law_id: &str,
    exceptions: &[norn_ir::LawException],
    conditions: &[RowPredicate],
) -> Option<RowPredicate> {
    if exceptions.is_empty() || exceptions.len() != conditions.len() {
        return None;
    }
    let mut children: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, exception) in exceptions.iter().enumerate() {
        children
            .entry(exception.overrides.as_str())
            .or_default()
            .push(index);
    }
    lifted_under(law_id, exceptions, conditions, &children, 0)
}

/// Plan one object/interface invariant over `table`.
#[must_use]
pub fn plan_invariant(source: &str, table: &TableSchema) -> RowPlan {
    let (predicate, nodes, key_bytes) = lower_and_count(source, table);
    RowPlan {
        condition: None,
        predicate,
        fuel: rule_fuel(nodes).saturating_add(key_bytes),
    }
}

/// Plan one governing law's response, optional `when` condition, and optional
/// `unless` exception tree into a unified [`RowPlan`].
///
/// When `law.condition`, every `exception.condition`, and the response
/// expression all lower into the typed Boolean [`RowPredicate`] subset (within
/// predicate depth), the exception tree is folded into
/// `effective_condition = when AND NOT lifted(exceptions)` so the engine
/// decides rows set-based with zero residue. Otherwise the plan falls back to
/// `(None, None)` while retaining its full static fuel bound.
fn plan_law(
    law: &norn_ir::LawIR,
    invariants: &[(norn_ir::InvariantOwner, &norn_ir::Invariant)],
    table: &TableSchema,
) -> Result<RowPlan, CellError> {
    let (condition, predicate, nodes, key_bytes, extra_fuel) = match &law.response {
        LawResponse::Predicate { expression } => {
            let (when_cond, cond_nodes, cond_keys) = match &law.condition {
                Some(condition) => {
                    let (lowered, nodes, keys) = lower_and_count(condition.as_str(), table);
                    (lowered.map(Some), nodes, keys)
                }
                None => (Some(None), 0, 0),
            };
            let (expr_pred, expr_nodes, expr_keys) = lower_and_count(expression.as_str(), table);
            let mut nodes = expr_nodes.saturating_add(cond_nodes);
            let mut key_bytes = expr_keys.saturating_add(cond_keys);
            if law.exceptions.is_empty() {
                match when_cond {
                    Some(condition) => (condition, expr_pred, nodes, key_bytes, 0),
                    None => (None, None, nodes, key_bytes, 0),
                }
            } else {
                let mut ex_preds = Vec::with_capacity(law.exceptions.len());
                let mut all_exceptions_lowered = true;
                for exception in &law.exceptions {
                    let (lowered, ex_nodes, ex_keys) =
                        lower_and_count(exception.condition.as_str(), table);
                    nodes = nodes.saturating_add(ex_nodes);
                    key_bytes = key_bytes.saturating_add(ex_keys);
                    match lowered {
                        Some(pred) => ex_preds.push(pred),
                        None => all_exceptions_lowered = false,
                    }
                }
                // `governing_laws::exception_lifts` evaluates each exception's
                // condition eagerly under `TurnFuel`, adding one rule entry
                // overhead (`RULE_FUEL`) per exception on top of its AST nodes.
                let extra = u64::try_from(law.exceptions.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(RULE_FUEL);
                let folded = if all_exceptions_lowered {
                    when_cond.zip(expr_pred).and_then(|(when, predicate)| {
                        let lifted = fold_exception_tree(&law.id, &law.exceptions, &ex_preds)?;
                        let not_lifted = RowPredicate::Not(Box::new(lifted));
                        let effective = match when {
                            Some(when) => RowPredicate::And(Box::new(when), Box::new(not_lifted)),
                            None => not_lifted,
                        };
                        (effective.kind(table) == Some(crate::analytics::PredicateKind::Bool))
                            .then_some((Some(effective), Some(predicate)))
                    })
                } else {
                    None
                };
                match folded {
                    Some((condition, predicate)) => (condition, predicate, nodes, key_bytes, extra),
                    None => (None, None, nodes, key_bytes, extra),
                }
            }
        }
        LawResponse::Invariant { invariant_id, .. } => {
            let source = invariants
                .iter()
                .find(|(_, invariant)| invariant.id.as_str() == invariant_id)
                .map(|(_, invariant)| invariant.expression.as_str());
            let (predicate, nodes, key_bytes) =
                source.map_or((None, 0, 0), |source| lower_and_count(source, table));
            (None, predicate, nodes, key_bytes, 0)
        }
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            return Err(refused(format!("{}: unsupported law semantics", law.id)));
        }
    };
    Ok(RowPlan {
        condition,
        predicate,
        fuel: rule_fuel(nodes)
            .saturating_add(key_bytes)
            .saturating_add(extra_fuel),
    })
}

impl<E: SqlEngine> CellStore<E> {
    /// MANIFEST-LAWS-1: every invariant and governing law of `object_type`,
    /// as the engine checks it over a dataset of `table`. The first check's
    /// fuel also carries the row's typed-property charge
    /// (`snapshot_observe::observation_typed_fuel`), so the checks' fuel
    /// together bounds what the per-row evaluator charges a row.
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
            let plan = plan_invariant(invariant.expression.as_str(), table);
            checks.push(RuleCheck {
                rule: rule_key(&rule_ref)?,
                label: format!("invariant:{}", invariant.id.as_str()),
                holds: quarantines(&rule_ref, severity)?,
                condition: plan.condition,
                predicate: plan.predicate,
                expect: true,
                fuel: plan.fuel,
            });
        }
        let laws = catalog
            .governing_laws(&name)
            .map_err(|error| refused(error.to_string()))?;
        for artifact in laws {
            let (law, rule_ref) = norn_kernel::governing_laws::law_rule(&catalog, &name, artifact)
                .map_err(|error| refused(error.to_string()))?;
            let plan = plan_law(&law, &invariants, table)?;
            let severity = severity_of(law.severity);
            checks.push(RuleCheck {
                rule: rule_key(&rule_ref)?,
                label: format!("law:{}", law.id),
                holds: quarantines(&rule_ref, severity)?,
                condition: plan.condition,
                predicate: plan.predicate,
                expect: matches!(law.modality, LawModality::Always {}),
                fuel: plan.fuel,
            });
        }
        // S2: the per-row evaluator charges a row one unit per typed
        // property before its first rule. The engine sums every check's
        // fuel for a row, so the first check carries that charge once.
        if let Some(first) = checks.first_mut() {
            first.fuel =
                first
                    .fuel
                    .saturating_add(norn_kernel::snapshot_observe::observation_typed_fuel(
                        &catalog, &name,
                    ));
        }
        Ok(checks)
    }

    /// MANIFEST-LAWS-1: the findings of residue rows, by the per-row
    /// observation path's own evaluation of each row's values
    /// (`snapshot_observe::observation_rule_findings`). LAW-04: only the
    /// rules the engine left to a row are taken from it; the engine's own
    /// decisions of the row's other rules stand.
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
                if !row.rules.contains(&rule) {
                    continue;
                }
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
