use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::loader::{DynamicLoader, NoopLoader};
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use serde_json::Value;
use std::sync::Arc;
use zen_expression::variable::Variable;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<GraphContent>,
    loader: DynamicLoader,
}

impl From<GraphContent> for Decision {
    fn from(value: GraphContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader),
        }
    }
}

impl From<Arc<GraphContent>> for Decision {
    fn from(value: Arc<GraphContent>) -> Self {
        Self {
            content: value,
            loader: Arc::new(NoopLoader),
        }
    }
}

impl Decision {
    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self
    }

    /// Evaluates a decision using an in-memory reference stored in struct
    pub async fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        self.evaluate_with_opts(context, Default::default()).await
    }

    /// Evaluates a decision using in-memory reference with advanced options
    pub async fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut decision_graph = DecisionGraph::try_new(self.graph_config(options))?;

        let response = decision_graph.evaluate(context).await?;

        Ok(response)
    }

    /// Norn `metering`: evaluates like [`Decision::evaluate_with_opts`], charging every
    /// VM opcode, expensive builtin, graph node visit and decision-table row to `meter`.
    /// Fails with [`EvaluationError::BudgetExhausted`] when it runs out. Sub-decisions
    /// share the meter. Cost does not depend on `options.trace` or precompilation.
    pub async fn evaluate_metered(
        &self,
        context: Variable,
        options: EvaluationOptions,
        meter: zen_expression::meter::Meter,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut config = self.graph_config(options);
        config.extensions.meter = Some(meter);
        let mut decision_graph = DecisionGraph::try_new(config)?;

        let response = decision_graph.evaluate(context).await?;

        Ok(response)
    }

    fn graph_config(&self, options: EvaluationOptions) -> DecisionGraphConfig {
        DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth,
            trace: options.trace,
            iteration: 0,
            extensions: NodeHandlerExtensions {
                loader: self.loader.clone(),
                compiled_cache: self.content.compiled_cache.clone(),
                ..Default::default()
            },
        }
    }

    pub async fn evaluate_serialized(
        &self,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value> {
        let response = self
            .evaluate_with_opts(
                context,
                EvaluationOptions {
                    trace: options.trace != EvaluationTraceKind::None,
                    max_depth: options.max_depth,
                },
            )
            .await;

        match response {
            Ok(ok) => Ok(ok
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
            Err(err) => Err(err
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
        }
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: 1,
            trace: false,
            iteration: 0,
            extensions: Default::default(),
        })?;

        decision_graph.validate()
    }

    pub fn compile(&mut self) {
        let cm = Arc::make_mut(&mut self.content);
        cm.compile();
    }
}
