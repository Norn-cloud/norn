//! LAW-EXAMPLE-1 (#1176 unit B): a checked example of a law or norm.
//!
//! An authored `example` names a law (or a norm's law), a subject value and the expected
//! verdict. [`evaluate`] gives the verdict the checker's own semantics assign: the condition,
//! then the exception tree ([`crate::law_exceptions::fold`]), then the response under the
//! modality. The authoring layer runs every example at check time and reports a mismatch with
//! [`codes::MISMATCH`]. Examples are evidence about one valuation only; they prove nothing about
//! others and grant nothing.

use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::{LawIR, LawModality, LawResponse, RuntimeValue};

/// Stable `law.example.*` codes.
pub mod codes {
    /// The verdict differs from the example's expected verdict.
    pub const MISMATCH: &str = "law.example.mismatch";
    /// The law cannot be evaluated on one valuation (timing, a non-predicate response,
    /// a malformed exception tree, or an expression that does not evaluate to a Boolean).
    pub const UNSUPPORTED: &str = "law.example.unsupported";
}

/// The verdict of a law on one subject value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The condition does not hold: the law does not apply.
    NotApplicable,
    /// An exception lifts the law; `applied` are the applied exception ids, sorted.
    Excepted {
        /// Exceptions that applied.
        applied: Vec<String>,
    },
    /// The law applies and its response holds under the modality.
    Holds,
    /// The law applies and its response fails under the modality.
    Violated,
}

impl Verdict {
    /// `{"verdict": "not_applicable" | "excepted" | "holds" | "violated", "applied"?: [...]}`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::NotApplicable => serde_json::json!({"verdict": "not_applicable"}),
            Self::Excepted { applied } => {
                serde_json::json!({"verdict": "excepted", "applied": applied})
            }
            Self::Holds => serde_json::json!({"verdict": "holds"}),
            Self::Violated => serde_json::json!({"verdict": "violated"}),
        }
    }
}

fn boolean(source: &str, context: &Value) -> Result<bool, String> {
    let expression = Expression::compile(source).map_err(|error| error.to_string())?;
    let evaluated = expression
        .evaluate(context, EvaluationBudget::default())
        .map_err(|error| error.to_string())?;
    match evaluated.value() {
        Value::Bool(value) => Ok(*value),
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Err(format!("{source:?} is not Boolean"))
        }
    }
}

/// The checker's verdict of `law` on the subject value `subject` (bound as `self`).
///
/// # Errors
/// A [`codes::UNSUPPORTED`] reason: a timed or progress law, a non-predicate response, an
/// invalid exception tree, or an expression that does not evaluate to a Boolean.
pub fn evaluate(law: &LawIR, subject: &serde_json::Value) -> Result<Verdict, String> {
    if law.timing.is_some() {
        return Err("a timed law needs a run, not one state".into());
    }
    let expected = match law.modality {
        LawModality::Always {} => true,
        LawModality::Never {} => false,
        LawModality::Eventually {} | LawModality::Until { .. } => {
            return Err("a progress law needs a run, not one state".into());
        }
    };
    let LawResponse::Predicate { expression } = &law.response else {
        return Err("only a predicate response is evaluated on one value".into());
    };
    crate::law_exceptions::check_tree(&law.id, &law.exceptions)
        .map_err(|refusal| refusal.code().to_owned())?;
    let runtime: RuntimeValue =
        serde_json::from_value(subject.clone()).map_err(|error| error.to_string())?;
    let value = Value::try_from(&runtime).map_err(|error| error.to_string())?;
    let context = Value::object([("self", value)]);
    if let Some(condition) = &law.condition
        && !boolean(condition.as_str(), &context)?
    {
        return Ok(Verdict::NotApplicable);
    }
    let matched = law
        .exceptions
        .iter()
        .map(|exception| boolean(exception.condition.as_str(), &context))
        .collect::<Result<Vec<_>, _>>()?;
    let (lifted, applied) = crate::law_exceptions::fold(&law.id, &law.exceptions, &matched);
    if lifted {
        return Ok(Verdict::Excepted {
            applied: applied.into_iter().map(str::to_owned).collect(),
        });
    }
    Ok(if boolean(expression.as_str(), &context)? == expected {
        Verdict::Holds
    } else {
        Verdict::Violated
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "fixed laws")]
mod tests {
    use super::*;
    use serde_json::json;

    fn credit_limit() -> LawIR {
        serde_json::from_value(json!({
            "lawVersion": "0.1", "id": "shop.Order.credit_limit", "owner": "shop", "severity": "error",
            "rationale": "Exposure stays within the limit.", "provenance": {"source": "authoring_sdk"},
            "scope": {"root": {"kind": "entity", "id": "shop.Order"}, "objectTypes": ["shop.Order"]},
            "subject": "shop.Order", "condition": "self.status == 'confirmed'",
            "modality": {"kind": "always"},
            "response": {"kind": "predicate", "expression": "self.total <= self.credit_limit"},
            "exceptions": [
                {"id": "vip", "overrides": "shop.Order.credit_limit", "condition": "self.tier == 'vip'", "rationale": "Board", "source": {"artifactId": "x", "pointer": "/payload/exceptions/0"}},
                {"id": "huge", "overrides": "vip", "condition": "self.total > 1000000", "rationale": "Too large", "source": {"artifactId": "x", "pointer": "/payload/exceptions/1"}}
            ]
        }))
        .expect("law")
    }

    fn order(status: &str, total: i64, tier: &str) -> serde_json::Value {
        json!({"status": status, "total": total, "credit_limit": 500, "tier": tier})
    }

    #[test]
    fn each_verdict_follows_the_condition_the_tree_and_the_response() {
        let law = credit_limit();
        assert_eq!(
            evaluate(&law, &order("draft", 900, "standard")),
            Ok(Verdict::NotApplicable)
        );
        assert_eq!(
            evaluate(&law, &order("confirmed", 400, "standard")),
            Ok(Verdict::Holds)
        );
        assert_eq!(
            evaluate(&law, &order("confirmed", 900, "standard")),
            Ok(Verdict::Violated)
        );
        assert_eq!(
            evaluate(&law, &order("confirmed", 900, "vip")),
            Ok(Verdict::Excepted {
                applied: vec!["vip".into()]
            })
        );
        // The nested exception re-applies the law to a huge VIP order.
        assert_eq!(
            evaluate(&law, &order("confirmed", 2_000_000, "vip")),
            Ok(Verdict::Violated)
        );
    }

    #[test]
    fn progress_laws_and_non_boolean_expressions_are_unsupported() {
        let mut eventually = credit_limit();
        eventually.modality = LawModality::Eventually {};
        assert!(evaluate(&eventually, &order("confirmed", 1, "vip")).is_err());
        let mut text = credit_limit();
        text.condition = Some(norn_ir::ZenExpression("self.status".into()));
        assert!(evaluate(&text, &order("confirmed", 1, "vip")).is_err());
    }
}
