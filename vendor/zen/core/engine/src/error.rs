use crate::engine::EvaluationTraceKind;
use crate::loader::LoaderError;
use crate::DecisionGraphValidationError;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;
use zen_types::variable::Variable;

#[derive(Debug, Error)]
#[error("expected {expected} content, got {got}")]
pub struct ContentKindError {
    pub expected: &'static str,
    pub got: &'static str,
}

/// Norn: returned (as the `source` of [`EvaluationError::NodeError`]) when an input or
/// output node declares a JSON schema: node-level schema validation is not part of the
/// engine, so a declared schema fails closed instead of being skipped.
/// `unresolved_reference` is always false.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SchemaCompileError {
    pub message: String,
    pub unresolved_reference: bool,
}

/// Norn: where a [`StrictEvaluationError`] was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictErrorSite {
    /// A decision-table input (predicate) cell.
    DecisionTableInput,
    /// A decision-table output cell of a matching row.
    DecisionTableOutput,
    /// A switch statement condition.
    SwitchCondition,
}

impl fmt::Display for StrictErrorSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StrictErrorSite::DecisionTableInput => "decision table input",
            StrictErrorSite::DecisionTableOutput => "decision table output",
            StrictErrorSite::SwitchCondition => "switch condition",
        })
    }
}

/// Norn (`strict-errors` feature): returned, as the `source` of
/// [`EvaluationError::NodeError`], when a decision-table cell or a switch condition fails
/// to evaluate or a predicate/condition is not a boolean. Upstream (feature off) treats
/// these as a non-match, a missing row result, or a false condition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{site} `{expression}` ({id}) failed: {message}")]
pub struct StrictEvaluationError {
    pub site: StrictErrorSite,
    /// Column id (decision table) or statement id (switch).
    pub id: Arc<str>,
    pub expression: Arc<str>,
    pub message: String,
}

impl StrictEvaluationError {
    pub(crate) fn new(
        site: StrictErrorSite,
        id: &str,
        expression: &str,
        message: impl ToString,
    ) -> Self {
        Self {
            site,
            id: Arc::from(id),
            expression: Arc::from(expression),
            message: message.to_string(),
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EvaluationError {
    #[error("Loader error")]
    LoaderError(LoaderError),

    #[error("{source}")]
    NodeError {
        node_id: Arc<str>,
        trace: Option<Variable>,
        source: Box<dyn std::error::Error>,
    },

    #[error("Depth limit exceeded")]
    DepthLimitExceeded,

    #[error("Invalid graph")]
    InvalidGraph(DecisionGraphValidationError),

    #[error("Validation failed")]
    Validation(Value),

    #[error("expected {expected} content for key '{key}', got {got}")]
    ContentKindMismatch {
        expected: &'static str,
        got: &'static str,
        key: Arc<str>,
    },

    /// Norn `metering`: the evaluation's operation budget ran out.
    #[error("{0}")]
    BudgetExhausted(zen_expression::meter::BudgetExhausted),
}

impl EvaluationError {
    pub fn serialize_with_mode<S>(
        &self,
        serializer: S,
        mode: EvaluationTraceKind,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        match self {
            EvaluationError::DepthLimitExceeded => {
                map.serialize_entry("type", "DepthLimitExceeded")?;
            }
            EvaluationError::NodeError {
                node_id,
                trace,
                source,
            } => {
                map.serialize_entry("type", "NodeError")?;
                map.serialize_entry("source", &source.to_string())?;
                map.serialize_entry("nodeId", &node_id)?;

                if let Some(trace) = &trace {
                    map.serialize_entry("trace", &mode.serialize_trace(trace))?;
                }
            }
            EvaluationError::LoaderError(err) => {
                map.serialize_entry("type", "LoaderError")?;
                match err {
                    LoaderError::Internal { key, source } => {
                        map.serialize_entry("key", key)?;
                        map.serialize_entry("source", &source.to_string())?;
                    }
                    LoaderError::NotFound(key) => {
                        map.serialize_entry("key", key)?;
                    }
                }
            }
            EvaluationError::InvalidGraph(err) => {
                map.serialize_entry("type", "InvalidGraph")?;
                map.serialize_entry("source", err)?;
            }
            EvaluationError::Validation(err) => {
                map.serialize_entry("type", "Validation")?;
                map.serialize_entry("source", err)?;
            }
            EvaluationError::ContentKindMismatch { expected, got, key } => {
                map.serialize_entry("type", "ContentKindMismatch")?;
                map.serialize_entry("expected", expected)?;
                map.serialize_entry("got", got)?;
                map.serialize_entry("key", key)?;
            }
            EvaluationError::BudgetExhausted(err) => {
                map.serialize_entry("type", "BudgetExhausted")?;
                map.serialize_entry("limit", &err.limit)?;
                map.serialize_entry("used", &err.used)?;
            }
        }

        map.end()
    }
}

impl Serialize for EvaluationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.serialize_with_mode(serializer, Default::default())
    }
}

impl From<LoaderError> for Box<EvaluationError> {
    fn from(error: LoaderError) -> Self {
        Box::new(EvaluationError::LoaderError(error))
    }
}

impl From<DecisionGraphValidationError> for Box<EvaluationError> {
    fn from(error: DecisionGraphValidationError) -> Self {
        Box::new(EvaluationError::InvalidGraph(error))
    }
}
