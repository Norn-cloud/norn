use crate::decision::Decision;
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::error::ContentKindError;
use crate::loader::{DynamicLoader, LoaderResult, NoopLoader};
use crate::model::{DecisionContent, GraphContent};
use crate::EvaluationError;
use serde_json::Value;
use std::fmt::Debug;
use std::sync::Arc;
use strum::{EnumString, IntoStaticStr};
use zen_expression::variable::Variable;

/// Structure used for generating and evaluating JDM decisions
#[derive(Clone)]
pub struct DecisionEngine {
    loader: DynamicLoader,
}

impl Debug for DecisionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionEngine")
            .field("loader", &self.loader)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluationOptions {
    pub trace: bool,
    pub max_depth: u8,
}

impl Default for EvaluationOptions {
    fn default() -> Self {
        Self {
            trace: false,
            max_depth: 10,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluationSerializedOptions {
    pub trace: EvaluationTraceKind,
    pub max_depth: u8,
}

impl Default for EvaluationSerializedOptions {
    fn default() -> Self {
        Self {
            trace: EvaluationTraceKind::None,
            max_depth: 10,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum EvaluationTraceKind {
    #[default]
    None,
    Default,
    String,
    Reference,
    ReferenceString,
}

impl EvaluationTraceKind {
    pub fn serialize_trace(&self, trace: &Variable) -> Value {
        match self {
            EvaluationTraceKind::None => Value::Null,
            EvaluationTraceKind::Default => serde_json::to_value(trace).unwrap_or_default(),
            EvaluationTraceKind::String => {
                Value::String(serde_json::to_string(&trace).unwrap_or_default())
            }
            EvaluationTraceKind::Reference => {
                serde_json::to_value(trace.serialize_ref()).unwrap_or_default()
            }
            EvaluationTraceKind::ReferenceString => {
                Value::String(serde_json::to_string(&trace.serialize_ref()).unwrap_or_default())
            }
        }
    }
}

impl Default for DecisionEngine {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader),
        }
    }
}

impl DecisionEngine {
    pub fn new(loader: DynamicLoader) -> Self {
        Self { loader }
    }

    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self
    }

    /// Evaluates a decision through loader using a key
    pub async fn evaluate<K>(
        &self,
        key: K,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        self.evaluate_with_opts(key, context, Default::default())
            .await
    }

    /// Evaluates a decision through loader using a key with advanced options
    pub async fn evaluate_with_opts<K>(
        &self,
        key: K,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        let content = self.loader.load(key.as_ref()).await?;
        let decision = self.decision_from_graph_arc(content);
        decision.evaluate_with_opts(context, options).await
    }

    pub async fn evaluate_serialized<K>(
        &self,
        key: K,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value>
    where
        K: AsRef<str>,
    {
        let content = self
            .loader
            .load(key.as_ref())
            .await
            .map_err(|err| Value::String(err.to_string()))?;
        let decision = self.decision_from_graph_arc(content);
        decision.evaluate_serialized(context, options).await
    }

    fn decision_from_graph_arc(&self, content: Arc<DecisionContent>) -> Decision {
        let graph: Arc<GraphContent> = match Arc::try_unwrap(content) {
            Ok(DecisionContent::Graph(g)) => g,
            Err(arc) => match arc.as_ref() {
                DecisionContent::Graph(g) => g.clone(),
            },
        };
        self.decision_from_graph(graph)
    }

    fn decision_from_graph(&self, graph: Arc<GraphContent>) -> Decision {
        Decision::from(graph).with_loader(self.loader.clone())
    }

    /// Creates a decision from DecisionContent, exists for easier binding creation
    pub fn create_decision(
        &self,
        content: Arc<DecisionContent>,
    ) -> Result<Decision, ContentKindError> {
        Ok(self.decision_from_graph_arc(content))
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(
        &self,
        key: &str,
    ) -> LoaderResult<Result<Decision, ContentKindError>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }
    pub fn loader(&self) -> DynamicLoader {
        self.loader.clone()
    }
}
