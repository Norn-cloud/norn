//! The tool calls a lowered flow can make: every `perform` record its decide
//! rule's `commands` output can produce, read from the parsed ZEN AST.
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

fn table(node: &Value, n: usize, found: &mut Vec<PerformRef>) -> Result<(), Unverifiable> {
    let content = node.get("content").unwrap_or(&Value::Null);
    let outputs = content
        .get("outputs")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let commands: Vec<&str> = outputs
        .iter()
        .filter(|output| output.get("field").and_then(Value::as_str) == Some("commands"))
        .filter_map(|output| output.get("id").and_then(Value::as_str))
        .collect();
    let [column] = commands.as_slice() else {
        return Err(unverifiable(
            format!("/graph/nodes/{n}/content/outputs"),
            "a flow rule's decision table has exactly one commands output",
        ));
    };
    let rules = content
        .get("rules")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for (r, rule) in rules.iter().enumerate() {
        let pointer = format!(
            "/graph/nodes/{n}/content/rules/{r}/{}",
            column.replace('~', "~0").replace('/', "~1")
        );
        let source = rule
            .get(*column)
            .and_then(Value::as_str)
            .unwrap_or_default();
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
                "outputs": [{ "id": "o_facts", "field": "facts" }, { "id": "o_commands", "field": "commands" }],
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
        assert!(
            perform_refs(&table_payload("[{kind: 'timer', due: 1}]"))
                .unwrap()
                .is_empty()
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
}
