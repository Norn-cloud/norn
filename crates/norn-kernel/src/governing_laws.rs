//! Direct governing-law resolution and evaluation on the shared metered expression engine.
use crate::{FactKey, TypeCatalog, cell_host::JsonObject};
use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::{
    ArtifactEnvelope, DataQualityOutcome, LawIR, LawModality, LawResponse, LawRoot, LawSeverity,
    ObjectTypeName,
};
use serde::{Deserialize, de::IntoDeserializer};

/// A retained governing law cannot be evaluated completely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LawEvaluationError {
    /// Inherited or effective-instance laws require B2 resolution.
    #[error("core.laws.unsupported_semantics: law {0} scope unsupported until B2")]
    UnsupportedUntilB2(String),
    /// The exact temporal/response contract is unsupported in CP2.
    #[error("core.laws.unsupported_semantics: law {0} requires unsupported semantics")]
    Unsupported(String),
    /// A retained declaration or subject cannot be resolved.
    #[error("core.laws.unresolved_scope: {0}")]
    Invalid(String),
    /// The cumulative expression budget was exhausted.
    #[error("core.laws.unsupported_semantics: governing-law evaluation budget exhausted")]
    BudgetExceeded,
    /// A native write violates a directly attached safety law.
    #[error("core.laws.violated: {0}")]
    Violated(String),
}
impl LawEvaluationError {
    /// Stable Core 05 diagnostic, independent of diagnostic detail.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedUntilB2(_) | Self::Unsupported(_) | Self::BudgetExceeded => {
                "core.laws.unsupported_semantics"
            }
            Self::Invalid(_) => "core.laws.unresolved_scope",
            Self::Violated(_) => "core.laws.violated",
        }
    }
}
/// One evaluated law violation; findings retain the actual law pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawFailure {
    /// Whether this declared law refuses native writes.
    pub refuses_native: bool,
    /// Declared explanation.
    pub message: String,
}
fn decode(artifact: &ArtifactEnvelope) -> Result<LawIR, LawEvaluationError> {
    let decoded: Result<LawIR, serde::de::value::Error> =
        LawIR::deserialize(artifact.payload.to_json_value().into_deserializer());
    decoded.map_err(|error| LawEvaluationError::Invalid(error.to_string()))
}
impl TypeCatalog<'_> {
    /// LAW-COMPLETE-1: never silently omit an applicable inherited/effective law.
    ///
    /// # Errors
    /// Unknown subject, malformed declaration, or scope unsupported until B2.
    pub fn governing_laws(
        &self,
        object_type: &ObjectTypeName,
    ) -> Result<Vec<&ArtifactEnvelope>, LawEvaluationError> {
        if !self.contains(object_type) && !self.law_artifacts.is_empty() {
            return Err(LawEvaluationError::Invalid(object_type.as_str().to_owned()));
        }
        let is_a = |name: &str| {
            self.lineage(object_type)
                .iter()
                .any(|ty| ty.as_str() == name)
                || self.implements(object_type, name)
        };
        let mut laws = Vec::new();
        for artifact in &self.law_artifacts {
            let law = decode(artifact)?;
            if !is_a(&law.subject) && !law.scope.object_types.iter().any(|ty| is_a(ty)) {
                continue;
            }
            if law.subject != object_type.as_str()
                || !matches!(&law.scope.root, LawRoot::Entity(name) if name == object_type.as_str())
                || law.scope.object_types != [object_type.as_str()]
            {
                return Err(LawEvaluationError::UnsupportedUntilB2(law.id));
            }
            laws.push(*artifact);
        }
        Ok(laws)
    }
}
fn supported(law: &LawIR) -> bool {
    law.timing.is_none()
        && law.scope.lifecycle_states.is_empty()
        && law.scope.commitments.is_empty()
        && matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        && matches!(law.response, LawResponse::Predicate { .. })
}
/// OBS-EVIDENCE-1 / LAW-SUPPORTED-1: evaluate retained direct laws, never caller payloads.
///
/// # Errors
/// Incomplete scope, unsupported semantics, invalid context or exhausted fuel.
pub fn evaluate(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    remaining: &mut u64,
) -> Result<Vec<LawFailure>, LawEvaluationError> {
    let artifacts = catalog.governing_laws(&key.object_type)?;
    if artifacts.is_empty() {
        return Ok(Vec::new());
    }
    let object = values
        .iter()
        .map(|(name, value)| crate::decision_commit::expr_value(value).map(|v| (name.clone(), v)))
        .collect::<Result<_, _>>()
        .map_err(LawEvaluationError::Invalid)?;
    let context = Value::object([("self", Value::Object(object))]);
    let mut failures = Vec::new();
    for artifact in artifacts {
        let law = decode(artifact)?;
        if !supported(&law) {
            return Err(LawEvaluationError::Unsupported(law.id));
        }
        let activation = match &law.condition {
            Some(expr) => boolean(expr.as_str(), &context, remaining)?,
            None => Ok(true),
        };
        let result = match activation {
            Ok(false) => continue,
            Err(outcome) => Err(outcome),
            Ok(true) => {
                let LawResponse::Predicate { expression } = &law.response else {
                    return Err(LawEvaluationError::Unsupported(law.id));
                };
                boolean(expression.as_str(), &context, remaining)?.and_then(|value| {
                    if value == matches!(law.modality, LawModality::Always {}) {
                        Ok(())
                    } else {
                        Err(DataQualityOutcome::NotSatisfied)
                    }
                })
            }
        };
        if result.is_err() {
            failures.push(LawFailure {
                refuses_native: law.severity == LawSeverity::Error,
                message: format!("{}: {}", law.id, law.rationale),
            });
        }
    }
    Ok(failures)
}
fn boolean(
    text: &str,
    context: &Value,
    remaining: &mut u64,
) -> Result<Result<bool, DataQualityOutcome>, LawEvaluationError> {
    if *remaining == 0 {
        return Err(LawEvaluationError::BudgetExceeded);
    }
    *remaining = remaining.saturating_sub(1);
    let expression = match Expression::compile(text) {
        Ok(expr) => expr,
        Err(e) => {
            return Ok(Err(DataQualityOutcome::EvaluationFailed {
                reason: e.to_string(),
            }));
        }
    };
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*remaining));
    *remaining = remaining.saturating_sub(attempt.units_used);
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
