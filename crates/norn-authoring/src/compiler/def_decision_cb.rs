//! Decision callback lowering, sub-decision `Norn.evaluate`, `Norn.orderedFold`,
//! and exact numeric `expressionNode` lowering split out of [`super::def_decisions`].

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::to_canonical_json;
use super::def_decisions::{build_decision_table_graph, build_exact_expression_graph};
use super::def_helpers::{DECISIONS, DefinitionCompiler, DefinitionSource, Members};
use super::expr::{Binding, Expr, Scope, assignment_error, render, type_of};
use super::flow_syntax::{DecisionHooks, DecisionRow, SyntaxCompiler};
use super::schema::NullableMode;
use super::types::{CompileError, DependencyPin};
use super::value_schema::{ExpansionBudget, ObjectSchema, ValueSchema};
use super::zen_reads::lexical_bounds;

pub(super) struct CallbackGraphSpec<'a> {
    pub(super) body: P<Node>,
    pub(super) hit: &'a str,
    pub(super) artifact_id: &'a str,
    pub(super) input_schema: &'a ObjectSchema,
    pub(super) output_schema: &'a ObjectSchema,
    pub(super) call: P<Node>,
}

struct OrderedFoldTarget<'a> {
    site: P<Node>,
    input: &'a Expr,
    child_call: P<Node>,
    child_members: &'a Members,
    output: &'a ValueSchema,
}

struct CallbackDecisionHooks<'c, 'a> {
    compiler: &'c mut DefinitionCompiler<'a>,
    body_fn: P<Node>,
    input_schema: &'c ObjectSchema,
    scope: &'c mut Scope,
    calls: &'c mut Vec<Value>,
    called_deps: &'c mut Vec<DependencyPin>,
}

impl DecisionHooks for CallbackDecisionHooks<'_, '_> {
    fn on_sub_decision(
        &mut self,
        site: P<Node>,
        input: Expr,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Expr, CompileError> {
        self.validate_sub_decision_site(site)?;
        let args = site.as_call_expression().arguments().nodes();
        let Some(&child_arg) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "expected a reference to a Norn.decision definition",
                site,
            ));
        };
        let (child_call, child_members) = self.compiler.referenced(
            checker,
            child_arg,
            DECISIONS,
            "a Norn.decision definition",
        )?;
        let child_contract = self.compiler.string(
            checker,
            DefinitionCompiler::member(&child_members, "contract", child_call)?,
        )?;
        if child_contract != "sub_decision" {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "evaluate requires contract sub_decision",
                child_arg,
            ));
        }
        let pin = self.compiler.decision_dependency(checker, child_arg)?;
        if !self
            .called_deps
            .iter()
            .any(|p| p.artifact_id == pin.artifact_id && p.version == pin.version)
        {
            self.called_deps.push(pin.clone());
        }
        let name = format!("_norn_sub_{}", self.calls.len());
        if self.input_schema.properties.contains_key(&name) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{name} is reserved for compiler results"),
                site,
            ));
        }
        let out_node = DefinitionCompiler::member(&child_members, "output", child_call)?;
        let out_read = self.compiler.schemas.read(checker, out_node)?;
        let output =
            self.compiler
                .schemas
                .closed(&out_read, NullableMode::Any, false, expansion)?;
        let mut input_field = render(&input, self.scope)
            .map_err(|err| CompileError::new(err.code, err.message, site))?;
        let mut ordered_fold: Option<Value> = None;
        if self.compiler.call_id(checker, site).as_deref()
            == Some("@norn/authoring#Norn.orderedFold")
        {
            let target = OrderedFoldTarget {
                site,
                input: &input,
                child_call,
                child_members: &child_members,
                output: &output,
            };
            let (fold_input_field, fold_val) =
                self.build_ordered_fold(&target, checker, expansion)?;
            input_field = fold_input_field;
            ordered_fold = Some(fold_val);
        }
        self.push_sub_decision_node(
            pin.artifact_id,
            name.clone(),
            input_field,
            ordered_fold,
            output,
        );
        Ok(Expr::input(name, Vec::new()))
    }

    fn on_escape(
        &mut self,
        _node: P<Node>,
        text: &str,
        schema: ValueSchema,
    ) -> Result<Expr, CompileError> {
        let name = format!("@escape:{}", self.scope.len());
        self.scope.insert(
            name.clone(),
            Binding::Zen {
                zen: format!("({text})"),
                schema,
            },
        );
        Ok(Expr::input(name, Vec::new()))
    }
}

