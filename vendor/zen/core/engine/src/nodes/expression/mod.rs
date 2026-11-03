use crate::model::ExpressionNodeContent;
use crate::nodes::result::NodeResult;
use ahash::HashMap;
use std::rc::Rc;

use crate::nodes::context::{NodeContext, NodeContextExt};
use crate::nodes::definition::NodeHandler;
use zen_expression::variable::{ToVariable, Variable};
use zen_types::decision::TransformAttributes;

#[derive(Debug, Clone)]
pub struct ExpressionNodeHandler;

pub type ExpressionNodeData = ExpressionNodeContent;
pub type ExpressionNodeTrace = HashMap<Rc<str>, ExpressionNodeTraceItem>;

impl NodeHandler for ExpressionNodeHandler {
    type NodeData = ExpressionNodeData;
    type TraceData = ExpressionNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let result = Variable::empty_object();
        let mut isolate = ctx.isolate();

        for expression in ctx.node.expressions.iter() {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }

            let binding = ctx.extensions.contracts.as_ref().and_then(|contracts| {
                contracts.expression(&ctx.id, &expression.key, &expression.value)
            });
            isolate.set_meter(ctx.extensions.meter.as_ref().map(|meter| {
                if binding.as_ref().is_some_and(|b| b.exact) {
                    meter.with_exact_arithmetic(None)
                } else {
                    meter.clone()
                }
            }));
            let value = isolate
                .run_standard(
                    binding
                        .as_ref()
                        .map_or(expression.value.as_ref(), |b| b.source.as_str()),
                )
                .with_node_context(&ctx, |error| {
                    format!(
                        r#"Failed to evaluate expression: "{}": {error}"#,
                        expression.value
                    )
                })?;
            ctx.trace(|trace| {
                trace.insert(
                    Rc::from(&*expression.key),
                    ExpressionNodeTraceItem {
                        result: value.clone(),
                    },
                );
            });

            isolate.insert_dollar(&expression.key, value.clone());

            result.dot_insert(&expression.key, value);
        }

        ctx.success(result)
    }
}

#[derive(Debug, Clone, ToVariable)]
pub struct ExpressionNodeTraceItem {
    result: Variable,
}
