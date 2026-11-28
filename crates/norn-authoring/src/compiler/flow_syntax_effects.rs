//! Effect expression & pipe-operator lowering split out of [`super::flow_syntax`].

use serde_json::Value;
use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::{Checker, Type, TypeFlags};
use tsrs_core::P;

use crate::checker::resolve::{list_nodes, node_text};

use super::expr::Expr;
use super::flow_source_map::{FlowSourceMapStepKind, PerformStep, Step};
use super::flow_syntax::{Env, Plan, SyntaxCompiler, json_scalar_to_string};
use super::types::CompileError;
use super::value_schema::{BudgetExceeded, ExpansionBudget, ValueSchema, merge_schemas};

struct OperatorCall<'a> {
    call: P<Node>,
    plan: Plan,
    env: &'a Env,
    hint: Option<Option<&'a str>>,
    direct: bool,
}

impl SyntaxCompiler<'_> {
    pub(super) fn pipe_call(
        &self,
        checker: &mut Checker,
        call: P<Node>,
    ) -> Option<(P<Node>, Vec<P<Node>>)> {
        let id = self.call_id(checker, call)?;
        let ce = call.as_call_expression();
        let args = ce.arguments().nodes();
        if id == "effect/Pipeable#Pipeable.pipe"
            && ce.expression().kind() == Kind::PropertyAccessExpression
        {
            return Some((
                ce.expression().as_property_access_expression().expression(),
                args.to_vec(),
            ));
        }
        if id == "effect/Function#pipe"
            && let Some((&base, rest)) = args.split_first()
        {
            return Some((base, rest.to_vec()));
        }
        None
    }

    pub(super) fn callback_node(expr: P<Node>) -> Result<P<Node>, CompileError> {
        let k = expr.kind();
        if k != Kind::ArrowFunction && k != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "continuation must be inline syntax",
                expr,
            ));
        }
        Ok(expr)
    }

    pub(super) fn callback_env(
        &self,
        fn_node: P<Node>,
        value: Expr,
        env: &Env,
        checker: &mut Checker,
    ) -> Result<Env, CompileError> {
        let mut next = env.clone();
        if let Some(&param) = fn_node.parameters().first() {
            let sym = self.symbol(checker, param.as_parameter_declaration().name())?;
            next.insert(sym, value);
        }
        Ok(next)
    }

    fn effect_outputs(
        checker: &mut Checker,
        ty: P<Type>,
        node: P<Node>,
    ) -> Result<Vec<P<Type>>, CompileError> {
        if ty.flags().intersects(TypeFlags::Never) {
            return Ok(Vec::new());
        }
        if ty.is_union() {
            let mut out = Vec::new();
            for &branch in ty.types() {
                out.extend(Self::effect_outputs(checker, branch, node)?);
            }
            return Ok(out);
        }
        let args = if ty.flags().intersects(TypeFlags::Object) {
            checker.get_type_arguments_exported(ty)
        } else {
            &[]
        };
        let Some(&first) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "effect output needs a closed value type",
                node,
            ));
        };
        if first.flags().intersects(TypeFlags::Never) {
            Ok(Vec::new())
        } else {
            Ok(vec![first])
        }
    }

    fn outputs_of(expr: P<Node>, checker: &mut Checker) -> Result<Vec<P<Type>>, CompileError> {
        let ty = checker.get_type_at_location(expr);
        Self::effect_outputs(checker, ty, expr)
    }

    fn output_schema(
        &mut self,
        outputs: &[P<Type>],
        node: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<ValueSchema, CompileError> {
        let Some((&first, rest)) = outputs.split_first() else {
            return Ok(ValueSchema::null_value());
        };
        let mut schema = self
            .schema_budget
            .result_schema(checker, expansion, first, node)?;
        for &output in rest {
            let next = self
                .schema_budget
                .result_schema(checker, expansion, output, node)?;
            schema = merge_schemas(&schema, &next, "$").map_err(|msg| {
                CompileError::new(
                    "NORN-FLOW-VALUE",
                    format!("effect results have no closed shared shape: {msg}"),
                    node,
                )
            })?;
            expansion
                .assert_schema(&schema)
                .map_err(|BudgetExceeded(msg)| {
                    CompileError::new("NORN-FLOW-UNBOUNDED", msg, node)
                })?;
        }
        Ok(schema)
    }

    fn compose(
        &mut self,
        body: Vec<Step>,
        outputs: Vec<P<Type>>,
        node: P<Node>,
        hint: Option<&str>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let schema = self.output_schema(&outputs, node, checker, expansion)?;
        let name = self.fresh(hint.unwrap_or("value"));
        let composed = self.annotate(
            Step::Compose {
                name: name.clone(),
                schema,
                body,
                at: Some(self.at(node)),
                source_map: None,
            },
            node,
            FlowSourceMapStepKind::Compose,
            None,
            None,
            None,
        );
        Ok(Plan {
            steps: vec![composed],
            value: Expr::input(name, Vec::new()),
            outputs,
        })
    }

    pub(super) fn returned(&mut self, value: Expr, node: P<Node>, root: bool) -> Step {
        if root {
            self.annotate(
                Step::Succeed {
                    value,
                    at: Some(self.at(node)),
                    source_map: None,
                },
                node,
                FlowSourceMapStepKind::Succeed,
                None,
                None,
                None,
            )
        } else {
            Step::Value {
                value,
                at: Some(self.at(node)),
                source_map: None,
            }
        }
    }

    fn callback_plan(
        &mut self,
        fn_node: P<Node>,
        env: &Env,
        pure: bool,
        hint: Option<&str>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let sig = checker.get_signature_from_declaration_exported(fn_node);
        let return_ty = checker.get_return_type_of_signature_exported(sig);
        let Some(body) = fn_node.body() else {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "callback has no body",
                fn_node,
            ));
        };
        if body.kind() != Kind::Block {
            return if pure {
                Ok(Plan {
                    steps: Vec::new(),
                    value: self.data(body, env, checker, expansion, None)?,
                    outputs: vec![return_ty],
                })
            } else {
                self.effect(body, env, hint.map(Some), checker, expansion)
            };
        }
        let outputs = if pure {
            vec![return_ty]
        } else {
            Self::effect_outputs(checker, return_ty, fn_node)?
        };
        let body_steps = self.statements(
            body.as_block().statements.nodes(),
            env,
            false,
            !pure,
            checker,
            expansion,
        )?;
        self.compose(body_steps, outputs, fn_node, hint, checker, expansion)
    }

    /// Lowers an effect expression.
    /// `hint`: `None` = no hint, `Some(None)` = discard result, `Some(Some(s))` = binding hint.
    pub(super) fn effect(
        &mut self,
        input: P<Node>,
        env: &Env,
        hint: Option<Option<&str>>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        self.check_budget(input)?;
        let e = Self::unwrap(input);
        if e.kind() != Kind::CallExpression {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "expected visible effect syntax",
                e,
            ));
        }
        if let Some((base, operators)) = self.pipe_call(checker, e) {
            let mut plan = self.effect(base, env, hint, checker, expansion)?;
            for op in operators {
                if op.kind() != Kind::CallExpression {
                    return Err(CompileError::new(
                        "NORN-FLOW-CAPTURE",
                        "pipe operator must be visible syntax",
                        op,
                    ));
                }
                plan = self.operator(
                    OperatorCall {
                        call: op,
                        plan,
                        env,
                        hint,
                        direct: false,
                    },
                    checker,
                    expansion,
                )?;
            }
            return Ok(plan);
        }
        let args = e.as_call_expression().arguments().nodes();
        let id = self.call_id(checker, e);
        self.effect_call((e, args, id.as_deref()), env, hint, checker, expansion)
    }

    fn effect_call(
        &mut self,
        call: (P<Node>, &[P<Node>], Option<&str>),
        env: &Env,
        hint: Option<Option<&str>>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let (e, args, id) = call;
        match id {
            Some("@norn/authoring#Norn.perform") => {
                self.effect_perform(e, args, env, hint, checker, expansion)
            }
            Some("effect/Effect#succeed") => {
                let Some(&a0) = args.first() else {
                    return Err(CompileError::new(
                        "NORN-FLOW-UNSUPPORTED-SYNTAX",
                        "Effect.succeed requires a value",
                        e,
                    ));
                };
                Ok(Plan {
                    steps: Vec::new(),
                    value: self.data(a0, env, checker, expansion, None)?,
                    outputs: Self::outputs_of(e, checker)?,
                })
            }
            Some("effect/Effect#sleep") => self.effect_sleep(e, args, hint, checker, expansion),
            Some("effect/Effect#fail") => self.effect_fail(e, args, env, checker, expansion),
            Some("effect/Effect#all") => self.effect_all(e, args, env, checker, expansion),
            Some("effect/Effect#gen") => self.effect_gen(e, args, env, hint, checker, expansion),
            Some(
                "effect/Effect#map"
                | "effect/Effect#flatMap"
                | "effect/Effect#catchTag"
                | "effect/Effect#retry",
            ) => {
                let Some(&a0) = args.first() else {
                    return Err(CompileError::new(
                        "NORN-FLOW-UNSUPPORTED-SYNTAX",
                        "effect syntax has no lowering",
                        e,
                    ));
                };
                let inner_hint = if id == Some("effect/Effect#retry") {
                    hint
                } else {
                    None
                };
                let plan = self.effect(a0, env, inner_hint, checker, expansion)?;
                self.operator(
                    OperatorCall {
                        call: e,
                        plan,
                        env,
                        hint,
                        direct: true,
                    },
                    checker,
                    expansion,
                )
            }
            Some(_) | None => Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "effect syntax has no lowering",
                e,
            )),
        }
    }

    fn effect_perform(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        hint: Option<Option<&str>>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let (Some(&a0), Some(&a1), Some(&a2)) = (args.first(), args.get(1), args.get(2)) else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "Norn.perform requires step, handle, and input",
                e,
            ));
        };
        let bind = match hint {
            Some(None) => None,
            Some(Some(h)) => Some(self.fresh(h)),
            None => Some(self.fresh("result")),
        };
        let step_name = json_scalar_to_string(&self.scalar(a0, checker, expansion)?);
        let func = self.fn_handle(a1, checker, expansion)?;
        let input_expr = self.data(a2, env, checker, expansion, None)?;
        let node = self.annotate(
            Step::Perform(Box::new(PerformStep {
                step: step_name.clone(),
                func,
                input: input_expr,
                bind: bind.clone(),
                label: None,
                at: Some(self.at(e)),
                source_map: None,
            })),
            e,
            FlowSourceMapStepKind::Perform,
            Some(&step_name),
            None,
            None,
        );
        let value = bind.map_or(Expr::lit(Value::Null), |n| Expr::input(n, Vec::new()));
        Ok(Plan {
            steps: vec![node],
            value,
            outputs: Self::outputs_of(e, checker)?,
        })
    }

    fn effect_sleep(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        hint: Option<Option<&str>>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let Some(&a0) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "Effect.sleep requires a duration",
                e,
            ));
        };
        let step_name = match hint.flatten() {
            Some(h) => h.to_owned(),
            None => {
                self.ordinal = self.ordinal.saturating_add(1);
                format!("sleep_{}", self.ordinal)
            }
        };
        let millis = self.duration(a0, checker, expansion)?;
        let node = self.annotate(
            Step::Sleep {
                step: step_name,
                millis,
                label: None,
                at: Some(self.at(e)),
                source_map: None,
            },
            e,
            FlowSourceMapStepKind::Sleep,
            None,
            None,
            None,
        );
        Ok(Plan {
            steps: vec![node],
            value: Expr::lit(Value::Null),
            outputs: Self::outputs_of(e, checker)?,
        })
    }

    fn effect_fail(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let Some(&error_expr) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "expected tagged error construction",
                e,
            ));
        };
        if error_expr.kind() != Kind::NewExpression {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "expected tagged error construction",
                e,
            ));
        }
        let ne = error_expr.as_new_expression();
        let err_sym = self.symbol(checker, ne.expression())?;
        let err_decl = self.tagged_error(err_sym, error_expr, checker, expansion)?;
        let fields_expr = match list_nodes(ne.arguments()).first().copied() {
            Some(f) => self.data(f, env, checker, expansion, None)?,
            None => Expr::Record(Vec::new()),
        };
        let node = self.annotate(
            Step::Fail {
                error: Box::new(err_decl),
                fields: fields_expr,
                at: Some(self.at(e)),
                source_map: None,
            },
            e,
            FlowSourceMapStepKind::Fail,
            None,
            None,
            None,
        );
        Ok(Plan {
            steps: vec![node],
            value: Expr::lit(Value::Null),
            outputs: Vec::new(),
        })
    }

    fn effect_all(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let Some(&list) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "Effect.all needs a literal tuple",
                e,
            ));
        };
        if list.kind() != Kind::ArrayLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "Effect.all needs a literal tuple",
                e,
            ));
        }
        let mut steps = Vec::new();
        let mut values = Vec::new();
        for &item in list.as_array_literal_expression().elements.nodes() {
            let part = self.effect(item, env, None, checker, expansion)?;
            steps.extend(part.steps);
            values.push(part.value);
        }
        let node = self.annotate(
            Step::All {
                steps,
                at: Some(self.at(e)),
                source_map: None,
            },
            e,
            FlowSourceMapStepKind::All,
            None,
            None,
            None,
        );
        Ok(Plan {
            steps: vec![node],
            value: Expr::List(values),
            outputs: Self::outputs_of(e, checker)?,
        })
    }

    fn effect_gen(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        hint: Option<Option<&str>>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let Some(&fn_node) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "generator must be inline",
                e,
            ));
        };
        if fn_node.kind() != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "generator must be inline",
                e,
            ));
        }
        let Some(body) = fn_node.body() else {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "generator must be inline",
                e,
            ));
        };
        let body_steps = self.statements(
            body.as_block().statements.nodes(),
            env,
            false,
            false,
            checker,
            expansion,
        )?;
        let outs = Self::outputs_of(e, checker)?;
        self.compose(body_steps, outs, e, hint.flatten(), checker, expansion)
    }

    fn operator(
        &mut self,
        op: OperatorCall<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let id = self.call_id(checker, op.call);
        match id.as_deref() {
            Some("effect/Effect#map") => self.operator_map(op, true, checker, expansion),
            Some("effect/Effect#flatMap") => self.operator_map(op, false, checker, expansion),
            Some("effect/Effect#retry") => self.operator_retry(op, checker, expansion),
            Some("effect/Effect#catchTag") => self.operator_catch_tag(op, checker, expansion),
            Some(_) | None => Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "unsupported pipe operator",
                op.call,
            )),
        }
    }

    fn operator_map(
        &mut self,
        op: OperatorCall<'_>,
        pure: bool,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let args = op.call.as_call_expression().arguments().nodes();
        let offset = usize::from(op.direct);
        let Some(&cb_arg) = args.get(offset) else {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "continuation must be inline syntax",
                op.call,
            ));
        };
        let fn_node = Self::callback_node(cb_arg)?;
        let next_env = self.callback_env(fn_node, op.plan.value, op.env, checker)?;
        let next = self.callback_plan(
            fn_node,
            &next_env,
            pure,
            op.hint.flatten(),
            checker,
            expansion,
        )?;
        let mut steps = op.plan.steps;
        steps.extend(next.steps);
        Ok(Plan {
            steps,
            value: next.value,
            outputs: next.outputs,
        })
    }

    fn operator_retry(
        &mut self,
        op: OperatorCall<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let args = op.call.as_call_expression().arguments().nodes();
        let offset = usize::from(op.direct);
        let mut iter = op.plan.steps.into_iter();
        let first = iter.next();
        let second = iter.next();
        let (Some(Step::Perform(perform)), None) = (first, second) else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "retry wraps exactly one Norn.perform",
                op.call,
            ));
        };
        let Some(&pol_arg) = args.get(offset) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected literal Schedule policy",
                op.call,
            ));
        };
        let policy = self.policy(pol_arg, checker, expansion)?;
        let sem_id = perform.step.clone();
        let node = self.annotate(
            Step::Retry {
                perform,
                policy,
                at: Some(self.at(op.call)),
                source_map: None,
            },
            op.call,
            FlowSourceMapStepKind::Retry,
            Some(&sem_id),
            None,
            None,
        );
        Ok(Plan {
            steps: vec![node],
            value: op.plan.value,
            outputs: op.plan.outputs,
        })
    }

    fn operator_catch_tag(
        &mut self,
        op: OperatorCall<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Plan, CompileError> {
        let args = op.call.as_call_expression().arguments().nodes();
        let offset = usize::from(op.direct);
        let (Some(&tag_arg), Some(&cb_arg)) =
            (args.get(offset), args.get(offset.saturating_add(1)))
        else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "catchTag requires tag and handler callback",
                op.call,
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
            Some(name) => self.callback_env(
                fn_node,
                Expr::input(name.clone(), Vec::new()),
                op.env,
                checker,
            )?,
            None => op.env.clone(),
        };
        let handler = self.callback_plan(fn_node, &next_env, false, None, checker, expansion)?;
        let mut body = op.plan.steps;
        body.push(self.returned(op.plan.value, op.call, false));
        let mut recovery = handler.steps;
        recovery.push(self.returned(handler.value, fn_node, false));
        let tag = json_scalar_to_string(&self.scalar(tag_arg, checker, expansion)?);
        let caught = self.annotate(
            Step::CatchTag {
                body,
                tag,
                as_name,
                handler: recovery,
                at: Some(self.at(op.call)),
                source_map: None,
            },
            op.call,
            FlowSourceMapStepKind::CatchTag,
            None,
            None,
            None,
        );
        let mut combined_outputs = op.plan.outputs;
        combined_outputs.extend(handler.outputs);
        self.compose(
            vec![caught],
            combined_outputs,
            op.call,
            op.hint.flatten(),
            checker,
            expansion,
        )
    }
}
