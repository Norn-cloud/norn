use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContext, NodeContextExt, NodeResponse};
use crate::{StrictErrorSite, StrictEvaluationError};
use ahash::HashMap;
use serde::Serialize;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::variable::ToVariable;
use zen_expression::Isolate;
use zen_types::decision::{
    DecisionTableContent, DecisionTableHitPolicy, DecisionTableInputField, TransformAttributes,
};
use zen_types::variable::Variable;

#[derive(Debug, Clone)]
pub struct DecisionTableNodeHandler;

pub type DecisionTableNodeData = DecisionTableContent;

type DecisionTableContext = NodeContext<DecisionTableNodeData, DecisionTableNodeTrace>;

impl NodeHandler for DecisionTableNodeHandler {
    type NodeData = DecisionTableNodeData;
    type TraceData = DecisionTableNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let has_collect_columns = ctx.node.outputs.iter().any(|output| output.write_path().1);
        match ctx.node.hit_policy {
            DecisionTableHitPolicy::First if has_collect_columns => {
                self.handle_first_hit_collect(ctx)
            }
            DecisionTableHitPolicy::First => self.handle_first_hit(ctx),
            DecisionTableHitPolicy::Collect => self.handle_collect(ctx),
        }
    }
}

impl DecisionTableNodeHandler {
    fn handle_first_hit(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = ctx.isolate();

        if !ctx.config.trace {
            for rule in ctx.node.rules.iter() {
                if let Some(RowResult::Output(output)) = self
                    .evaluate_row(&ctx, rule, &mut isolate)
                    .node_context(&ctx)?
                {
                    return ctx.success(output);
                }
            }
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let mut hit = None;
        for (index, rule) in ctx.node.rules.iter().enumerate() {
            let row = self
                .evaluate_row(&ctx, rule, &mut isolate)
                .node_context(&ctx)?;
            hit = match row {
                None => continue,
                Some(RowResult::WithTrace {
                    output,
                    explanation,
                }) => Some((index, output, explanation)),
                Some(RowResult::Output(output)) => Some((index, output, Default::default())),
            };
            break;
        }

        match hit {
            Some((index, output, explanation)) => {
                ctx.trace(|t| *t = DecisionTableNodeTrace::FirstHit(explanation.into_trace(index)));
                ctx.success(output)
            }
            None => Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            }),
        }
    }

    fn handle_collect(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut outputs = Vec::new();
        let mut traces = Vec::new();
        let mut isolate = ctx.isolate();

        for (index, rule) in ctx.node.rules.iter().enumerate() {
            if let Some(result) = self
                .evaluate_row(&ctx, rule, &mut isolate)
                .node_context(&ctx)?
            {
                match result {
                    RowResult::Output(output) => {
                        outputs.push(output);
                    }
                    RowResult::WithTrace {
                        output,
                        explanation,
                    } => {
                        outputs.push(output);
                        traces.push(explanation.into_trace(index));
                    }
                }
            }
        }

        ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });

        ctx.success(Variable::from_array(outputs))
    }

    /// Norn: evaluates one input cell. `Err` means the cell failed to evaluate or did not
    /// produce a boolean; the error aborts the table (strict errors; upstream treated
    /// both as a non-match).
    fn evaluate_cell(
        input: &DecisionTableInputField,
        rule_value: &str,
        isolate: &mut Isolate,
    ) -> Result<bool, StrictEvaluationError> {
        let error = |expression: &str, message: String| {
            StrictEvaluationError::new(
                StrictErrorSite::DecisionTableInput,
                &input.id,
                expression,
                message,
            )
        };
        match &input.field {
            None => {
                let result = isolate
                    .run_standard(rule_value)
                    .map_err(|err| error(rule_value, err.to_string()))?;
                result.as_bool().ok_or_else(|| {
                    error(
                        rule_value,
                        format!("expected a boolean, got {}", result.type_name()),
                    )
                })
            }
            Some(field) => {
                isolate
                    .set_reference(field)
                    .map_err(|err| error(field, err.to_string()))?;
                isolate
                    .run_unary(rule_value)
                    .map_err(|err| error(rule_value, err.to_string()))
            }
        }
    }

    fn handle_first_hit_collect(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = ctx.isolate();

        let mut scalars: Option<Variable> = None;
        let mut collected: Vec<Vec<Variable>> = vec![Vec::new(); ctx.node.outputs.len()];
        let mut matched = false;
        let mut traces = Vec::new();

        for (row_idx, rule) in ctx.node.rules.iter().enumerate() {
            if !Self::row_matches(&ctx, rule, &mut isolate).node_context(&ctx)? {
                continue;
            }

            let Some((row_scalars, row_collects)) =
                Self::evaluate_row_cells(&ctx, rule, &mut isolate, scalars.is_none())
                    .node_context(&ctx)?
            else {
                continue;
            };
            matched = true;
            if let Some(row_scalars) = row_scalars {
                scalars = Some(row_scalars);
            }
            for (column_idx, value) in row_collects {
                collected[column_idx].push(value);
            }

            if ctx.config.trace {
                traces.push(Self::row_explanation(&ctx, rule, &isolate).into_trace(row_idx));
            }
        }

        if !matched {
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let output = scalars.unwrap_or_else(Variable::empty_object);
        for (column_idx, column) in ctx.node.outputs.iter().enumerate() {
            let (path, collect) = column.write_path();
            if !collect || path.is_empty() {
                continue;
            }
            let values = std::mem::take(&mut collected[column_idx]);
            output.dot_insert(path, Variable::from_array(values));
        }

        ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });
        ctx.success(output)
    }

    fn row_matches(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
    ) -> Result<bool, StrictEvaluationError> {
        // Norn `metering`: one unit per row considered. On exhaustion the row does not
        // match; the graph evaluator turns the exhausted meter into a typed error.
        if let Some(meter) = &ctx.extensions.meter {
            if meter
                .charge(zen_expression::meter::cost::TABLE_ROW)
                .is_err()
            {
                return Ok(false);
            }
        }
        for input in ctx.node.inputs.iter() {
            let Some(rule_value) = rule.get(&input.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let passed = Self::evaluate_cell(input, rule_value, isolate)?;
            if !passed {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Norn: evaluates one output cell. A failure aborts the table (strict errors;
    /// upstream dropped the whole row result).
    fn evaluate_output(
        output: &zen_types::decision::DecisionTableOutputField,
        rule_value: &str,
        isolate: &mut Isolate,
    ) -> Result<Variable, StrictEvaluationError> {
        isolate.run_standard(rule_value).map_err(|err| {
            StrictEvaluationError::new(
                StrictErrorSite::DecisionTableOutput,
                &output.id,
                rule_value,
                err,
            )
        })
    }

    fn evaluate_row_cells(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        include_scalars: bool,
    ) -> Result<Option<(Option<Variable>, Vec<(usize, Variable)>)>, StrictEvaluationError> {
        let scalars = include_scalars.then(Variable::empty_object);
        let mut collects = Vec::new();
        for (column_idx, output) in ctx.node.outputs.iter().enumerate() {
            let (path, collect) = output.write_path();
            if path.is_empty() || (!collect && !include_scalars) {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let value = Self::evaluate_output(output, rule_value, isolate)?.deep_clone();
            if collect {
                collects.push((column_idx, value));
            } else if let Some(scalars) = &scalars {
                scalars.dot_insert(path, value);
            }
        }
        Ok(Some((scalars, collects)))
    }

    /// Norn (#1711): a row's explanation is built only from selector values observed
    /// while the row was matched (the isolate's reference cache). A selector whose cell
    /// is blank or absent was skipped by matching, so it is listed as unevaluated and never run
    /// here: building a trace does no work outside the meter and cannot fail.
    fn row_explanation(
        ctx: &DecisionTableContext,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &Isolate,
    ) -> RowExplanation {
        let id_str = Rc::<str>::from("_id");
        let description_str = Rc::<str>::from("_description");

        let rule_id = match rule.get(id_str.as_ref()) {
            Some(rid) => Rc::<str>::from(rid.deref()),
            None => Rc::from(""),
        };

        let mut explanation = RowExplanation::default();
        explanation.rule.insert(id_str.clone(), rule_id.clone());
        if let Some(description) = rule.get(description_str.as_ref()) {
            explanation
                .rule
                .insert(description_str.clone(), Rc::from(description.deref()));
        }

        for input in ctx.node.inputs.iter() {
            let Some(input_field) = &input.field else {
                continue;
            };
            // Matching skips an absent cell and a blank one alike: neither is observed.
            let cell = rule.get(input.id.deref()).filter(|value| !value.is_empty());
            match cell.and_then(|_| isolate.get_reference(input_field.deref())) {
                Some(reference) => {
                    explanation
                        .reference_map
                        .insert(Rc::from(input_field.deref()), reference);
                }
                None => explanation.unevaluated.push(Rc::from(input_field.deref())),
            }

            let Some(rule_value) = rule.get(input.id.deref()) else {
                continue;
            };
            let input_identifier = format!("{input_field}[{}]", input.id);
            explanation.rule.insert(
                Rc::from(input_identifier.as_str()),
                Rc::from(rule_value.deref()),
            );
        }

        explanation
    }

    fn evaluate_row<'a>(
        &self,
        ctx: &'a DecisionTableContext,
        rule: &'a HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
    ) -> Result<Option<RowResult>, StrictEvaluationError> {
        if !Self::row_matches(ctx, rule, isolate)? {
            return Ok(None);
        }

        let outputs = Variable::empty_object();
        for output in ctx.node.outputs.iter() {
            let (path, _) = output.write_path();
            if path.is_empty() {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let res = Self::evaluate_output(output, rule_value, isolate)?;
            outputs.dot_insert(path, res.deep_clone());
        }

        if !ctx.config.trace {
            return Ok(Some(RowResult::Output(outputs)));
        }

        Ok(Some(RowResult::WithTrace {
            output: outputs.to_variable(),
            explanation: Self::row_explanation(ctx, rule, isolate),
        }))
    }
}

enum RowResult {
    Output(Variable),
    WithTrace {
        output: Variable,
        explanation: RowExplanation,
    },
}

/// Norn (#1711): what a matched row's trace records, observed while matching.
#[derive(Default)]
struct RowExplanation {
    reference_map: HashMap<Rc<str>, Variable>,
    rule: HashMap<Rc<str>, Rc<str>>,
    unevaluated: Vec<Rc<str>>,
}

impl RowExplanation {
    fn into_trace(self, index: usize) -> DecisionTableRowTrace {
        DecisionTableRowTrace {
            index,
            reference_map: self.reference_map,
            rule: self.rule,
            unevaluated: self.unevaluated,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToVariable)]
pub struct DecisionTableRowTrace {
    index: usize,
    reference_map: HashMap<Rc<str>, Variable>,
    rule: HashMap<Rc<str>, Rc<str>>,
    /// Norn (#1711): input selectors the row skipped (blank or absent cells); never evaluated
    /// for the trace.
    unevaluated: Vec<Rc<str>>,
}

#[derive(Debug, Clone, Serialize, ToVariable)]
#[serde(untagged)]
pub enum DecisionTableNodeTrace {
    FirstHit(DecisionTableRowTrace),
    Collect(Vec<DecisionTableRowTrace>),
}

impl Default for DecisionTableNodeTrace {
    fn default() -> Self {
        DecisionTableNodeTrace::Collect(Default::default())
    }
}
