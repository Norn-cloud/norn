//! Flow root and statement-sequence lowering (`root`, `statements`, `switch`)
//! split out of [`super::flow_syntax`].

use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use crate::checker::cfg::{Terminator, returns, terminator};
use crate::checker::resolve::node_text;

use super::expr::{CompareOp, Expr};
use super::flow_source_map::{FlowSourceMapStepKind, RouteBranch, Step};
use super::flow_syntax::{Env, SyntaxCompiler, json_scalar_to_string};
use super::flow_syntax_effects::EffectHint;
use super::types::CompileError;
use super::value_schema::ExpansionBudget;

/// Grouped `switch` clause mirroring `switchClauses` in `tools/norn-ts/src/compiler/cfg.ts`.
struct SwitchGroup {
    labels: Vec<P<Node>>,
    fallback: bool,
    body: Vec<P<Node>>,
    terminator: Terminator,
}

impl SyntaxCompiler<'_> {
    pub(super) fn root(
        &mut self,
        input: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<Step>, CompileError> {
        let e = Self::unwrap(input);
        if e.kind() == Kind::CallExpression {
            if let Some((base, operators)) = self.pipe_call(checker, e)
                && operators.iter().all(|&op| {
                    op.kind() == Kind::CallExpression
                        && self.call_id(checker, op).as_deref() == Some("effect/Effect#catchTag")
                })
            {
                return self.root_catch_tags(base, &operators, env, checker, expansion);
            }
            if self.call_id(checker, e).as_deref() == Some("effect/Effect#gen")
                && let Some(&fn_node) = e.as_call_expression().arguments().nodes().first()
                && fn_node.kind() == Kind::FunctionExpression
                && let Some(body) = fn_node.body()
            {
                return self.statements(
                    body.as_block().statements.nodes(),
                    env,
                    true,
                    false,
                    checker,
                    expansion,
                );
            }
        }
        let plan = self.effect(e, env, EffectHint::None, checker, expansion)?;
        let mut steps = plan.steps;
        steps.push(self.annotate(
            Step::Succeed {
                value: plan.value,
                at: Some(self.at(e)),
                source_map: None,
            },
            e,
            FlowSourceMapStepKind::Succeed,
            None,
            None,
            None,
        ));
        Ok(steps)
    }

    fn root_catch_tags(
        &mut self,
        base: P<Node>,
        operators: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<Step>, CompileError> {
        let mut body = self.root(base, env, checker, expansion)?;
        for &op in operators {
            let args = op.as_call_expression().arguments().nodes();
            let (Some(&tag_arg), Some(&cb_arg)) = (args.first(), args.get(1)) else {
                return Err(CompileError::new(
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "catchTag requires tag and handler",
                    op,
                ));
            };
            let fn_node = Self::callback_node(cb_arg)?;
            let param = fn_node.parameters().first().copied();
            let as_name = param.map(|p| {
                let sf = get_source_file_of_node(p).unwrap_or(self.source);
                let text = node_text(sf, p.as_parameter_declaration().name());
                self.fresh(&text)
            });
            let next_env = match &as_name {
                Some(name) => {
                    self.callback_env(fn_node, Expr::input(name.clone(), Vec::new()), env, checker)?
                }
                None => env.clone(),
            };
            let Some(fn_body) = fn_node.body() else {
                return Err(CompileError::new(
                    "NORN-FLOW-CAPTURE",
                    "catchTag handler needs a body",
                    fn_node,
                ));
            };
            let handler = if fn_body.kind() == Kind::Block {
                self.statements(
                    fn_body.as_block().statements.nodes(),
                    &next_env,
                    true,
                    true,
                    checker,
                    expansion,
                )?
            } else {
                self.root(fn_body, &next_env, checker, expansion)?
            };
            let tag = json_scalar_to_string(&self.scalar(tag_arg, checker, expansion)?);
            body = vec![self.annotate(
                Step::CatchTag {
                    body,
                    tag,
                    as_name,
                    handler,
                    at: Some(self.at(op)),
                    source_map: None,
                },
                op,
                FlowSourceMapStepKind::CatchTag,
                None,
                None,
                None,
            )];
        }
        Ok(body)
    }

    pub(super) fn statements(
        &mut self,
        stmts: &[P<Node>],
        initial: &Env,
        root: bool,
        effect_return: bool,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<Step>, CompileError> {
        let mut env = initial.clone();
        let mut steps = Vec::new();
        for &stmt in stmts {
            self.check_budget(stmt)?;
            let kind = stmt.kind();
            if kind == Kind::VariableStatement {
                self.stmt_var(stmt, &mut env, &mut steps, checker, expansion)?;
            } else if kind == Kind::ExpressionStatement {
                self.stmt_yield_expr(stmt, &env, &mut steps, checker, expansion)?;
            } else if kind == Kind::ReturnStatement {
                self.stmt_return(
                    stmt,
                    &env,
                    &mut steps,
                    (root, effect_return),
                    checker,
                    expansion,
                )?;
            } else if kind == Kind::IfStatement {
                let step = self.stmt_if(stmt, &env, root, effect_return, checker, expansion)?;
                steps.push(step);
            } else if kind == Kind::SwitchStatement {
                let switch_step =
                    self.compile_switch(stmt, &env, root, effect_return, checker, expansion)?;
                steps.push(switch_step);
            } else if kind == Kind::Block {
                steps.extend(self.statements(
                    stmt.as_block().statements.nodes(),
                    &env,
                    root,
                    effect_return,
                    checker,
                    expansion,
                )?);
            } else if kind != Kind::EmptyStatement {
                return Err(CompileError::new(
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "statement has no lowering",
                    stmt,
                ));
            }
            if returns(stmt) {
                break;
            }
        }
        Ok(steps)
    }

    fn stmt_var(
        &mut self,
        stmt: P<Node>,
        env: &mut Env,
        steps: &mut Vec<Step>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<(), CompileError> {
        let decls = stmt
            .as_variable_statement()
            .declaration_list
            .as_variable_declaration_list()
            .declarations
            .nodes();
        for &decl in decls {
            let vd = decl.as_variable_declaration();
            let Some(init) = vd.initializer() else {
                return Err(CompileError::new(
                    "NORN-FLOW-CAPTURE",
                    "const binding needs an identifier and value",
                    decl,
                ));
            };
            if vd.name().kind() != Kind::Identifier {
                return Err(CompileError::new(
                    "NORN-FLOW-CAPTURE",
                    "const binding needs an identifier and value",
                    decl,
                ));
            }
            let unwrapped = Self::unwrap(init);
            let value = if unwrapped.kind() == Kind::YieldExpression {
                let Some(y_expr) = unwrapped.as_yield_expression().expression else {
                    return Err(CompileError::new(
                        "NORN-FLOW-UNSUPPORTED-SYNTAX",
                        "expected a yielded effect",
                        unwrapped,
                    ));
                };
                let plan = self.effect(
                    y_expr,
                    env,
                    EffectHint::Bind(vd.name().text()),
                    checker,
                    expansion,
                )?;
                steps.extend(plan.steps);
                plan.value
            } else {
                self.data(unwrapped, env, checker, expansion, None)?
            };
            let sym = self.symbol(checker, vd.name())?;
            env.insert(sym, value);
        }
        Ok(())
    }

    fn stmt_yield_expr(
        &mut self,
        stmt: P<Node>,
        env: &Env,
        steps: &mut Vec<Step>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<(), CompileError> {
        let expr = stmt.as_expression_statement().expression;
        if expr.kind() != Kind::YieldExpression {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "expected a yielded effect",
                stmt,
            ));
        }
        let Some(y_expr) = expr.as_yield_expression().expression else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "expected a yielded effect",
                stmt,
            ));
        };
        let plan = self.effect(y_expr, env, EffectHint::Discard, checker, expansion)?;
        steps.extend(plan.steps);
        Ok(())
    }

    fn stmt_return(
        &mut self,
        stmt: P<Node>,
        env: &Env,
        steps: &mut Vec<Step>,
        flags: (bool, bool),
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<(), CompileError> {
        let (root, effect_return) = flags;
        let Some(ret_expr) = stmt.as_return_statement().expression() else {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "return an explicit JSON result",
                stmt,
            ));
        };
        if effect_return {
            if root {
                steps.extend(self.root(ret_expr, env, checker, expansion)?);
            } else {
                let plan = self.effect(ret_expr, env, EffectHint::None, checker, expansion)?;
                steps.extend(plan.steps);
                steps.push(self.returned(plan.value, stmt, false));
            }
        } else if ret_expr.kind() == Kind::YieldExpression {
            let Some(y_expr) = ret_expr.as_yield_expression().expression else {
                return Err(CompileError::new(
                    "NORN-FLOW-UNSUPPORTED-SYNTAX",
                    "expected a yielded effect",
                    ret_expr,
                ));
            };
            let plan = self.effect(y_expr, env, EffectHint::None, checker, expansion)?;
            steps.extend(plan.steps);
            steps.push(self.returned(plan.value, stmt, root));
        } else {
            let val = self.data(ret_expr, env, checker, expansion, None)?;
            steps.push(self.returned(val, stmt, root));
        }
        Ok(())
    }

    fn stmt_if(
        &mut self,
        stmt: P<Node>,
        env: &Env,
        root: bool,
        effect_return: bool,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Step, CompileError> {
        let if_stmt = stmt.as_if_statement();
        let then_steps = self.statement_body(
            if_stmt.then_statement,
            env,
            root,
            effect_return,
            checker,
            expansion,
        )?;
        let otherwise = match if_stmt.else_statement {
            Some(else_stmt) => {
                self.statement_body(else_stmt, env, root, effect_return, checker, expansion)?
            }
            None => Vec::new(),
        };
        let when = self.data(if_stmt.expression, env, checker, expansion, None)?;
        Ok(self.annotate(
            Step::Route {
                branches: vec![RouteBranch {
                    when,
                    then: then_steps,
                }],
                otherwise,
                at: Some(self.at(stmt)),
                source_map: None,
            },
            stmt,
            FlowSourceMapStepKind::If,
            None,
            Some(vec!["then".to_owned()]),
            Some("else".to_owned()),
        ))
    }

    fn compile_switch(
        &mut self,
        stmt: P<Node>,
        env: &Env,
        root: bool,
        effect_return: bool,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Step, CompileError> {
        let sw = stmt.as_switch_statement();
        let disc = self.data(sw.expression, env, checker, expansion, None)?;
        let clauses = switch_clauses(stmt)?;
        let last_is_fallback = !clauses.iter().any(|c| c.fallback)
            && clauses.iter().all(|c| c.terminator == Terminator::Return);
        let mut branches = Vec::new();
        let mut otherwise = Vec::new();
        let mut default_present = false;
        let last_idx = clauses.len().saturating_sub(1);
        for (idx, clause) in clauses.iter().enumerate() {
            let body =
                self.statements(&clause.body, env, root, effect_return, checker, expansion)?;
            if clause.fallback || (last_is_fallback && idx == last_idx) {
                otherwise = body;
                default_present = clause.fallback;
            } else {
                let mut cond: Option<Expr> = None;
                for &label in &clause.labels {
                    let rhs = self.data(label, env, checker, expansion, None)?;
                    let eq = Expr::compare(CompareOp::Eq, disc.clone(), rhs);
                    cond = Some(match cond {
                        Some(prev) => Expr::or(prev, eq),
                        None => eq,
                    });
                }
                if let Some(when) = cond {
                    branches.push(RouteBranch { when, then: body });
                }
            }
        }
        let labels = (0..branches.len())
            .map(|i| format!("case-{}", i.saturating_add(1)))
            .collect();
        let otherwise_label = if default_present {
            "default".to_owned()
        } else {
            "otherwise".to_owned()
        };
        Ok(self.annotate(
            Step::Route {
                branches,
                otherwise,
                at: Some(self.at(stmt)),
                source_map: None,
            },
            stmt,
            FlowSourceMapStepKind::Switch,
            None,
            Some(labels),
            Some(otherwise_label),
        ))
    }

    fn statement_body(
        &mut self,
        stmt: P<Node>,
        env: &Env,
        root: bool,
        effect_return: bool,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<Step>, CompileError> {
        if stmt.kind() == Kind::Block {
            self.statements(
                stmt.as_block().statements.nodes(),
                env,
                root,
                effect_return,
                checker,
                expansion,
            )
        } else {
            self.statements(&[stmt], env, root, effect_return, checker, expansion)
        }
    }
}

fn switch_clauses(stmt: P<Node>) -> Result<Vec<SwitchGroup>, CompileError> {
    let mut groups = Vec::new();
    let mut labels = Vec::new();
    let mut fallback = false;
    let clauses = stmt
        .as_switch_statement()
        .case_block
        .as_case_block()
        .clauses
        .nodes();
    let len = clauses.len();
    for (idx, &clause) in clauses.iter().enumerate() {
        if clause.kind() == Kind::CaseClause {
            if let Some(expr) = clause.as_case_or_default_clause().expression() {
                labels.push(expr);
            }
        } else {
            fallback = true;
        }
        let is_last = idx.saturating_add(1) == len;
        let exit = terminator(clause, is_last);
        let stmts = clause.as_case_or_default_clause().statements.nodes();
        if exit == Terminator::Fallthrough {
            if !stmts.is_empty() {
                return Err(CompileError::new(
                    "NORN-FLOW-ROUTE",
                    "nonempty switch clause falls through",
                    clause,
                ));
            }
            continue;
        }
        let body = if exit == Terminator::Break {
            stmts
                .get(..stmts.len().saturating_sub(1))
                .unwrap_or(&[])
                .to_vec()
        } else {
            stmts.to_vec()
        };
        groups.push(SwitchGroup {
            labels: std::mem::take(&mut labels),
            fallback,
            body,
            terminator: exit,
        });
        fallback = false;
    }
    Ok(groups)
}
