//! Closed `decide` input and output schema contract checks for `norn-decisions`.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Value;
use norn_ir::rule::{DECIDE_OPTIONAL_INPUT_MEMBERS, DECIDE_PRINCIPAL_MEMBER};
use norn_ir::{
    DECIDE_INPUT_MEMBERS, DECIDE_OPTIONAL_OUTPUT_MEMBERS, DECIDE_OUTPUT_MEMBERS, JsonValue,
    RuleRoot, RuntimeObject, RuntimeValue,
};

use crate::convert::{value_from_ir, value_from_runtime};
use crate::error::AdmissionError;
use crate::schema::Schema;

pub(crate) fn invalid_schema(
    rule_id: &str,
    path: &str,
    reason: impl Into<String>,
) -> AdmissionError {
    AdmissionError::InvalidSchema {
        rule_id: rule_id.to_owned(),
        path: path.to_owned(),
        reason: reason.into(),
    }
}

pub(crate) fn parse_schema(rule: &RuleRoot, member: &str) -> Result<Schema, AdmissionError> {
    parse_schema_at(rule, member, member)
}

pub(crate) fn parse_schema_at(
    rule: &RuleRoot,
    member: &str,
    path: &str,
) -> Result<Schema, AdmissionError> {
    let document = norn_ir::JsonValue::Object(match member {
        "inputSchema" => rule.input_schema.clone(),
        _ => rule.output_schema.clone(),
    });
    Schema::parse(&document, path)
        .map_err(|violation| invalid_schema(&rule.rule_id, &violation.path, violation.reason))
}

/// A sub-decision's schema: the closed dialect, rooted at a record.
pub(crate) fn closed_record(
    rule: &RuleRoot,
    member: &str,
    path: &str,
) -> Result<Schema, AdmissionError> {
    let schema = parse_schema_at(rule, member, path)?;
    match schema.properties() {
        Some(_) => Ok(schema),
        None => Err(invalid_schema(
            &rule.rule_id,
            path,
            "must be a closed record",
        )),
    }
}

/// The record members of a decide schema: all `members` plus any declared
/// `optional` members, all required.
fn contract_members(
    rule: &RuleRoot,
    schema: &Schema,
    member: &str,
    members: &[&str],
    optional: &[&str],
) -> Result<BTreeMap<String, Schema>, AdmissionError> {
    let properties = schema
        .properties()
        .ok_or_else(|| invalid_schema(&rule.rule_id, member, "must be a closed record"))?;
    let declared: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
    let required: BTreeSet<&str> = schema
        .required()
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !members.iter().all(|m| declared.contains(m))
        || !declared
            .iter()
            .all(|d| members.contains(d) || optional.contains(d))
        || required != declared
    {
        return Err(invalid_schema(
            &rule.rule_id,
            member,
            format!(
                "the decide contract requires {members:?} (optional {optional:?}), all required"
            ),
        ));
    }
    Ok(properties.clone())
}

/// The `slice` and `event` records of a decide input, and its optional
/// `principal` record (PRINCIPAL-INPUT-1).
pub(crate) fn decide_input(
    rule: &RuleRoot,
) -> Result<(Schema, Schema, Option<Schema>), AdmissionError> {
    let schema = parse_schema(rule, "inputSchema")?;
    let mut members = contract_members(
        rule,
        &schema,
        "inputSchema",
        &DECIDE_INPUT_MEMBERS,
        &DECIDE_OPTIONAL_INPUT_MEMBERS,
    )?;
    let mut record = |name: &str| {
        members
            .remove(name)
            .filter(|member| member.properties().is_some())
            .ok_or_else(|| {
                invalid_schema(
                    &rule.rule_id,
                    &format!("inputSchema.properties.{name}"),
                    "must be a closed record",
                )
            })
    };
    let slice = record("slice")?;
    let event = record("event")?;
    Ok((slice, event, members.remove(DECIDE_PRINCIPAL_MEMBER)))
}

