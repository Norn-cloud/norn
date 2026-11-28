//! Metered governing-law evaluation over subject values, linked targets, and
//! §8.1 `unless` exception trees.

use super::{
    AppliedLawException, BoundInvariantFailure, LawEvaluationError, LawEvaluationReport, LinkRead,
    LinkTargets, bind_invariant_failure, decode, expr_object, invariant_binding, law_failure,
    law_link_reads, law_operands, pin, supported,
};
use crate::{FactKey, TypeCatalog, cell_host::JsonObject};
use norn_expr::{TurnFuel, Value};
use norn_ir::{ArtifactEnvelope, DataQualityOutcome, LawIR, LawModality, LawResponse};

/// The link reads one law evaluation bound: each read's pin, and the values
/// of every to-one target and inverse-role source, by link or role.
#[derive(Default)]
struct BoundLinks {
    pinned: std::collections::BTreeMap<String, norn_ir::FactRef>,
    targets: std::collections::BTreeMap<String, JsonObject>,
    sources: std::collections::BTreeMap<String, Vec<JsonObject>>,
}

/// LAW-LINK-1 / LAW-LINK-MANY-1: bind each link a law reads into the
/// subject's expression object: a to-one target (or `null`), or an inverse
/// role's sources as a list ordered by key, each pinned at the version read.
fn bind_links(
    key: &FactKey,
    reads: Vec<LinkRead>,
    links: &mut dyn LinkTargets,
    object: &mut std::collections::BTreeMap<String, Value>,
) -> Result<BoundLinks, LawEvaluationError> {
    let mut bound = BoundLinks::default();
    for read in reads {
        match read {
            LinkRead::Forward(link) => {
                let value = match links.target(&link)? {
                    Some(target) => {
                        bound.targets.insert(link.clone(), target.values.clone());
                        bound.pinned.insert(
                            link.clone(),
                            norn_ir::FactRef::new(
                                target.key.object_type.as_str(),
                                target.key.key.clone(),
                                target.version,
                            ),
                        );
                        expr_object(&target.values)?
                    }
                    None => Value::Null,
                };
                object.insert(link, value);
            }
            LinkRead::Inverse { role, source, link } => {
                let mut found = links.sources(&role, &source, &link)?;
                if found.len() > crate::commit_check::MAX_LINK_LAW_SUBJECTS {
                    return Err(LawEvaluationError::LinkFanOutExceeded {
                        target: format!("{}/{}", key.object_type.as_str(), key.key),
                        limit: crate::commit_check::MAX_LINK_LAW_SUBJECTS,
                    });
                }
                found.sort_by(|left, right| left.key.key.cmp(&right.key.key));
                found.dedup_by(|left, right| left.key == right.key);
                let mut items = Vec::with_capacity(found.len());
                let mut rows = Vec::with_capacity(found.len());
                for item in found {
                    // Each source is pinned at the exact version read, by
                    // `role/key`, so a retained finding replays the same set.
                    bound.pinned.insert(
                        format!("{role}/{}", item.key.key),
                        norn_ir::FactRef::new(
                            item.key.object_type.as_str(),
                            item.key.key.clone(),
                            item.version,
                        ),
                    );
                    items.push(expr_object(&item.values)?);
                    rows.push(item.values);
                }
                bound.sources.insert(role.clone(), rows);
                object.insert(role, Value::Array(items));
            }
        }
    }
    Ok(bound)
}

struct EvalSubject<'a> {
    catalog: &'a TypeCatalog<'a>,
    key: &'a FactKey,
    values: &'a JsonObject,
    subject: &'a std::collections::BTreeMap<String, Value>,
}

pub(super) fn evaluate_linked_with_audit(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    links: &mut dyn LinkTargets,
    remaining: &mut TurnFuel,
) -> Result<LawEvaluationReport, LawEvaluationError> {
    let artifacts = catalog.governing_laws(&key.object_type)?;
    if artifacts.is_empty() {
        return Ok(LawEvaluationReport::default());
    }
    let Value::Object(subject) = expr_object(values)? else {
        return Err(LawEvaluationError::Invalid("subject values".into()));
    };
    let eval = EvalSubject {
        catalog,
        key,
        values,
        subject: &subject,
    };
    let mut report = LawEvaluationReport::default();
    for artifact in artifacts {
        evaluate_one_law(&eval, artifact, links, remaining, &mut report)?;
    }
    Ok(report)
}

