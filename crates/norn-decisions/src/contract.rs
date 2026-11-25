//! Closed `decide` input and output schema contract checks for `norn-decisions`.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_ir::rule::{DECIDE_OPTIONAL_INPUT_MEMBERS, DECIDE_PRINCIPAL_MEMBER};
use norn_ir::{
    DECIDE_INPUT_MEMBERS, DECIDE_OPTIONAL_OUTPUT_MEMBERS, DECIDE_OUTPUT_MEMBERS, RuleRoot,
};

use crate::error::AdmissionError;

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
