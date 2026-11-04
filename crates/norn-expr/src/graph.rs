//! VAL-EXACT-1 / VAL-RATE: bind the bounded expression graph to its declared schemas.
use crate::{EnvOptions, Expression, Type, ZenTypeEnv, schema::Schema};
use norn_ir::{
    JsonValue, RuleRoot,
    jdm::{JdmExecutionMode, JdmNode},
};
use std::collections::BTreeMap;

/// One immutable assignment passed to the existing shared expression VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundAssignment {
    /// Node identity from the admitted graph.
    pub node: String,
    /// Assignment key from the admitted graph.
    pub key: String,
    /// Original source, checked when selecting this binding.
    pub source: String,
    /// Source with declared Decimal result sites lowered to shared rescale.
    pub runtime_source: String,
    /// Select the single exact arithmetic profile for this assignment.
    pub exact: bool,
    /// Informational unit inference evidence.
    pub notes: Vec<crate::Diagnostic>,
    /// Admission-derived assignment fuel evidence; graph overhead remains residual.
    pub fuel: crate::check::FuelCertificate,
}

/// Derive typed assignments only when a schema declares exact numeric sites.
/// The bounded profile is one expression node between input and output; other
/// graph forms retain their existing profile when no exact schema is declared.
///
/// # Errors
/// Refuses unsupported exact graphs, erased numeric types and dimension mismatches.
pub fn bind(root: &RuleRoot) -> Result<Vec<BoundAssignment>, String> {
    if !crate::schema::contains_exact_annotation(&JsonValue::Object(root.input_schema.clone()))
        && !crate::schema::contains_exact_annotation(&JsonValue::Object(root.output_schema.clone()))
    {
        return Ok(Vec::new());
    }
    let input = Schema::parse(&JsonValue::Object(root.input_schema.clone()), "input")
        .map_err(|e| e.reason)?;
    let output = Schema::parse(&JsonValue::Object(root.output_schema.clone()), "output")
        .map_err(|e| e.reason)?;
    let graph = root.graph.as_ref().ok_or("exact schemas require a graph")?;
    if graph.nodes.len() != 3 || graph.edges.len() != 2 {
        return Err(
            "exact schema profile requires one expression node between input and output".into(),
        );
    }
    let mut entry = None;
    let mut exit = None;
    let mut assignment = None;
    for node in &graph.nodes {
        match node {
            JdmNode::InputNode { id, .. } => entry = Some(id.as_str()),
            JdmNode::OutputNode { id, .. } => exit = Some(id.as_str()),
            JdmNode::ExpressionNode { id, content, .. }
                if content.input_field.is_none()
                    && content.output_path.is_none()
                    && !content.pass_through.unwrap_or(false)
                    && !matches!(content.execution_mode, Some(JdmExecutionMode::Loop)) =>
            {
                assignment = Some((id, content));
            }
            JdmNode::ExpressionNode { .. }
            | JdmNode::DecisionTableNode { .. }
            | JdmNode::SwitchNode { .. }
            | JdmNode::DecisionNode { .. }
            | JdmNode::FunctionNode { .. }
            | JdmNode::CustomNode { .. } => {
                return Err("unsupported node or transform in exact schema graph".into());
            }
        }
    }
    let (id, content) = assignment.ok_or("missing exact expression node")?;
    let (entry, exit) = (entry.ok_or("missing input")?, exit.ok_or("missing output")?);
    if !graph
        .edges
        .iter()
        .any(|e| e.source_id == entry && e.target_id == *id && e.source_handle.is_none())
        || !graph
            .edges
            .iter()
            .any(|e| e.source_id == *id && e.target_id == exit && e.source_handle.is_none())
    {
        return Err("exact graph edges do not connect input/expression/output".into());
    }
    let mut records = BTreeMap::new();
    let input_type = input.expression_type("exact.input", &mut records);
    let output_type = output.expression_type("exact.output", &mut records);
    let Type::Record(input_name) = input_type else {
        return Err("exact input must be a record".into());
    };
    let fields = records
        .get(&input_name)
        .ok_or("missing input record")?
        .clone();
    let mut env =
        ZenTypeEnv::input_only_with_records(records, fields.clone(), EnvOptions::default())
            .map_err(|e| e.to_string())?;
    for (name, ty) in fields {
        env = env.bind_operand(&name, ty).map_err(|e| e.to_string())?;
    }
    let Type::Record(output_name) = output_type else {
        return Err("exact output must be a record".into());
    };
    crate::check::validate_exact_environment(&env, "exact.")?;
    let outputs = env.record(&output_name).ok_or("missing output record")?;
    let mut result = Vec::new();
    for expression in &content.expressions {
        let expected = &outputs
            .fields
            .get(&expression.key)
            .ok_or("exact assignment must name a declared output field")?
            .ty;
        let compiled = Expression::compile(expression.value.as_str()).map_err(|e| e.to_string())?;
        let (checked, fuel) = crate::check::certify_expression(
            &compiled, &env, expected, &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        ).map_err(|reason| format!("node {id} assignment {}: {reason}", expression.key))?;
        result.push(BoundAssignment {
            fuel,
            node: id.clone(),
            key: expression.key.clone(),
            source: expression.value.as_str().to_owned(),
            runtime_source: checked.runtime_source().to_owned(),
            notes: checked.notes().to_vec(),
            exact: checked.uses_exact_arithmetic(),
        });
    }
    Ok(result)
}
