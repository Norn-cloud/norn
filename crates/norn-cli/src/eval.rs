//! Files-only decision evaluation for differential port verification: a
//! rule and its whole sub-decision closure, on the cell's evaluator.
//! This seam never opens a cell, authorizes a write, or dispatches effects.

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate, value_from_runtime};
use norn_expr::EvaluationBudget;
use norn_ir::{ArtifactPayload, RuleRoot, RuntimeValue};

use norn_ir::ArtifactEnvelope;
use serde_json::Value;

/// A checker-admitted decision, with its sub-decision closure, prepared for
/// repeated offline evaluations.
pub(crate) struct OfflineDecision {
    decision: AdmittedDecision,
    closure_digest: String,
}

/// The `RuleRoot` of a rule artifact, as the cell decodes it.
fn rule_root(artifact: &ArtifactEnvelope) -> Result<RuleRoot, String> {
    let error = |reason: String| format!("{}: {reason}", artifact.artifact_id);
    let ArtifactPayload::Rule(payload) = &artifact.payload else {
        return Err(error("artifact is not a rule".to_owned()));
    };
    serde_json::from_value(
        serde_json::to_value(payload.to_json_value())
            .map_err(|failure| error(failure.to_string()))?,
    )
    .map_err(|failure| error(failure.to_string()))
}

impl OfflineDecision {
    /// Prepare an untriggered `decide` rule, and every sub-decision it
    /// reaches, from an admitted definition set.
    ///
    /// # Errors
    /// The rule is absent, its closure does not resolve, or a graph cannot
    /// load into the evaluator.
    pub(crate) fn prepare(
        artifacts: &[ArtifactEnvelope],
        exact_payloads: &BTreeMap<String, RuntimeValue>,
        receipt: &Value,
        receipt_sha256: &str,
        artifact_id: &str,
    ) -> Result<Self, String> {
        let error = |reason: String| format!("{artifact_id}: {reason}");
        // The same pinned-dependency closure admission checked.
        let closure = norn_checker::rule_closure(artifacts, &[], artifact_id)
            .map_err(|failure| error(failure.to_string()))?;
        let root = rule_root(closure.root)?;
        let sub_decisions = closure
            .sub_decisions
            .iter()
            .map(|(key, artifact)| rule_root(artifact).map(|root| ((*key).to_owned(), root)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let binding = DecisionBinding {
            profile_id: receipt
                .get("exprProfile")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(norn_expr::check::CHECKER_PROFILE)
                .to_owned(),
            environment_hash: receipt_sha256.to_owned(),
        };
        let defaults = exact_payloads
            .get(artifact_id)
            .and_then(|payload| payload.get("defaults"))
            .and_then(RuntimeValue::as_object);
        let decision =
            AdmittedDecision::admit_with_defaults(&root, &sub_decisions, binding, defaults)
                .map_err(|failure| error(failure.to_string()))?;
        Ok(Self {
            decision,
            closure_digest: closure.digest,
        })
    }

    /// The Norn1 identity of exactly the rule set this decision evaluates.
    pub(crate) fn closure_digest(&self) -> &str {
        &self.closure_digest
    }

    /// Evaluate one exact JSON input `{slice, event}` and return canonical
    /// candidate effects. These effects are data and are never executed.
    ///
    /// # Errors
    /// Malformed input, schema refusal, evaluation refusal, or encoding failure.
    pub(crate) fn run(&self, input: &str, units: u64) -> Result<String, String> {
        let input = RuntimeValue::from_json_text(input).map_err(|error| error.to_string())?;
        let object = input
            .as_object()
            .ok_or_else(|| "input must be a record of slice and event".to_owned())?;
        if object.len() != 2 {
            return Err("input must contain exactly slice and event".to_owned());
        }
        let member = |name: &str| {
            object
                .get(name)
                .ok_or_else(|| format!("input is missing {name}"))
                .and_then(|value| value_from_runtime(value).map_err(|error| error.to_string()))
        };
        let slice = self
            .decision
            .validate_slice(member("slice")?)
            .map_err(|error| error.to_string())?;
        let event = self
            .decision
            .type_event(member("event")?)
            .map_err(|error| error.to_string())?;
        let evaluated = evaluate(&self.decision, &slice, &event, EvaluationBudget::new(units))
            .map_err(|error| error.to_string())?;
        let output = norn_ir::runtime::encode(&evaluated.output)?;
        String::from_utf8(output.canonical_json().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "fixed fixtures")]

    use super::*;

    const DECIDE: &str =
        include_str!("../../../fixtures/rule-ir-v2/valid/reorder-decision.rule.json");
    const HELPER: &str =
        include_str!("../../../fixtures/rule-ir-v2/valid/reorder-priority.rule.json");
    const INPUT: &str = r#"{"slice":{"sku":{"skuId":"sku-1","onHand":5,"reorderPoint":10,"status":"active"}},"event":{"quantity":100,"unitCost":50}}"#;

    fn envelope(text: &str) -> ArtifactEnvelope {
        serde_json::from_str(text).expect("fixture should decode")
    }

    fn prepare(artifacts: &[ArtifactEnvelope]) -> Result<OfflineDecision, String> {
        OfflineDecision::prepare(
            artifacts,
            &BTreeMap::new(),
            &serde_json::json!({}),
            "sha256:test",
            &artifacts.first().expect("a root").artifact_id,
        )
    }

    /// G-EVAL-CLOSURE: a decision that calls a sub-decision evaluates
    /// offline, exactly as the cell's evaluator admits the same closure.
    #[test]
    fn a_decision_evaluates_with_its_sub_decision_closure() {
        let set = [envelope(DECIDE), envelope(HELPER)];
        let offline = prepare(&set).expect("the closure resolves");
        let output = offline.run(INPUT, 1_000_000).expect("evaluates");
        assert_eq!(offline.run(INPUT, 1_000_000).as_ref(), Ok(&output));
        // The cell's path: the same closure through `AdmittedDecision::admit`.
        let closure = norn_checker::rule_closure(&set, &[], &set[0].artifact_id).expect("closure");
        let subs = closure
            .sub_decisions
            .iter()
            .map(|(key, artifact)| ((*key).to_owned(), rule_root(artifact).expect("rule")))
            .collect();
        let cell = AdmittedDecision::admit(
            &rule_root(closure.root).expect("rule"),
            &subs,
            DecisionBinding {
                profile_id: norn_expr::check::CHECKER_PROFILE.to_owned(),
                environment_hash: "sha256:test".to_owned(),
            },
        )
        .expect("admits");
        let cell = OfflineDecision {
            decision: cell,
            closure_digest: closure.digest.clone(),
        };
        assert_eq!(cell.run(INPUT, 1_000_000), Ok(output));
        assert_eq!(offline.closure_digest(), closure.digest);
    }

    #[test]
    fn a_missing_sub_decision_is_refused_not_guessed() {
        let refused = prepare(&[envelope(DECIDE)]).err().expect("refused");
        assert!(
            refused.contains("retail.reorder_priority.rule"),
            "{refused}"
        );
    }
}
