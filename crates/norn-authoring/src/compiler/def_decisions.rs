//! Decision (`Norn.decision`) lowering: metadata, table graphs, rule triggers,
//! and JDM graph builders.
//!
//! Ports `decision`, `tableGraph`, `trigger`, and `lowerDecision` from
//! `tools/norn-ts/src/compiler/definitions.ts` and `builder/rule.ts`.

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_dotted_lower_id;
use super::def_decision_cb::CallbackGraphSpec;
use super::def_helpers::{DefinitionCompiler, LabelTarget, LoweredDef, Members, OBJECT_TYPES};
use super::def_literals::{build_label_entries, build_labels_artifact};
use super::expr::{Binding, Expr, Scope, render, type_of};
use super::flow_syntax::{DecisionHooks, SyntaxCompiler};
use super::schema::NullableMode;
use super::types::{
    CompileError, DependencyPin, EnvelopeMeta, EnvelopeOptions, build_envelope_checked,
    pinned_dependencies,
};
use super::value_schema::{ExpansionBudget, ObjectSchema, ValueSchema};

const JDM_CONTENT_TYPE: &str = "application/vnd.gorules.decision";

const DECISION_ALLOWED: &[&str] = &[
    "id",
    "artifactId",
    "title",
    "version",
    "contract",
    "dependencies",
    "triggers",
    "slice",
    "principalInputs",
    "input",
    "output",
    "scopeRequired",
    "graph",
    "hit",
    "label",
    "localizedDescription",
    "needsReview",
];

struct RejectSubDecisionHooks;

impl DecisionHooks for RejectSubDecisionHooks {
    fn on_sub_decision(
        &mut self,
        call: P<Node>,
        _input: Expr,
        _checker: &mut Checker,
        _expansion: &mut ExpansionBudget,
    ) -> Result<Expr, CompileError> {
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "a table row cannot evaluate a sub-decision; use a decision callback",
            call,
        ))
    }

    fn on_escape(
        &mut self,
        node: P<Node>,
        _text: &str,
        _schema: ValueSchema,
    ) -> Result<Expr, CompileError> {
        Err(CompileError::new(
            "NORN-AUTHORING-UNSAFE-ZEN",
            "raw ZEN cannot be embedded in typed expressions",
            node,
        ))
    }

    fn supports_escape(&self) -> bool {
        false
    }
}

struct TableGraphSpec<'a> {
    rows_array: P<Node>,
    hit: &'a str,
    input: &'a ObjectSchema,
    output: &'a ObjectSchema,
    call: P<Node>,
}

struct DecisionGraphSpec<'a> {
    body: Option<P<Node>>,
    members: &'a Members,
    hit: &'a str,
    artifact_id: &'a str,
    input_schema: &'a ObjectSchema,
    output_schema: &'a ObjectSchema,
    call: P<Node>,
}