impl CallbackDecisionHooks<'_, '_> {
    fn push_sub_decision_node(
        &mut self,
        artifact_id: String,
        name: String,
        input_field: String,
        ordered_fold: Option<Value>,
        output: ValueSchema,
    ) {
        let mut content = Map::new();
        content.insert("key".to_owned(), Value::String(artifact_id));
        content.insert("inputField".to_owned(), Value::String(input_field));
        content.insert("outputPath".to_owned(), Value::String(name.clone()));
        content.insert("passThrough".to_owned(), Value::Bool(true));
        if let Some(fold) = ordered_fold {
            content.insert("orderedFold".to_owned(), fold);
        }
        let mut node_obj = Map::new();
        node_obj.insert("id".to_owned(), Value::String(name.clone()));
        node_obj.insert("name".to_owned(), Value::String(name.clone()));
        node_obj.insert("type".to_owned(), Value::String("decisionNode".to_owned()));
        node_obj.insert("content".to_owned(), Value::Object(content));
        self.calls.push(Value::Object(node_obj));
        self.scope.insert(
            name.clone(),
            Binding::Zen {
                zen: name,
                schema: output,
            },
        );
    }

    fn validate_sub_decision_site(&self, site: P<Node>) -> Result<(), CompileError> {
        let valid_site = site
            .parent()
            .filter(|p| p.kind() == Kind::VariableDeclaration)
            .and_then(|vd| vd.parent())
            .filter(|vdl| vdl.kind() == Kind::VariableDeclarationList)
            .and_then(|vdl| vdl.parent())
            .filter(|vs| vs.kind() == Kind::VariableStatement)
            .filter(|vs| vs.parent() == self.body_fn.body());
        let Some(declaration) = valid_site else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "evaluate a sub-decision in a top-level const before branching",
                site,
            ));
        };
        let Some(body_block) = self.body_fn.body().filter(|b| b.kind() == Kind::Block) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "evaluate a sub-decision in the unconditional const prefix before branching",
                site,
            ));
        };
        let stmts = body_block.as_block().statements.nodes();
        let pos = stmts.iter().position(|&s| s == declaration).unwrap_or(0);
        if stmts
            .get(..pos)
            .unwrap_or(&[])
            .iter()
            .any(|s| s.kind() != Kind::VariableStatement)
        {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "evaluate a sub-decision in the unconditional const prefix before branching",
                site,
            ));
        }
        Ok(())
    }

    fn build_ordered_fold(
        &mut self,
        target: &OrderedFoldTarget<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<(String, Value), CompileError> {
        let args = target.site.as_call_expression().arguments().nodes();
        let (Expr::Record(fields), Some(&policy_arg)) = (target.input, args.get(2)) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "orderedFold needs a step, {items, initial} and literal policy",
                target.site,
            ));
        };
        if args.len() != 3 {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "orderedFold needs a step, {items, initial} and literal policy",
                target.site,
            ));
        }
        let items = fields.iter().find(|(k, _)| k == "items").map(|(_, v)| v);
        let initial = fields.iter().find(|(k, _)| k == "initial").map(|(_, v)| v);
        let (Some(items), Some(initial)) = (items, initial) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "orderedFold input is exactly {items, initial}",
                target.site,
            ));
        };
        if fields.len() != 2 {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "orderedFold input is exactly {items, initial}",
                target.site,
            ));
        }
        let policy = self.validate_ordered_fold_policy(checker, policy_arg)?;
        let item_schema =
            self.validate_ordered_fold_schemas(target, items, initial, checker, expansion)?;
        let items_zen = render(items, self.scope)
            .map_err(|err| CompileError::new(err.code, err.message, target.site))?;
        let init_zen = render(initial, self.scope)
            .map_err(|err| CompileError::new(err.code, err.message, target.site))?;
        let fold = self.build_ordered_fold_json(
            checker,
            expansion,
            &policy,
            policy_arg,
            (&item_schema, target.output),
            init_zen,
        )?;
        Ok((items_zen, fold))
    }

    fn validate_ordered_fold_policy(
        &self,
        checker: &mut Checker,
        policy_arg: P<Node>,
    ) -> Result<Members, CompileError> {
        let policy = self.compiler.closed_record(
            checker,
            policy_arg,
            &[
                "identity",
                "order",
                "maxItems",
                "maxAccumulatorBytes",
                "arithmetic",
            ],
            &[
                "identity",
                "order",
                "maxItems",
                "maxAccumulatorBytes",
                "arithmetic",
            ],
        )?;
        self.compiler.closed_record(
            checker,
            DefinitionCompiler::member(&policy, "arithmetic", policy_arg)?,
            &["scale", "rounding"],
            &["scale", "rounding"],
        )?;
        for key in self.compiler.list(
            checker,
            DefinitionCompiler::member(&policy, "order", policy_arg)?,
        )? {
            self.compiler.closed_record(
                checker,
                key,
                &["property", "kind"],
                &["property", "kind"],
            )?;
        }
        Ok(policy)
    }

    fn validate_ordered_fold_schemas(
        &mut self,
        target: &OrderedFoldTarget<'_>,
        items: &Expr,
        initial: &Expr,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<ValueSchema, CompileError> {
        let step_in_node =
            DefinitionCompiler::member(target.child_members, "input", target.child_call)?;
        let step_in_read = self.compiler.schemas.read(checker, step_in_node)?;
        let step_input =
            self.compiler
                .schemas
                .closed(&step_in_read, NullableMode::Any, false, expansion)?;
        let ValueSchema::Object(step_obj) = step_input else {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "orderedFold requires a closed step input and object state",
                target.site,
            ));
        };
        let Some(item_schema) = step_obj.properties.get("item").cloned() else {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "orderedFold requires a closed step input and object state",
                target.site,
            ));
        };
        if !matches!(target.output, ValueSchema::Object(_)) {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "orderedFold requires a closed step input and object state",
                target.site,
            ));
        }
        let items_err = assignment_error(
            items,
            &ValueSchema::array(item_schema.clone()),
            self.scope,
            "$",
        )
        .map_err(|err| CompileError::new("NORN-FLOW-TYPE", err.message, target.site))?;
        let init_err = assignment_error(initial, target.output, self.scope, "$")
            .map_err(|err| CompileError::new("NORN-FLOW-TYPE", err.message, target.site))?;
        if items_err.is_some() || init_err.is_some() {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "orderedFold items and initial must satisfy the helper's closed schemas",
                target.site,
            ));
        }
        Ok(item_schema)
    }

    fn build_ordered_fold_json(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        policy: &Members,
        policy_arg: P<Node>,
        schemas: (&ValueSchema, &ValueSchema),
        init_zen: String,
    ) -> Result<Value, CompileError> {
        let (item_schema, output) = schemas;
        let mut fold = Map::new();
        fold.insert(
            "semanticsVersion".to_owned(),
            Value::String("1.0".to_owned()),
        );
        for key in [
            "identity",
            "order",
            "arithmetic",
            "maxItems",
            "maxAccumulatorBytes",
        ] {
            fold.insert(
                key.to_owned(),
                self.compiler.json(
                    checker,
                    expansion,
                    DefinitionCompiler::member(policy, key, policy_arg)?,
                    0,
                )?,
            );
        }
        let item_val = item_schema.to_json();
        let acc_val = output.to_json();
        let item_canon = to_canonical_json(&item_val)
            .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), policy_arg))?;
        let acc_canon = to_canonical_json(&acc_val)
            .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), policy_arg))?;
        fold.insert("itemSchema".to_owned(), item_canon);
        fold.insert("accumulatorSchema".to_owned(), acc_canon);
        fold.insert("initial".to_owned(), Value::String(init_zen));
        Ok(Value::Object(fold))
    }
}

