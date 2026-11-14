//! The tool calls a lowered flow can make: every `perform` record its decide
//! rule's `commands` output can produce, read from the parsed ZEN AST.
//!
//! The table is exactly `lowerFlow`'s: hit policy `first`, no transform
//! attributes, and the output columns `facts`, `commands` and `timers`, each
//! spelled as the path the engine writes it to, so no other column can write
//! `commands` at run time.
//!
//! The expressions are the only source of truth: the driver dispatches the
//! records they evaluate to. So the shape is allowlisted, not detected: a
//! flow rule's graph is `lowerFlow`'s (input, one decision table, output),
//! and every `commands` cell is an array of plain object literals with a
//! literal `kind` (or a conditional choosing between such arrays). Anything
//! that could construct a command record another way (a variable, `merge`,
//! a member read, a computed key) is unverifiable and refused, as is a
//! `perform` whose `function` or `functionVersion` is not a literal.

use norn_expr::{FieldValue, ObjectLiteral, command_records};
use serde_json::Value;

/// One `perform` record and where its expression is in the rule payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformRef {
    /// RFC 6901 pointer to the expression, relative to the rule `payload`.
    pub pointer: String,
    /// `(function, functionVersion)` when both are string literals.
    pub name: Option<(String, String)>,
    /// The record's literal `step` id, when it has one.
    pub step: Option<String>,
}

/// Why a rule's commands cannot be verified before they run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unverifiable {
    /// RFC 6901 pointer into the rule payload.
    pub pointer: String,
    /// What is not fixed.
    pub message: String,
}

fn unverifiable(pointer: impl Into<String>, message: impl Into<String>) -> Unverifiable {
    Unverifiable {
        pointer: pointer.into(),
        message: message.into(),
    }
}

