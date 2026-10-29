//! Files-only decision evaluation for differential port verification.
//! This seam never opens a cell, authorizes a write, or dispatches effects.

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate, value_from_runtime};
use norn_expr::EvaluationBudget;
use norn_ir::{ArtifactPayload, RuleRoot, RuntimeValue};

use norn_ir::ArtifactEnvelope;
use serde_json::Value;

/// A checker-admitted decision prepared for repeated offline evaluations.
pub(crate) struct OfflineDecision(AdmittedDecision);

impl OfflineDecision {
    /// Prepare an untriggered `decide` rule from an admitted definition set.
    ///
    /// # Errors
    /// The artifact is absent, malformed, or cannot load into the evaluator.
    pub(crate) fn prepare(
        artifacts: &[ArtifactEnvelope],
        exact_payloads: &BTreeMap<String, RuntimeValue>,
        receipt: &Value,
        receipt_sha256: &str,
        artifact_id: &str,
    ) -> Result<Self, String> {
        let error = |reason: String| format!("{artifact_id}: {reason}");
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == artifact_id)
            .ok_or_else(|| error("artifact is absent from the admitted set".to_owned()))?;
        let ArtifactPayload::Rule(payload) = &artifact.payload else {
            return Err(error("artifact is not a rule".to_owned()));
        };
        let root: RuleRoot = serde_json::from_value(
            serde_json::to_value(payload.to_json_value())
                .map_err(|failure| error(failure.to_string()))?,
        )
        .map_err(|failure| error(failure.to_string()))?;
        // This minimal seam admits standalone graphs. The evaluator refuses
        // unresolved decisionNodes rather than guessing their dependency closure.
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
        AdmittedDecision::admit_with_defaults(&root, &BTreeMap::new(), binding, defaults)
            .map(Self)
            .map_err(|failure| error(failure.to_string()))
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
            .0
            .validate_slice(member("slice")?)
            .map_err(|error| error.to_string())?;
        let event = self
            .0
            .type_event(member("event")?)
            .map_err(|error| error.to_string())?;
        let evaluated = evaluate(&self.0, &slice, &event, EvaluationBudget::new(units))
            .map_err(|error| error.to_string())?;
        let output = norn_ir::runtime::encode(&evaluated.output)?;
        String::from_utf8(output.canonical_json().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }
}