impl DefinitionCompiler<'_> {
    pub(super) fn compile_decision_callback(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: CallbackGraphSpec<'_>,
        called_deps: &mut Vec<DependencyPin>,
    ) -> Result<Value, CompileError> {
        let bk = spec.body.kind();
        if bk != Kind::ArrowFunction && bk != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "decision body must be an inline callback",
                spec.body,
            ));
        }
        let Some(sf) = get_source_file_of_node(spec.call) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "missing source file",
                spec.call,
            ));
        };
        let mut scope = Scope::new();
        scope.insert(
            "input".to_owned(),
            Binding::Zen {
                zen: String::new(),
                schema: ValueSchema::object(spec.input_schema.clone()),
            },
        );
        let mut calls: Vec<Value> = Vec::new();
        let mut syntax = self.syntax_compiler(sf);
        let rows = {
            let mut hooks = CallbackDecisionHooks {
                compiler: self,
                body_fn: spec.body,
                input_schema: spec.input_schema,
                scope: &mut scope,
                calls: &mut calls,
                called_deps,
            };
            syntax.decision_rows(spec.body, checker, expansion, Some(&mut hooks))?
        };
        self.warnings.extend(syntax.warnings.drain(..));

        let exact = ValueSchema::object(spec.input_schema.clone()).has_exact()
            || ValueSchema::object(spec.output_schema.clone()).has_exact();
        if exact && !calls.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "exact numeric schemas currently require a direct callback without sub-decisions",
                spec.body,
            ));
        }
        let outputs: Vec<&str> = spec.output_schema.ordered_keys().collect();
        let table_idx = calls.len().saturating_add(1);
        let rules =
            self.render_callback_rows(&syntax, &rows, &scope, &outputs, &spec, (table_idx, exact))?;
        if spec.hit == "unique" {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "a decision callback's branches are tried in order, so they cannot overlap; declare a unique table as a list of { when, then } rows",
                spec.body,
            ));
        }
        if exact {
            return self.lower_exact_callback(&outputs, &rules, spec.artifact_id, spec.body);
        }
        let rule_vals = rules.into_iter().map(Value::Object).collect();
        Ok(build_decision_table_graph(
            spec.hit, &outputs, calls, rule_vals,
        ))
    }

    fn render_callback_rows(
        &mut self,
        syntax: &SyntaxCompiler<'_>,
        rows: &[DecisionRow],
        scope: &Scope,
        outputs: &[&str],
        spec: &CallbackGraphSpec<'_>,
        target: (usize, bool),
    ) -> Result<Vec<Map<String, Value>>, CompileError> {
        let (table_idx, exact) = target;
        let mut rules: Vec<Map<String, Value>> = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            if let Some(cond) = &row.condition {
                let cond_ty = type_of(cond, scope)
                    .map_err(|err| CompileError::new(err.code, err.message, spec.body))?;
                if !matches!(cond_ty, ValueSchema::Boolean { .. }) {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "expected a boolean row condition",
                        spec.body,
                    ));
                }
                if !exact {
                    let node = syntax.find_expression_source(cond).unwrap_or(spec.body);
                    self.source_map.push(DefinitionSource {
                        artifact_id: spec.artifact_id.to_owned(),
                        pointer: format!(
                            "/payload/graph/nodes/{table_idx}/content/rules/{i}/condition"
                        ),
                        at: self.at(node),
                    });
                }
            }
            let mut cells = Map::new();
            cells.insert("_id".to_owned(), Value::String(format!("row_{i}")));
            let cond_text = match &row.condition {
                Some(cond) => render(cond, scope)
                    .map_err(|err| CompileError::new(err.code, err.message, spec.body))?,
                None => String::new(),
            };
            cells.insert("condition".to_owned(), Value::String(cond_text));
            for &field in outputs {
                let Some(projected) = project_field(&row.value, field) else {
                    return Err(CompileError::new(
                        "NORN-FLOW-RESULT",
                        format!("expected output field {field}; return every declared output"),
                        spec.body,
                    ));
                };
                if !exact {
                    let escaped = field.replace('~', "~0").replace('/', "~1");
                    let node = syntax
                        .find_expression_source(&projected)
                        .unwrap_or(spec.body);
                    self.source_map.push(DefinitionSource {
                        artifact_id: spec.artifact_id.to_owned(),
                        pointer: format!(
                            "/payload/graph/nodes/{table_idx}/content/rules/{i}/out_{escaped}"
                        ),
                        at: self.at(node),
                    });
                }
                let rendered = render(&projected, scope)
                    .map_err(|err| CompileError::new(err.code, err.message, spec.body))?;
                cells.insert(format!("out_{field}"), Value::String(rendered));
            }
            rules.push(cells);
        }
        Ok(rules)
    }

    fn lower_exact_callback(
        &mut self,
        outputs: &[&str],
        rules: &[Map<String, Value>],
        artifact_id: &str,
        body: P<Node>,
    ) -> Result<Value, CompileError> {
        let mut expressions = Vec::with_capacity(outputs.len());
        for (index, &key) in outputs.iter().enumerate() {
            let out_col = format!("out_{key}");
            let mut value = rules
                .last()
                .and_then(|r| r.get(&out_col))
                .and_then(Value::as_str)
                .unwrap_or("null")
                .to_owned();
            let mut row_idx = rules.len().saturating_sub(1);
            while row_idx > 0 {
                row_idx = row_idx.saturating_sub(1);
                if let Some(cells) = rules.get(row_idx) {
                    let cond = cells.get("condition").and_then(Value::as_str).unwrap_or("");
                    let cell = cells
                        .get(&out_col)
                        .and_then(Value::as_str)
                        .unwrap_or("null");
                    value = if cond.is_empty() {
                        cell.to_owned()
                    } else {
                        format!("({cond}) ? ({cell}) : ({value})")
                    };
                }
            }
            if !lexical_bounds(&value) {
                return Err(CompileError::new(
                    "NORN-FLOW-UNBOUNDED",
                    "exact decision output exceeds norn-expr/4 source bounds (16384 UTF-8 bytes, 128 bracket levels, 512 recursion-opening tokens)",
                    body,
                ));
            }
            self.source_map.push(DefinitionSource {
                artifact_id: artifact_id.to_owned(),
                pointer: format!("/payload/graph/nodes/1/content/expressions/{index}/value"),
                at: self.at(body),
            });
            let mut ex = Map::new();
            ex.insert("id".to_owned(), Value::String(format!("out_{key}")));
            ex.insert("key".to_owned(), Value::String(key.to_owned()));
            ex.insert("value".to_owned(), Value::String(value));
            expressions.push(Value::Object(ex));
        }
        Ok(build_exact_expression_graph(expressions))
    }
}

fn project_field(value: &Expr, field: &str) -> Option<Expr> {
    match value {
        Expr::Record(fields) => fields
            .iter()
            .find(|(k, _)| k == field)
            .map(|(_, v)| v.clone()),
        Expr::Input { name, path } => {
            let mut next_path = path.clone();
            next_path.push(field.to_owned());
            Some(Expr::input(name.clone(), next_path))
        }
        Expr::Conditional { condition, yes, no } => {
            let y = project_field(yes, field)?;
            let n = project_field(no, field)?;
            Some(Expr::conditional((**condition).clone(), y, n))
        }
        Expr::Binary { .. }
        | Expr::Call { .. }
        | Expr::Closure { .. }
        | Expr::Coalesce { .. }
        | Expr::Bound { .. }
        | Expr::Literal(_)
        | Expr::Compare { .. }
        | Expr::Logical { .. }
        | Expr::Not(_)
        | Expr::List(_) => None,
    }
}
