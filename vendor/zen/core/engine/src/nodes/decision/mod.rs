use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig};
use crate::nodes::{NodeContext, NodeContextExt, NodeError, NodeHandler, NodeResult};
use crate::EvaluationError;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use zen_types::decision::{DecisionNodeContent, TransformAttributes};
use zen_types::variable::{ToVariable, Variable};

#[derive(Debug, Clone, Default)]
pub struct DecisionNodeHandler {
    decision_graph: Rc<RefCell<Option<DecisionGraph>>>,
}

pub type DecisionNodeData = DecisionNodeContent;
pub type DecisionNodeTrace = Variable;

impl NodeHandler for DecisionNodeHandler {
    type NodeData = DecisionNodeData;
    type TraceData = DecisionNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    async fn after_transform_attributes(
        &self,
        _ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Result<(), NodeError> {
        if let Some(graph) = self.decision_graph.borrow_mut().as_mut() {
            graph.reset_graph();
        };

        Ok(())
    }

    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "Norn #3404: each sub-decision node owns its handler and the graph future is driven to completion without re-entering it"
    )]
    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        if let Some(contracts) = &ctx.extensions.contracts {
            if let Some(result) = contracts.derive_with_tzdb(
                &ctx.node.key,
                &ctx.input,
                ctx.extensions.meter.as_ref(),
                ctx.extensions.tzdb.as_ref(),
            ) {
                return result
                    .node_context(&ctx)
                    .and_then(|value| ctx.success(value));
            }
        }
        // Norn (#1708): the child graph runs on the value the host carries across,
        // and a host-shaped child sees no caller node data through `$nodes`.
        let shaped = cross(&ctx, &ctx.input, true)?;
        let shaped_input = shaped.is_some();
        let input = shaped.unwrap_or_else(|| ctx.input.clone());
        let mut decision_graph_ref = self.decision_graph.borrow_mut();

        if decision_graph_ref.is_none() {
            let loader = ctx.extensions.loader();
            let sub_decision = loader.load(ctx.node.key.deref()).await.node_context(&ctx)?;
            let sub_kind = sub_decision.kind();
            let Some(sub_graph) = sub_decision.into_graph_arc() else {
                return ctx.error(format!(
                    "sub-decision '{}' is a {sub_kind}, expected graph",
                    ctx.node.key
                ));
            };

            let sub_graph = {
                let mut owned = (*sub_graph).clone();
                owned.compile();
                std::sync::Arc::new(owned)
            };

            let mut extensions = ctx.extensions.clone();
            extensions.compiled_cache = sub_graph.compiled_cache.clone();

            let dg = DecisionGraph::try_new(DecisionGraphConfig {
                content: sub_graph,
                extensions,
                trace: ctx.config.trace,
                iteration: ctx.iteration + 1,
                max_depth: ctx.config.max_depth,
            })
            .node_context(&ctx)?;

            *decision_graph_ref = Some(dg);
        }

        let Some(decision_graph) = decision_graph_ref.as_mut() else {
            return ctx.error("Failed to initialize decision graph".to_string());
        };

        decision_graph.set_parent_nodes(if shaped_input {
            None
        } else {
            ctx.nodes.clone()
        });

        let evaluate_result = Box::pin(decision_graph.evaluate(input)).await;
        match evaluate_result {
            Ok(mut result) => {
                if let Some(shaped) = cross(&ctx, &result.result, false)? {
                    result.result = shaped;
                }
                ctx.trace(|trace| {
                    *trace = result
                        .trace
                        .and_then(|t| t.into_graph())
                        .as_ref()
                        .map(|m| m.to_variable())
                        .unwrap_or(Variable::Null);
                });

                ctx.success(result.result)
            }
            Err(err) => {
                if let EvaluationError::NodeError { trace, .. } = err.deref() {
                    ctx.trace(|t| *t = trace.to_variable());
                }

                // Norn: keep the child error in the source chain (same message as the
                // upstream `to_string()`), so callers can downcast through it, e.g. to
                // `StrictEvaluationError`.
                ctx.error(*err)
            }
        }
    }
}

/// Norn (#1708): checks `value` at the sub-decision boundary of `ctx`
/// ([`DecisionContracts::cross`](crate::nodes::DecisionContracts::cross)) and
/// returns the host-shaped value to carry across, if any.
fn cross(
    ctx: &NodeContext<DecisionNodeData, DecisionNodeTrace>,
    value: &Variable,
    input: bool,
) -> Result<Option<Variable>, NodeError> {
    match &ctx.extensions.contracts {
        None => Ok(None),
        Some(contracts) => contracts
            .cross(&ctx.node.key, value, input, ctx.extensions.meter.as_ref())
            .node_context(ctx),
    }
}