struct DecisionPayloadSpec<'a> {
    members: &'a Members,
    id: &'a str,
    contract: &'a str,
    input_schema: &'a ObjectSchema,
    output_schema: &'a ObjectSchema,
    graph: Value,
    call: P<Node>,
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_decision(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let members = self.meta(checker, call, DECISION_ALLOWED)?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "rule id {id:?} must be dotted lower-case segments (commerce.rule.credit_hold)"
                ),
                call,
            ));
        }
        let body = call
            .as_call_expression()
            .arguments()
            .nodes()
            .get(1)
            .copied();
        let hit = self.read_decision_hit(checker, &members, body)?;
        let contract = self.read_decision_contract(checker, &members, call)?;
        let artifact_id = match members.get("artifactId") {
            Some(a) => self.string(checker, a)?,
            None => format!("{id}.rule"),
        };
        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let localized = self.localized(checker, &members)?;
        let input_schema =
            self.read_decision_schema(checker, expansion, &members, call, "input")?;
        let output_schema =
            self.read_decision_schema(checker, expansion, &members, call, "output")?;

        let mut called_deps = Vec::new();
        let graph = self.compile_decision_graph(
            checker,
            expansion,
            &DecisionGraphSpec {
                body,
                members: &members,
                hit: &hit,
                artifact_id: &artifact_id,
                input_schema: &input_schema,
                output_schema: &output_schema,
                call,
            },
            &mut called_deps,
        )?;
        let all_deps = self.collect_decision_deps(checker, &members, called_deps)?;
        let payload = self.build_decision_payload(
            checker,
            expansion,
            DecisionPayloadSpec {
                members: &members,
                id: &id,
                contract: &contract,
                input_schema: &input_schema,
                output_schema: &output_schema,
                graph,
                call,
            },
        )?;
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "rule".to_owned(),
                artifact_id,
                namespace,
                name: title,
                version,
                dependencies: pinned_dependencies(&all_deps, "rule"),
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        let entries = build_label_entries(&LabelTarget::Rule(id), &localized, None);
        let labels = build_labels_artifact(&artifact, entries, options, call)?;
        Ok(LoweredDef {
            artifact,
            labels,
            at: Some(self.at(call)),
        })
    }

    fn collect_decision_deps(
        &self,
        checker: &mut Checker,
        members: &Members,
        called_deps: Vec<DependencyPin>,
    ) -> Result<Vec<DependencyPin>, CompileError> {
        let mut all_deps = Vec::new();
        if let Some(deps_node) = members.get("dependencies") {
            for dep in self.list(checker, deps_node)? {
                all_deps.push(self.decision_dependency(checker, dep)?);
            }
        }
        all_deps.extend(called_deps);
        Ok(all_deps)
    }

    fn read_decision_hit(
        &self,
        checker: &mut Checker,
        members: &Members,
        body: Option<P<Node>>,
    ) -> Result<String, CompileError> {
        let Some(hit_node) = members.get("hit") else {
            return Ok("first".to_owned());
        };
        let s = self.string(checker, hit_node)?;
        if s != "first" && s != "unique" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a decision's hit policy is \"first\" or \"unique\"",
                hit_node,
            ));
        }
        if body.is_none() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "hit applies to a decision callback; an imported editor graph carries its own hit policies",
                hit_node,
            ));
        }
        Ok(s)
    }

    fn read_decision_contract(
        &self,
        checker: &mut Checker,
        members: &Members,
        call: P<Node>,
    ) -> Result<String, CompileError> {
        let contract_node = Self::member(members, "contract", call)?;
        let contract = self.string(checker, contract_node)?;
        if contract != "decide" && contract != "sub_decision" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a decision contract is decide or sub_decision",
                contract_node,
            ));
        }
        Ok(contract)
    }

    fn compile_decision_graph(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: &DecisionGraphSpec<'_>,
        called_deps: &mut Vec<DependencyPin>,
    ) -> Result<Value, CompileError> {
        let Some(body_node) = spec.body else {
            let g_node = Self::member(spec.members, "graph", spec.call)?;
            let g = self.json(checker, expansion, g_node, 0)?;
            if !g.is_object() {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a decision graph is a JDM object { nodes, edges }",
                    g_node,
                ));
            }
            return Ok(g);
        };
        let deref = self.deref(checker, body_node)?;
        if deref.kind() == Kind::ArrayLiteralExpression {
            if spec.members.has("graph") {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "choose table rows or an imported editor graph",
                    body_node,
                ));
            }
            return self.compile_table_graph(
                checker,
                expansion,
                &TableGraphSpec {
                    rows_array: deref,
                    hit: spec.hit,
                    input: spec.input_schema,
                    output: spec.output_schema,
                    call: spec.call,
                },
            );
        }
        if spec.members.has("graph") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "choose a callback or an imported editor graph",
                body_node,
            ));
        }
        self.compile_decision_callback(
            checker,
            expansion,
            &CallbackGraphSpec {
                body: body_node,
                hit: spec.hit,
                artifact_id: spec.artifact_id,
                input_schema: spec.input_schema,
                output_schema: spec.output_schema,
                call: spec.call,
            },
            called_deps,
        )
    }

    fn build_decision_payload(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: DecisionPayloadSpec<'_>,
    ) -> Result<Map<String, Value>, CompileError> {
        let mut payload = Map::new();
        payload.insert("ruleId".to_owned(), Value::String(spec.id.to_owned()));
        payload.insert("ruleVersion".to_owned(), Value::String("2.0".to_owned()));
        payload.insert(
            "contract".to_owned(),
            Value::String(spec.contract.to_owned()),
        );
        if let Some(trig_node) = spec.members.get("triggers") {
            let mut triggers = Vec::new();
            for t in self.list(checker, trig_node)? {
                triggers.push(self.compile_trigger(checker, t)?);
            }
            if !triggers.is_empty() {
                payload.insert("triggers".to_owned(), Value::Array(triggers));
            }
        }
        if let Some(slice_node) = spec.members.get("slice") {
            let mut slice = Vec::new();
            for entry in self.list(checker, slice_node)? {
                slice.push(self.compile_slice_entry(checker, entry)?);
            }
            if !slice.is_empty() {
                payload.insert("slice".to_owned(), Value::Array(slice));
            }
        }
        if let Some(pi_node) = spec.members.get("principalInputs") {
            let mut pis = Vec::new();
            for entry in self.list(checker, pi_node)? {
                pis.push(self.json(checker, expansion, entry, 0)?);
            }
            if !pis.is_empty() {
                payload.insert("principalInputs".to_owned(), Value::Array(pis));
            }
        }
        payload.insert("inputSchema".to_owned(), spec.input_schema.to_json());
        payload.insert("outputSchema".to_owned(), spec.output_schema.to_json());
        payload.insert(
            "scopeRequired".to_owned(),
            Value::Array(
                self.strings(
                    checker,
                    Self::member(spec.members, "scopeRequired", spec.call)?,
                )?
                .into_iter()
                .map(Value::String)
                .collect(),
            ),
        );
        payload.insert("graph".to_owned(), spec.graph);
        Ok(payload)
    }

    fn read_decision_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        call: P<Node>,
        key: &str,
    ) -> Result<ObjectSchema, CompileError> {
        let node = Self::member(members, key, call)?;
        let read = self.schemas.read(checker, node)?;
        let closed = self
            .schemas
            .closed(&read, NullableMode::Any, false, expansion)?;
        match closed {
            ValueSchema::Object(obj) => Ok(*obj),
            ValueSchema::String { .. }
            | ValueSchema::Number { .. }
            | ValueSchema::Integer { .. }
            | ValueSchema::Boolean { .. }
            | ValueSchema::Null { .. }
            | ValueSchema::JsonValue { .. }
            | ValueSchema::Array { .. }
            | ValueSchema::Nullable(_) => Err(CompileError::new(
                "NORN-FLOW-SCHEMA-PROJECTION",
                format!("decision {key} is a Schema.Struct"),
                node,
            )),
        }
    }

    fn compile_table_graph(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: &TableGraphSpec<'_>,
    ) -> Result<Value, CompileError> {
        let mut scope = Scope::new();
        scope.insert(
            "input".to_owned(),
            Binding::Zen {
                zen: String::new(),
                schema: ValueSchema::object(spec.input.clone()),
            },
        );
        let Some(sf) = get_source_file_of_node(spec.call) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "missing source file",
                spec.call,
            ));
        };
        let mut syntax = self.syntax_compiler(sf);
        let elements = spec
            .rows_array
            .as_array_literal_expression()
            .elements
            .nodes();
        if elements.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a table has at least one row",
                spec.rows_array,
            ));
        }
        let outputs: Vec<&str> = spec.output.ordered_keys().collect();
        let mut rules = Vec::with_capacity(elements.len());
        let mut reject_hooks = RejectSubDecisionHooks;

        for (i, &element) in elements.iter().enumerate() {
            let cells = self.compile_table_row(
                &mut syntax,
                checker,
                expansion,
                (&scope, &outputs),
                (i, element),
                &mut reject_hooks,
            )?;
            rules.push(Value::Object(cells));
        }
        self.warnings.extend(syntax.warnings);
        Ok(build_decision_table_graph(
            spec.hit,
            &outputs,
            Vec::new(),
            rules,
        ))
    }

    fn compile_table_row(
        &mut self,
        syntax: &mut SyntaxCompiler<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ctx: (&Scope, &[&str]),
        row: (usize, P<Node>),
        reject_hooks: &mut RejectSubDecisionHooks,
    ) -> Result<Map<String, Value>, CompileError> {
        let (scope, outputs) = ctx;
        let (index, element) = row;
        let m = self.closed_record(checker, element, &["when", "then"], &["when", "then"])?;
        let when_node = Self::member(&m, "when", element)?;
        let then_node = Self::member(&m, "then", element)?;

        let cond =
            self.table_row_expr(syntax, checker, expansion, when_node, "when", reject_hooks)?;
        let cond_ty = type_of(&cond, scope)
            .map_err(|err| CompileError::new(err.code, err.message, element))?;
        if !matches!(cond_ty, ValueSchema::Boolean { .. }) {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "a table row's when is Boolean",
                element,
            ));
        }
        let val =
            self.table_row_expr(syntax, checker, expansion, then_node, "then", reject_hooks)?;
        let Expr::Record(fields) = val else {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "a table row's then returns a record of the declared outputs",
                element,
            ));
        };
        let mut cells = Map::new();
        cells.insert("_id".to_owned(), Value::String(format!("row_{index}")));
        cells.insert(
            "condition".to_owned(),
            Value::String(
                render(&cond, scope)
                    .map_err(|err| CompileError::new(err.code, err.message, element))?,
            ),
        );
        for &field in outputs {
            let Some((_, cell)) = fields.iter().find(|(k, _)| k == field) else {
                return Err(CompileError::new(
                    "NORN-FLOW-RESULT",
                    format!("expected output field {field}; return every declared output"),
                    element,
                ));
            };
            cells.insert(
                format!("out_{field}"),
                Value::String(
                    render(cell, scope)
                        .map_err(|err| CompileError::new(err.code, err.message, element))?,
                ),
            );
        }
        Ok(cells)
    }

    fn table_row_expr(
        &mut self,
        syntax: &mut SyntaxCompiler<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        node: P<Node>,
        what: &str,
        hooks: &mut RejectSubDecisionHooks,
    ) -> Result<Expr, CompileError> {
        let fn_node = self.deref(checker, node)?;
        let fk = fn_node.kind();
        if fk != Kind::ArrowFunction && fk != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                format!("a table row's {what} is an inline callback"),
                node,
            ));
        }
        let found = syntax.decision_rows(fn_node, checker, expansion, Some(hooks))?;
        let mut iter = found.into_iter();
        let (Some(first), None) = (iter.next(), iter.next()) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                format!("a table row's {what} is one expression, without branches"),
                fn_node,
            ));
        };
        if first.condition.is_some() {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                format!("a table row's {what} is one expression, without branches"),
                fn_node,
            ));
        }
        Ok(first.value)
    }

    fn compile_trigger(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["on", "type", "transition", "source"],
            &["on"],
        )?;
        let on_node = Self::member(&m, "on", input)?;
        let on = self.string(checker, on_node)?;
        let check_only = |allowed: &[&str]| -> Result<(), CompileError> {
            for (k, v) in m.iter() {
                if k != "on" && !allowed.contains(&k) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("a {on} trigger takes no {k}"),
                        v,
                    ));
                }
            }
            for &k in allowed {
                if !m.has(k) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("a {on} trigger needs {k}"),
                        input,
                    ));
                }
            }
            Ok(())
        };
        let mut obj = Map::new();
        obj.insert("on".to_owned(), Value::String(on.clone()));
        match on.as_str() {
            "transitioned" => {
                check_only(&["type", "transition"])?;
                obj.insert(
                    "objectType".to_owned(),
                    Value::String(self.type_name(
                        checker,
                        Self::member(&m, "type", input)?,
                        OBJECT_TYPES,
                    )?),
                );
                obj.insert(
                    "transition".to_owned(),
                    Value::String(self.string(checker, Self::member(&m, "transition", input)?)?),
                );
            }
            "created" | "deleted" => {
                check_only(&["type"])?;
                obj.insert(
                    "objectType".to_owned(),
                    Value::String(self.type_name(
                        checker,
                        Self::member(&m, "type", input)?,
                        OBJECT_TYPES,
                    )?),
                );
            }
            "observed" => {
                check_only(&["source"])?;
                obj.insert(
                    "source".to_owned(),
                    Value::String(self.string(checker, Self::member(&m, "source", input)?)?),
                );
            }
            _ => {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a trigger is transitioned, created, deleted, or observed",
                    on_node,
                ));
            }
        }
        Ok(Value::Object(obj))
    }
}

