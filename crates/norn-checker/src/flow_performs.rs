//! The tool calls a lowered flow can make: every `perform` record in its
//! decide rule's expressions, read from the parsed ZEN AST.
//!
//! The expressions are the only source of truth: the driver evaluates the
//! same records at dispatch. A record whose `function` or `functionVersion`
//! is not a string literal cannot be resolved before it runs, so callers
//! refuse it.

use norn_expr::{FieldValue, ObjectLiteral, object_literals};
use serde_json::Value;

/// One `perform` record and where its expression is in the rule payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformRef {
    /// RFC 6901 pointer to the expression, relative to the rule `payload`.
    pub pointer: String,
    /// `(function, functionVersion)` when both are string literals.
    pub name: Option<(String, String)>,
}

/// Every `perform` record in the decision-table outputs and expression nodes
/// of `payload`'s graph, in graph order.
///
/// # Errors
/// The pointer and message of an expression that does not parse.
pub fn perform_refs(payload: &Value) -> Result<Vec<PerformRef>, (String, String)> {
    let mut found = Vec::new();
    let nodes = payload
        .pointer("/graph/nodes")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for (n, node) in nodes.iter().enumerate() {
        let content = node.get("content").unwrap_or(&Value::Null);
        match node.get("type").and_then(Value::as_str) {
            Some("decisionTableNode") => {
                let outputs: Vec<&str> = content
                    .get("outputs")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .filter_map(|output| output.get("id").and_then(Value::as_str))
                    .collect();
                let rules = content
                    .get("rules")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                for (r, rule) in rules.iter().enumerate() {
                    for output in &outputs {
                        if let Some(source) = rule.get(*output).and_then(Value::as_str) {
                            let pointer = format!(
                                "/graph/nodes/{n}/content/rules/{r}/{}",
                                output.replace('~', "~0").replace('/', "~1")
                            );
                            scan(source, &pointer, &mut found)?;
                        }
                    }
                }
            }
            Some("expressionNode") => {
                let expressions = content
                    .get("expressions")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                for (e, expression) in expressions.iter().enumerate() {
                    if let Some(source) = expression.get("value").and_then(Value::as_str) {
                        let pointer = format!("/graph/nodes/{n}/content/expressions/{e}/value");
                        scan(source, &pointer, &mut found)?;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    Ok(found)
}

fn scan(source: &str, pointer: &str, found: &mut Vec<PerformRef>) -> Result<(), (String, String)> {
    if source.trim().is_empty() {
        return Ok(());
    }
    let objects =
        object_literals(source).map_err(|error| (pointer.to_owned(), error.to_string()))?;
    for object in objects.iter().filter(|object| is_perform(object)) {
        found.push(PerformRef {
            pointer: pointer.to_owned(),
            name: literal_name(object),
        });
    }
    Ok(())
}

/// A record is a perform when its `kind` is the literal `perform`, or when
/// its `kind` is not a literal and it names a function: an unresolvable kind
/// is treated as a perform so it cannot slip past the check.
fn is_perform(object: &ObjectLiteral) -> bool {
    match object.fields.get("kind") {
        Some(FieldValue::String(kind)) => kind == "perform",
        Some(FieldValue::Other) => {
            object.fields.contains_key("function") || object.fields.contains_key("functionVersion")
        }
        None => object.computed_keys && object.fields.contains_key("function"),
    }
}

fn literal_name(object: &ObjectLiteral) -> Option<(String, String)> {
    if object.computed_keys {
        return None;
    }
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

    fn table(cell: &str) -> Value {
        json!({ "graph": { "nodes": [
            { "id": "in", "type": "inputNode" },
            { "id": "route", "type": "decisionTableNode", "content": {
                "outputs": [{ "id": "o_commands", "field": "commands" }],
                "rules": [{ "_id": "r0", "o_commands": cell }, { "_id": "r1", "o_commands": "[]" }] } }
        ] } })
    }

    #[test]
    fn a_perform_nested_in_a_conditional_output_is_found() {
        let refs = perform_refs(&table(
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
    fn computed_names_kinds_and_keys_cannot_be_resolved() {
        for cell in [
            "[{kind: 'perform', function: input.fn, functionVersion: '1'}]",
            "[{kind: 'perform', function: 'a', functionVersion: string(1)}]",
            "[{kind: input.kind, function: 'a', functionVersion: '1', extra: input.x}]",
        ] {
            let refs = perform_refs(&table(cell)).unwrap();
            assert_eq!(refs.len(), 1, "{cell}");
            if cell.contains("input.kind") {
                // A computed kind is treated as a perform; its names are literal.
                assert_eq!(refs[0].name, Some(("a".to_owned(), "1".to_owned())));
            } else {
                assert_eq!(refs[0].name, None, "{cell}");
            }
        }
        // A record that is not a perform is not reported.
        assert!(
            perform_refs(&table("[{kind: 'timer', due: 1}]"))
                .unwrap()
                .is_empty()
        );
        // An expression that does not parse is reported with its place.
        let (pointer, _) = perform_refs(&table("[{kind: 'perform'")).unwrap_err();
        assert_eq!(pointer, "/graph/nodes/1/content/rules/0/o_commands");
    }
}