/// Every `perform` record the rule `payload`'s `commands` output can
/// produce, in graph order.
///
/// # Errors
/// The first place where the graph is not `lowerFlow`'s shape or a command
/// record is not fixed by the expression.
pub fn perform_refs(payload: &Value) -> Result<Vec<PerformRef>, Unverifiable> {
    let nodes = payload
        .pointer("/graph/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| unverifiable("/graph/nodes", "a flow rule has a graph of nodes"))?;
    let mut tables = 0_usize;
    let mut found = Vec::new();
    for (n, node) in nodes.iter().enumerate() {
        match node.get("type").and_then(Value::as_str) {
            Some("inputNode" | "outputNode") => {}
            Some("decisionTableNode") => {
                tables = tables.saturating_add(1);
                table(node, n, &mut found)?;
            }
            Some(_) | None => {
                return Err(unverifiable(
                    format!("/graph/nodes/{n}/type"),
                    "a flow rule's graph has only input, decision table and output nodes",
                ));
            }
        }
    }
    if tables != 1 {
        return Err(unverifiable(
            "/graph/nodes",
            "a flow rule has exactly one decision table",
        ));
    }
    Ok(found)
}

/// The output columns `lowerFlow` writes, by field, each exactly once.
const OUTPUT_FIELDS: [&str; 3] = ["facts", "commands", "timers"];

/// The members `lowerFlow`'s decision table has: no transform attributes
/// (`inputField`, `outputPath`, `executionMode`, `passThrough`) that could
/// move or reshape what the table writes.
const TABLE_MEMBERS: [&str; 4] = ["hitPolicy", "inputs", "outputs", "rules"];

/// The path the engine writes an output column to, normalised exactly as
/// `zen`'s `DecisionTableOutputField::write_path` does: trimmed, with a
/// trailing `[]` (append) and the whitespace before it removed.
fn write_path(field: &str) -> &str {
    let field = field.trim();
    field.strip_suffix("[]").map_or(field, str::trim_end)
}

/// `lowerFlow`'s table shape, and the id of its one `commands` column.
///
/// Sol S1 (round 2): the engine writes every output column to its normalised
/// path, so a second column whose field normalises to `commands` (padded,
/// appended `commands[]`, or nested `commands.x`) would overwrite the checked
/// column at run time. Only the exact shape is admitted: hit policy `first`,
/// no transform attributes, and exactly the columns `facts`, `commands` and
/// `timers`, each spelled exactly as its normalised path, with unique ids.
fn commands_column(content: &Value, n: usize) -> Result<&str, Unverifiable> {
    let at = format!("/graph/nodes/{n}/content");
    let members = content
        .as_object()
        .ok_or_else(|| unverifiable(at.clone(), "a flow rule's decision table has content"))?;
    if let Some(extra) = members
        .keys()
        .find(|key| !TABLE_MEMBERS.contains(&key.as_str()))
    {
        return Err(unverifiable(
            format!("{at}/{}", extra.replace('~', "~0").replace('/', "~1")),
            "a flow rule's decision table has only hitPolicy, inputs, outputs and rules",
        ));
    }
    if members.get("hitPolicy").and_then(Value::as_str) != Some("first") {
        return Err(unverifiable(
            format!("{at}/hitPolicy"),
            "a flow rule's decision table has hit policy first",
        ));
    }
    let outputs = members
        .get("outputs")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut fields = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut commands = None;
    for (o, output) in outputs.iter().enumerate() {
        let field = output
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = output.get("id").and_then(Value::as_str).unwrap_or_default();
        let path = write_path(field);
        let root = path.split('.').next().unwrap_or_default();
        if path != field || !OUTPUT_FIELDS.contains(&path) {
            let message = if OUTPUT_FIELDS.contains(&root) {
                format!("a second writer to {root:?}: the field {field:?} writes there")
            } else {
                format!(
                    "a flow rule's decision table writes only facts, commands and timers, not {field:?}"
                )
            };
            return Err(unverifiable(format!("{at}/outputs/{o}/field"), message));
        }
        if !fields.insert(path) {
            return Err(unverifiable(
                format!("{at}/outputs/{o}/field"),
                format!("a second writer to {path:?}"),
            ));
        }
        if id.is_empty() || !ids.insert(id) {
            return Err(unverifiable(
                format!("{at}/outputs/{o}/id"),
                "each output column has its own non-empty id",
            ));
        }
        if path == "commands" {
            commands = Some(id);
        }
    }
    match commands {
        Some(column) if fields.len() == OUTPUT_FIELDS.len() => Ok(column),
        _ => Err(unverifiable(
            format!("{at}/outputs"),
            "a flow rule's decision table has exactly the facts, commands and timers columns",
        )),
    }
}

fn table(node: &Value, n: usize, found: &mut Vec<PerformRef>) -> Result<(), Unverifiable> {
    let content = node.get("content").unwrap_or(&Value::Null);
    let column = commands_column(content, n)?;
    let rules = content
        .get("rules")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for (r, rule) in rules.iter().enumerate() {
        let pointer = format!(
            "/graph/nodes/{n}/content/rules/{r}/{}",
            column.replace('~', "~0").replace('/', "~1")
        );
        let source = rule.get(column).and_then(Value::as_str).unwrap_or_default();
        if source.trim().is_empty() {
            continue;
        }
        let records = command_records(source)
            .map_err(|error| unverifiable(pointer.clone(), error.to_string()))?
            .ok_or_else(|| {
                unverifiable(
                    pointer.clone(),
                    "commands must be an array of plain object literals with a literal kind",
                )
            })?;
        for record in records.iter().filter(|record| {
            matches!(record.fields.get("kind"), Some(FieldValue::String(kind)) if kind == "perform")
        }) {
            found.push(PerformRef {
                pointer: pointer.clone(),
                name: literal_name(record),
                step: match record.fields.get("step") {
                    Some(FieldValue::String(step)) => Some(step.clone()),
                    Some(FieldValue::Other) | None => None,
                },
            });
        }
    }
    Ok(())
}

fn literal_name(object: &ObjectLiteral) -> Option<(String, String)> {
    match (
        object.fields.get("function"),
        object.fields.get("functionVersion"),
    ) {
        (Some(FieldValue::String(function)), Some(FieldValue::String(version))) => {
            Some((function.clone(), version.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "fixed payloads"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table_payload(cell: &str) -> Value {
        json!({ "graph": { "nodes": [
            { "id": "in", "type": "inputNode" },
            { "id": "route", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": [{ "id": "c_when", "name": "when", "type": "expression" }],
                "outputs": [
                    { "id": "o_facts", "name": "facts", "field": "facts" },
                    { "id": "o_commands", "name": "commands", "field": "commands" },
                    { "id": "o_timers", "name": "timers", "field": "timers" }
                ],
                "rules": [{ "_id": "r0", "o_facts": "[]", "o_commands": cell }, { "_id": "r1", "o_commands": "[]" }] } },
            { "id": "out", "type": "outputNode" }
        ] } })
    }

    #[test]
    fn a_perform_nested_in_a_conditional_output_is_found() {
        let refs = perform_refs(&table_payload(
            "status == 'started' ? [{kind: 'perform', runId: key, step: 's', function: 'norn.decision', functionVersion: '1', input: {}}] : []",
        ))
        .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].pointer, "/graph/nodes/1/content/rules/0/o_commands");
        assert_eq!(
            refs[0].name,
            Some(("norn.decision".to_owned(), "1".to_owned()))
        );
    }

    #[test]
    fn computed_function_names_resolve_to_nothing() {
        for cell in [
            "[{kind: 'perform', function: input.fn, functionVersion: '1'}]",
            "[{kind: 'perform', function: 'a', functionVersion: string(1)}]",
        ] {
            let refs = perform_refs(&table_payload(cell)).unwrap();
            assert_eq!(refs.len(), 1, "{cell}");
            assert_eq!(refs[0].name, None, "{cell}");
        }
        // A record that is not a perform is not reported.
        assert_eq!(
            perform_refs(&table_payload("[{kind: 'timer', due: 1}]")).unwrap(),
            []
        );
    }

    /// Sol S1: anything that could construct a command record without a
    /// fixed literal is refused, not missed.
    #[test]
    fn constructed_commands_are_unverifiable() {
        for cell in [
            "[merge([{kind: 'timer', function: 'uninstalled', functionVersion: '1'}, {['ki' + 'nd']: 'perform'}])]",
            "merge([{kind: 'perform', function: 'a', functionVersion: '1'}], [])",
            "[{kind: input.kind, function: 'a', functionVersion: '1'}]",
            "[event.command]",
            "event.commands",
            "map([1], {kind: 'perform', function: 'a', functionVersion: '1'})",
            "x ? [{kind: 'perform', function: 'a', functionVersion: '1'}] : event.more",
            "[{kind: 'perform'",
            // Fable B1: the VM keeps the first of duplicate keys, a map the last.
            "[{kind: 'perform', function: 'x.y', functionVersion: '1', kind: 'timer'}]",
            "[{kind: 'perform', function: 'payments.charge', functionVersion: '1', function: 'norn.decision'}]",
            // Fable B1: a bracketed identifier key is evaluated, not a literal name.
            "[{kind: 'timer', function: 'x.y', functionVersion: '1', [k]: 'perform'}]",
        ] {
            let refused = perform_refs(&table_payload(cell)).unwrap_err();
            assert_eq!(
                refused.pointer, "/graph/nodes/1/content/rules/0/o_commands",
                "{cell}"
            );
        }
        // The graph is lowerFlow's: another node kind, or no single commands
        // output, is unverifiable.
        let mut extra = table_payload("[]");
        extra["graph"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": "x", "type": "expressionNode" }));
        assert_eq!(
            perform_refs(&extra).unwrap_err().pointer,
            "/graph/nodes/3/type"
        );
        let mut no_commands = table_payload("[]");
        no_commands["graph"]["nodes"][1]["content"]["outputs"] =
            json!([{ "id": "o_facts", "field": "facts" }]);
        assert!(perform_refs(&no_commands).is_err());
    }

    /// Sol S1 (round 2): the engine writes each column to its normalised
    /// path, so any second column that lands on `commands` (padded, appended,
    /// nested) is refused, as is any departure from `lowerFlow`'s table.
    #[test]
    fn only_lower_flows_exact_table_is_admitted() {
        let shadow = "[{kind: 'perform', function: 'uninstalled', functionVersion: '1'}]";
        for field in [
            " commands ",
            "commands[]",
            "commands []",
            "commands.0",
            "commands",
            "facts ",
        ] {
            let mut payload = table_payload("[]");
            let content = &mut payload["graph"]["nodes"][1]["content"];
            content["outputs"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "id": "o_shadow", "name": "shadow", "field": field }));
            content["rules"][0]["o_shadow"] = json!(shadow);
            assert_eq!(
                perform_refs(&payload).unwrap_err().pointer,
                "/graph/nodes/1/content/outputs/3/field",
                "{field:?}"
            );
        }
        // A padded field in place of the real column is refused too.
        let mut padded = table_payload("[]");
        padded["graph"]["nodes"][1]["content"]["outputs"][1]["field"] = json!(" commands");
        assert_eq!(
            perform_refs(&padded).unwrap_err().pointer,
            "/graph/nodes/1/content/outputs/1/field"
        );
        let mut duplicate_id = table_payload("[]");
        duplicate_id["graph"]["nodes"][1]["content"]["outputs"][2]["id"] = json!("o_commands");
        assert_eq!(
            perform_refs(&duplicate_id).unwrap_err().pointer,
            "/graph/nodes/1/content/outputs/2/id"
        );
        let mut collect = table_payload("[]");
        collect["graph"]["nodes"][1]["content"]["hitPolicy"] = json!("collect");
        assert_eq!(
            perform_refs(&collect).unwrap_err().pointer,
            "/graph/nodes/1/content/hitPolicy"
        );
        let mut moved = table_payload("[]");
        moved["graph"]["nodes"][1]["content"]["outputPath"] = json!("commands");
        assert_eq!(
            perform_refs(&moved).unwrap_err().pointer,
            "/graph/nodes/1/content/outputPath"
        );
        let mut no_timers = table_payload("[]");
        no_timers["graph"]["nodes"][1]["content"]["outputs"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert_eq!(
            perform_refs(&no_timers).unwrap_err().pointer,
            "/graph/nodes/1/content/outputs"
        );
        assert!(perform_refs(&table_payload("[]")).is_ok());
    }
}