pub(super) fn build_decision_table_graph(
    hit: &str,
    outputs: &[&str],
    calls: Vec<Value>,
    rules: Vec<Value>,
) -> Value {
    let mut in_node = Map::new();
    in_node.insert("id".to_owned(), Value::String("in".to_owned()));
    in_node.insert("name".to_owned(), Value::String("input".to_owned()));
    in_node.insert("type".to_owned(), Value::String("inputNode".to_owned()));

    let mut cond_in = Map::new();
    cond_in.insert("id".to_owned(), Value::String("condition".to_owned()));
    cond_in.insert("name".to_owned(), Value::String("condition".to_owned()));
    cond_in.insert("type".to_owned(), Value::String("expression".to_owned()));

    let out_cols: Vec<Value> = outputs
        .iter()
        .map(|&field| {
            let mut col = Map::new();
            col.insert("id".to_owned(), Value::String(format!("out_{field}")));
            col.insert("name".to_owned(), Value::String(field.to_owned()));
            col.insert("field".to_owned(), Value::String(field.to_owned()));
            Value::Object(col)
        })
        .collect();

    let mut content = Map::new();
    content.insert("hitPolicy".to_owned(), Value::String(hit.to_owned()));
    content.insert(
        "inputs".to_owned(),
        Value::Array(vec![Value::Object(cond_in)]),
    );
    content.insert("outputs".to_owned(), Value::Array(out_cols));
    content.insert("rules".to_owned(), Value::Array(rules));

    let mut dec_node = Map::new();
    dec_node.insert("id".to_owned(), Value::String("decision".to_owned()));
    dec_node.insert("name".to_owned(), Value::String("decision".to_owned()));
    dec_node.insert(
        "type".to_owned(),
        Value::String("decisionTableNode".to_owned()),
    );
    dec_node.insert("content".to_owned(), Value::Object(content));

    let mut out_node = Map::new();
    out_node.insert("id".to_owned(), Value::String("out".to_owned()));
    out_node.insert("name".to_owned(), Value::String("output".to_owned()));
    out_node.insert("type".to_owned(), Value::String("outputNode".to_owned()));

    let mut chain: Vec<String> = Vec::with_capacity(calls.len().saturating_add(3));
    chain.push("in".to_owned());
    for c in &calls {
        if let Some(cid) = c.get("id").and_then(Value::as_str) {
            chain.push(cid.to_owned());
        }
    }
    chain.push("decision".to_owned());
    chain.push("out".to_owned());

    let mut nodes = Vec::with_capacity(calls.len().saturating_add(3));
    nodes.push(Value::Object(in_node));
    nodes.extend(calls);
    nodes.push(Value::Object(dec_node));
    nodes.push(Value::Object(out_node));

    let edges: Vec<Value> = chain
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            let src = pair.first()?;
            let dst = pair.get(1)?;
            let mut e = Map::new();
            e.insert("id".to_owned(), Value::String(format!("edge_{index}")));
            e.insert("sourceId".to_owned(), Value::String(src.clone()));
            e.insert("targetId".to_owned(), Value::String(dst.clone()));
            e.insert("type".to_owned(), Value::String("edge".to_owned()));
            Some(Value::Object(e))
        })
        .collect();

    let mut graph = Map::new();
    graph.insert(
        "contentType".to_owned(),
        Value::String(JDM_CONTENT_TYPE.to_owned()),
    );
    graph.insert("nodes".to_owned(), Value::Array(nodes));
    graph.insert("edges".to_owned(), Value::Array(edges));
    Value::Object(graph)
}

