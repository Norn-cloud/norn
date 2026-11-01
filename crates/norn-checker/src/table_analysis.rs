//! Exhaustive decision-table analysis over finite closed input schemas.
//!
//! Reuses the admitted schema parser and ZEN evaluator. This is an analysis of
//! an already validated table, not an alternative JDM validator or evaluator.
//! Unbounded domains, unsupported execution modes and exhausted budgets return
//! an explicit unknown result. Neither admitted hit policy forbids overlap:
//! `first` prioritizes it; `collect` selects every matching row.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_expr::{EvaluationBudget, Expression, UnaryTest, Value};
use norn_ir::jdm::{JdmDecisionTableContent, JdmExecutionMode, JdmHitPolicy};
use serde_json::Value as Json;

/// Maximum finite input combinations explored by one table analysis.
pub const MAX_CONTEXTS: usize = 256;
/// Maximum row/cell evaluations, including column selectors.
pub const MAX_EVALUATIONS: usize = 8_192;
/// Maximum aggregate encoded bytes in an enumerated domain.
pub const MAX_DOMAIN_BYTES: usize = 65_536;
const EXPRESSION_FUEL: u64 = 1_000;

/// Why an exhaustive verdict could not be established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unknown {
    /// The schema has an unbounded leaf or array.
    UnboundedDomain,
    /// Enumeration or expression-work limits would be exceeded.
    Limit,
    /// Table loop execution requires a different input model.
    LoopExecution,
    /// Array-collect output changes selection even under the first policy.
    ArrayOutput,
    /// The engine shares its mutable unary-test subject between cells.
    SharedSubject,
    /// Opaque temporal values cannot cross the Norn value projection losslessly.
    TemporalIntermediate,
    /// Re-entering the evaluator would normalize an intermediate string or key.
    NonCanonicalIntermediate,
    /// An expression did not compile, evaluate, or produce a boolean.
    Expression(String),
}

/// A concrete input matching more than one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    /// Complete schema-valid input, including optional-member absence.
    pub input: Json,
    /// Matching row ids in declaration order.
    pub matching_rows: Vec<String>,
    /// Rows actually selected under the table's hit policy.
    pub selected_rows: Vec<String>,
}

/// Exhaustive results for one admitted table, relative to its input schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    /// Number of schema-valid inputs checked.
    pub inputs_checked: usize,
    /// First uncovered input, in deterministic enumeration order.
    pub uncovered: Option<Json>,
    /// Number of uncovered input combinations.
    pub uncovered_count: usize,
    /// Rows that can never be selected (including first-policy shadowing).
    pub unreachable_rows: Vec<String>,
    /// Overlap witnesses; legal under both admitted hit policies.
    pub overlaps: Vec<Overlap>,
}

#[derive(Default)]
struct Domain {
    values: Vec<Json>,
    bytes: usize,
}

impl Domain {
    fn push(&mut self, value: Json) -> Result<(), Unknown> {
        if self.values.len() >= MAX_CONTEXTS {
            return Err(Unknown::Limit);
        }
        let size = norn_canonical::canonical_json_of(&value)
            .map_err(|_serialization_error| Unknown::Limit)?
            .len();
        self.bytes = self.bytes.checked_add(size).ok_or(Unknown::Limit)?;
        if self.bytes > MAX_DOMAIN_BYTES {
            return Err(Unknown::Limit);
        }
        self.values.push(value);
        Ok(())
    }
}

fn domain(schema: &Schema) -> Result<Domain, Unknown> {
    let mut out = Domain::default();
    match schema {
        Schema::Boolean => {
            out.push(Json::Bool(false))?;
            out.push(Json::Bool(true))?;
        }
        Schema::Null => out.push(Json::Null)?,
        Schema::String(Some(values)) => {
            for value in values {
                out.push(Json::String(value.clone()))?;
            }
        }
        Schema::Nullable(inner) => {
            out = domain(inner)?;
            out.push(Json::Null)?;
        }
        Schema::Object {
            properties,
            required,
        } => {
            out.push(Json::Object(serde_json::Map::new()))?;
            for (key, schema) in properties {
                let field = domain(schema)?;
                let mut next = Domain::default();
                for object in &out.values {
                    if !required.contains(key) {
                        next.push(object.clone())?;
                    }
                    for value in &field.values {
                        let mut object = object.as_object().cloned().ok_or(Unknown::Limit)?;
                        object.insert(key.clone(), value.clone());
                        next.push(Json::Object(object))?;
                    }
                }
                out = next;
            }
        }
        Schema::Array(_)
        | Schema::String(None)
        | Schema::Integer
        | Schema::Number
        | Schema::Json => {
            return Err(Unknown::UnboundedDomain);
        }
    }
    Ok(out)
}

fn noncanonical(value: &Value) -> bool {
    match value {
        Value::String(text) => norn_ir::canonical::nfc(text) != *text,
        Value::Array(items) => items.iter().any(noncanonical),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| norn_ir::canonical::nfc(key) != *key || noncanonical(value)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn expression(source: &str, context: &Value) -> Result<Value, Unknown> {
    let value = Expression::compile(source)
        .and_then(|compiled| compiled.evaluate(context, EvaluationBudget::new(EXPRESSION_FUEL)))
        .map(|result| result.value().clone())
        .map_err(|error| Unknown::Expression(error.to_string()))?;
    if noncanonical(&value) {
        return Err(Unknown::NonCanonicalIntermediate);
    }
    Ok(value)
}

fn matches(
    table: &JdmDecisionTableContent,
    cells: &BTreeMap<String, norn_ir::ZenExpression>,
    context: &Value,
) -> Result<bool, Unknown> {
    for column in &table.inputs {
        let source = cells
            .get(&column.id)
            .map_or("", norn_ir::ZenExpression::as_str)
            .trim();
        if source.is_empty() {
            continue;
        }
        let matched = if let Some(field) = column
            .field
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let subject = expression(field, context)?;
            UnaryTest::compile(source)
                .and_then(|test| {
                    test.evaluate(context, &subject, EvaluationBudget::new(EXPRESSION_FUEL))
                })
                .map_err(|error| Unknown::Expression(error.to_string()))?
        } else {
            match expression(source, context)? {
                Value::Bool(value) => value,
                Value::Null
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_)
                | Value::Object(_) => {
                    return Err(Unknown::Expression("input cell is not boolean".to_owned()));
                }
            }
        };
        if !matched {
            return Ok(false);
        }
    }
    Ok(true)
}