fn evaluate_one_law(
    eval: &EvalSubject<'_>,
    artifact: &ArtifactEnvelope,
    links: &mut dyn LinkTargets,
    remaining: &mut TurnFuel,
    report: &mut LawEvaluationReport,
) -> Result<(), LawEvaluationError> {
    let law = decode(eval.catalog, artifact)?;
    if !supported(law) {
        return Err(LawEvaluationError::Unsupported(law.id.clone()));
    }
    if !law.exceptions.is_empty() {
        norn_checker::law_exceptions::check_tree(&law.id, &law.exceptions).map_err(|refusal| {
            LawEvaluationError::Invalid(format!("{}: {}", law.id, refusal.code()))
        })?;
    }
    let reads = law_link_reads(eval.catalog, &eval.key.object_type, law)?;
    if !reads.is_empty() && links.defers() {
        return Ok(());
    }
    let mut object = eval.subject.clone();
    let bound = bind_links(eval.key, reads, links, &mut object)?;
    let context = Value::object([("self", Value::Object(object))]);
    let invariant_id = invariant_binding(eval.catalog, &eval.key.object_type, law)?;
    let mut invariant_failure = None;
    let activation = match &law.condition {
        Some(expr) => boolean(eval.catalog, expr.as_str(), &context, remaining)?,
        None => Ok(true),
    };
    let result = match activation {
        Ok(false) => return Ok(()),
        Err(outcome) => Err(outcome),
        Ok(true) => match exception_lifts(eval.catalog, law, &context, remaining)? {
            Ok(Some(applied_exception_ids)) => {
                report.applied_exceptions.push(AppliedLawException {
                    rule_ref: pin(artifact, law)?,
                    applied_exception_ids,
                });
                return Ok(());
            }
            Err(outcome) => Err(outcome),
            Ok(None) => evaluate_response(
                eval,
                law,
                &context,
                invariant_id,
                remaining,
                &mut invariant_failure,
            )?,
        },
    };
    if let Err(outcome) = result {
        let mut failure = law_failure(artifact, law, outcome, invariant_failure)?;
        failure.values = law_operands(law, eval.values, &bound.targets, &bound.sources);
        failure.links = bound.pinned;
        report.failures.push(failure);
    }
    Ok(())
}

fn evaluate_response(
    eval: &EvalSubject<'_>,
    law: &LawIR,
    context: &Value,
    invariant_id: Option<&str>,
    remaining: &mut TurnFuel,
    invariant_failure: &mut Option<Box<BoundInvariantFailure>>,
) -> Result<Result<(), DataQualityOutcome>, LawEvaluationError> {
    match &law.response {
        LawResponse::Predicate { expression } => {
            Ok(
                boolean(eval.catalog, expression.as_str(), context, remaining)?.and_then(|value| {
                    if value == matches!(law.modality, LawModality::Always {}) {
                        Ok(())
                    } else {
                        Err(DataQualityOutcome::NotSatisfied)
                    }
                }),
            )
        }
        LawResponse::Invariant { .. } => {
            let id = invariant_id.ok_or_else(|| LawEvaluationError::Invalid(law.id.clone()))?;
            let failures = crate::commit_check::evaluate_named_invariant(
                eval.catalog,
                eval.key,
                eval.values,
                remaining,
                id,
            )
            .map_err(|error| match error {
                crate::commit_check::InvariantEvaluationError::BudgetExceeded => {
                    LawEvaluationError::BudgetExceeded
                }
                other @ (crate::commit_check::InvariantEvaluationError::InvalidEffectiveValues(
                    _,
                )
                | crate::commit_check::InvariantEvaluationError::MissingInterface(_)) => {
                    LawEvaluationError::Invalid(format!("{other:?}"))
                }
            })?;
            if let Some(failure) = failures.into_iter().next() {
                let outcome = failure.outcome.clone();
                *invariant_failure = Some(bind_invariant_failure(eval.catalog, failure)?);
                Ok(Err(outcome))
            } else {
                Ok(Ok(()))
            }
        }
        LawResponse::Action { .. }
        | LawResponse::Transition { .. }
        | LawResponse::TranslationPreserves { .. } => {
            Err(LawEvaluationError::Unsupported(law.id.clone()))
        }
    }
}

/// Evaluate an admitted exception tree under `remaining` fuel and fold it with
/// `norn_checker::law_exceptions::fold`; evaluation errors fail closed.
/// Returns `Ok(Some(applied_ids))` (sorted) when the law is lifted, or
/// `Ok(None)` when no exception lifts it.
fn exception_lifts(
    catalog: &TypeCatalog<'_>,
    law: &LawIR,
    context: &Value,
    remaining: &mut TurnFuel,
) -> Result<Result<Option<Vec<String>>, DataQualityOutcome>, LawEvaluationError> {
    if law.exceptions.is_empty() {
        return Ok(Ok(None));
    }
    let mut matched = Vec::with_capacity(law.exceptions.len());
    for exception in &law.exceptions {
        match boolean(catalog, exception.condition.as_str(), context, remaining)? {
            Ok(value) => matched.push(value),
            Err(outcome) => return Ok(Err(outcome)),
        }
    }
    let (lifted, applied) = norn_checker::law_exceptions::fold(&law.id, &law.exceptions, &matched);
    if lifted {
        Ok(Ok(Some(applied.into_iter().map(str::to_owned).collect())))
    } else {
        Ok(Ok(None))
    }
}

fn boolean(
    catalog: &TypeCatalog<'_>,
    text: &str,
    context: &Value,
    remaining: &mut TurnFuel,
) -> Result<Result<bool, DataQualityOutcome>, LawEvaluationError> {
    if remaining.charge(1).is_err() {
        return Err(LawEvaluationError::BudgetExceeded);
    }
    let expression = match catalog.compiled_expression(text) {
        Ok(expr) => expr,
        Err(reason) => {
            return Ok(Err(DataQualityOutcome::EvaluationFailed { reason }));
        }
    };
    let attempt = remaining.spend(|budget| expression.evaluate_metered(context, budget));
    Ok(match attempt.result {
        Err(norn_expr::ExprError::BudgetExhausted { .. }) => {
            return Err(LawEvaluationError::BudgetExceeded);
        }
        Err(e) => Err(DataQualityOutcome::EvaluationFailed {
            reason: e.to_string(),
        }),
        Ok(result) => match result.value() {
            Value::Bool(value) => Ok(*value),
            other @ (Value::Null
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_)) => Err(DataQualityOutcome::NonBoolean {
                actual: format!("{other:?}"),
            }),
        },
    })
}