pub(super) fn build_exact_expression_graph(expressions: Vec<Value>) -> Value {
    let mut in_node = Map::new();
    in_node.insert("id".to_owned(), Value::String("in".to_owned()));
    in_node.insert("name".to_owned(), Value::String("input".to_owned()));
    in_node.insert("type".to_owned(), Value::String("inputNode".to_owned()));

    let mut content = Map::new();
    content.insert("expressions".to_owned(), Value::Array(expressions));
    let mut dec_node = Map::new();
    dec_node.insert("id".to_owned(), Value::String("decision".to_owned()));
    dec_node.insert("name".to_owned(), Value::String("decision".to_owned()));
    dec_node.insert(
        "type".to_owned(),
        Value::String("expressionNode".to_owned()),
    );
    dec_node.insert("content".to_owned(), Value::Object(content));

    let mut out_node = Map::new();
    out_node.insert("id".to_owned(), Value::String("out".to_owned()));
    out_node.insert("name".to_owned(), Value::String("output".to_owned()));
    out_node.insert("type".to_owned(), Value::String("outputNode".to_owned()));

    let make_edge = |id: &str, src: &str, dst: &str| {
        let mut e = Map::new();
        e.insert("id".to_owned(), Value::String(id.to_owned()));
        e.insert("sourceId".to_owned(), Value::String(src.to_owned()));
        e.insert("targetId".to_owned(), Value::String(dst.to_owned()));
        Value::Object(e)
    };
    let mut graph = Map::new();
    graph.insert(
        "contentType".to_owned(),
        Value::String(JDM_CONTENT_TYPE.to_owned()),
    );
    graph.insert(
        "nodes".to_owned(),
        Value::Array(vec![
            Value::Object(in_node),
            Value::Object(dec_node),
            Value::Object(out_node),
        ]),
    );
    graph.insert(
        "edges".to_owned(),
        Value::Array(vec![
            make_edge("edge_0", "in", "decision"),
            make_edge("edge_1", "decision", "out"),
        ]),
    );
    Value::Object(graph)
}