fn supported_mode(table: &JdmDecisionTableContent) -> Result<(), Unknown> {
    if table.execution_mode == Some(JdmExecutionMode::Loop) {
        return Err(Unknown::LoopExecution);
    }
    if table
        .outputs
        .iter()
        .any(|output| output.field.trim().ends_with("[]"))
    {
        return Err(Unknown::ArrayOutput);
    }
    // Conservative lexical refusal includes dollars inside string literals.
    // Never infer an independent-cell result for the engine's shared isolate.
    let shared_subject = table
        .input_field
        .as_deref()
        .is_some_and(|s| s.contains('$'))
        || table
            .inputs
            .iter()
            .any(|column| column.field.as_deref().is_some_and(|s| s.contains('$')))
        || table.rules.iter().any(|row| {
            table.inputs.iter().any(|column| {
                row.cells
                    .get(&column.id)
                    .is_some_and(|cell| cell.as_str().contains('$'))
            })
        });
    if shared_subject {
        return Err(Unknown::SharedSubject);
    }
    let temporal = |source: &str| source.contains("date") || source.contains("timestamp");
    if table.input_field.as_deref().is_some_and(temporal)
        || table
            .inputs
            .iter()
            .any(|column| column.field.as_deref().is_some_and(temporal))
    {
        return Err(Unknown::TemporalIntermediate);
    }
    Ok(())
}

/// Analyze an admitted table exhaustively over `input_schema`.
///
/// `input_schema` describes the context arriving at this table, not necessarily
/// the graph's root input. Outputs are not evaluated: overlap is resolved by
/// the hit policy, so differing output values are not a policy conflict.
///
/// # Errors
/// Returns [`Unknown`] rather than a partial or sampled proof when any domain,
/// mode, evaluation or resource bound prevents exhaustive analysis.
pub fn analyze_table(
    table: &JdmDecisionTableContent,
    input_schema: &Schema,
) -> Result<Analysis, Unknown> {
    supported_mode(table)?;
    let inputs = domain(input_schema)?;
    let work = inputs
        .values
        .len()
        .checked_mul(table.rules.len().max(1))
        .and_then(|n| n.checked_mul(table.inputs.len().max(1)))
        .and_then(|n| n.checked_mul(3))
        .ok_or(Unknown::Limit)?;
    if work > MAX_EVALUATIONS {
        return Err(Unknown::Limit);
    }
    // Bound result construction as well as evaluator work: row identities can
    // otherwise multiply across all overlap witnesses before the caller can meter them.
    let row_bytes = table
        .rules
        .iter()
        .fold(0_usize, |total, row| total.saturating_add(row.id.len()));
    let output_bound = inputs.bytes.saturating_mul(2).saturating_add(
        inputs
            .values
            .len()
            .saturating_mul(row_bytes)
            .saturating_mul(2),
    );
    if output_bound > MAX_DOMAIN_BYTES {
        return Err(Unknown::Limit);
    }
    let mut selected = BTreeSet::new();
    let mut result = Analysis {
        inputs_checked: inputs.values.len(),
        uncovered: None,
        uncovered_count: 0,
        unreachable_rows: Vec::new(),
        overlaps: Vec::new(),
    };
    for input in inputs.values {
        let runtime: norn_ir::RuntimeValue = serde_json::from_value(input.clone())
            .map_err(|error| Unknown::Expression(error.to_string()))?;
        let mut context =
            Value::try_from(&runtime).map_err(|error| Unknown::Expression(error.to_string()))?;
        if let Some(field) = table
            .input_field
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            context = expression(field, &context)?;
        }
        let mut matching = Vec::new();
        for (index, row) in table.rules.iter().enumerate() {
            if matches(table, &row.cells, &context)? {
                matching.push((index, row.id.clone()));
            }
        }
        let count = match table.hit_policy {
            JdmHitPolicy::First => usize::from(!matching.is_empty()),
            JdmHitPolicy::Collect => matching.len(),
        };
        for (index, _) in matching.iter().take(count) {
            selected.insert(*index);
        }
        if matching.is_empty() {
            result.uncovered_count = result.uncovered_count.saturating_add(1);
            if result.uncovered.is_none() {
                result.uncovered = Some(input);
            }
        } else if matching.len() > 1 {
            result.overlaps.push(Overlap {
                input,
                selected_rows: matching
                    .iter()
                    .take(count)
                    .map(|(_, id)| id.clone())
                    .collect(),
                matching_rows: matching.into_iter().map(|(_, id)| id).collect(),
            });
        }
    }
    result.unreachable_rows = table
        .rules
        .iter()
        .enumerate()
        .filter(|(index, _)| !selected.contains(index))
        .map(|(_, row)| row.id.clone())
        .collect();
    Ok(result)
}