pub(crate) fn decide_output(rule: &RuleRoot) -> Result<Schema, AdmissionError> {
    let schema = parse_schema(rule, "outputSchema")?;
    let members = contract_members(
        rule,
        &schema,
        "outputSchema",
        &DECIDE_OUTPUT_MEMBERS,
        &DECIDE_OPTIONAL_OUTPUT_MEMBERS,
    )?;
    for (name, member) in &members {
        match member {
            Schema::Array(items) if items.properties().is_some() => {}
            Schema::Object { .. }
            | Schema::Array(_)
            | Schema::String(_)
            | Schema::Integer
            | Schema::Number
            | Schema::Decimal(_)
            | Schema::Boolean
            | Schema::Null
            | Schema::Json
            | Schema::Nullable(_) => {
                return Err(invalid_schema(
                    &rule.rule_id,
                    &format!("outputSchema.properties.{name}"),
                    "must be a list of closed records",
                ));
            }
        }
    }
    Ok(schema)
}

pub(crate) fn defaults(
    rule: &RuleRoot,
    output: &Schema,
    exact: Option<&RuntimeObject>,
) -> Result<BTreeMap<String, Value>, AdmissionError> {
    if let Some(exact) = exact {
        let rule_defaults = rule.defaults.as_ref();
        if let Some(extra) = exact
            .keys()
            .find(|key| rule_defaults.is_none_or(|map| !map.contains_key(key.as_str())))
        {
            return Err(AdmissionError::InvalidDefault {
                rule_id: rule.rule_id.clone(),
                member: extra.clone(),
                reason: "the exact default does not match the decoded artifact".to_owned(),
            });
        }
    }
    let mut defaults = BTreeMap::new();
    for (member, value) in rule.defaults.iter().flatten() {
        let invalid = |reason: String| AdmissionError::InvalidDefault {
            rule_id: rule.rule_id.clone(),
            member: member.clone(),
            reason,
        };
        let value = if let Some(exact) = exact {
            let source = exact.get(member).ok_or_else(|| {
                invalid("the exact default does not match the decoded artifact".to_owned())
            })?;
            if !same_up_to_f64(value, source) {
                return Err(invalid(
                    "the exact default does not match the decoded artifact".to_owned(),
                ));
            }
            value_from_runtime(source).map_err(|error| invalid(error.to_string()))?
        } else {
            value_from_ir(value).map_err(|error| invalid(error.to_string()))?
        };
        let schema = output
            .properties()
            .and_then(|properties| properties.get(member))
            .ok_or_else(|| invalid("is not a declared output member".to_owned()))?;
        schema
            .check(&value, member)
            .map_err(|violation| invalid(format!("{}: {}", violation.path, violation.reason)))?;
        defaults.insert(member.clone(), value);
    }
    Ok(defaults)
}

const fn f64_bits(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits == (-0.0_f64).to_bits() {
        0
    } else {
        bits
    }
}

/// Whether the exact `source` decodes to the IR value `decoded`: the same
/// shape, and every number's `f64` (nearest, as JSON decoding gives) equal.
pub(crate) fn same_up_to_f64(decoded: &JsonValue, source: &RuntimeValue) -> bool {
    let mut stack = vec![(decoded, source)];
    while let Some((decoded, source)) = stack.pop() {
        match (decoded, source) {
            (JsonValue::Null, RuntimeValue::Null) => {}
            (JsonValue::Bool(left), RuntimeValue::Bool(right)) if left == right => {}
            // IR decoding reads numbers with `serde_json`'s historical parser
            // (`norn_ir::JsonNumber`); a correctly rounded parse is also
            // accepted, so no spelling of this artifact's own number is refused.
            (JsonValue::Number(left), RuntimeValue::Number(right)) => {
                let left_bits = f64_bits(*left);
                let text = right.canonical();
                let historical = norn_ir::JsonNumber::parse_literal(&text)
                    .is_ok_and(|number| f64_bits(number.as_f64()) == left_bits);
                let rounded = text
                    .parse::<f64>()
                    .is_ok_and(|parsed| f64_bits(parsed) == left_bits);
                if !(historical || rounded) {
                    return false;
                }
            }
            (JsonValue::String(left), RuntimeValue::String(right)) if left == right => {}
            (JsonValue::Array(left), RuntimeValue::Array(right)) if left.len() == right.len() => {
                stack.extend(left.iter().zip(right));
            }
            (JsonValue::Object(left), RuntimeValue::Object(right)) if left.len() == right.len() => {
                for ((left_key, left_val), (right_key, right_val)) in left.iter().zip(right) {
                    if left_key != right_key {
                        return false;
                    }
                    stack.push((left_val, right_val));
                }
            }
            _ => return false,
        }
    }
    true
}
