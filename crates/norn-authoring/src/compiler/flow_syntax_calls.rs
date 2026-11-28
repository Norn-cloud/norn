//! `Norn.*` intrinsic calls, `unsafeZen` escapes, and array/string method-call
//! lowering split out of [`super::flow_syntax_expr`].

use std::collections::HashSet;

use serde_json::Value;
use tsrs_ast::{Kind, Node};
use tsrs_checker::{Checker, is_tuple_type_exported};
use tsrs_core::P;

use crate::checker::resolve::list_nodes;

use super::expr::{BinaryOp, BinderId, CallFn, ClosureFn, Expr};
use super::flow_syntax::{DecisionHooks, Env, SyntaxCompiler};
use super::types::{CompileError, diagnostic_for_node};
use super::value_schema::ExpansionBudget;

struct CallSpec<'a> {
    id: &'a str,
    node: P<Node>,
    args: &'a [P<Node>],
    env: &'a Env,
}

struct ClosureCallSpec<'a> {
    call: P<Node>,
    recv_node: P<Node>,
    receiver: Expr,
    c_fn: ClosureFn,
    name: &'a str,
    args: &'a [P<Node>],
    env: &'a Env,
}

impl SyntaxCompiler<'_> {
    pub(super) fn data_call(
        &mut self,
        e: P<Node>,
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Expr, CompileError> {
        let ce = e.as_call_expression();
        let args = ce.arguments().nodes();
        if let Some(id) = self.call_id(checker, e) {
            let spec = CallSpec {
                id: &id,
                node: e,
                args,
                env,
            };
            if let Some(res) =
                self.data_norn_call(&spec, checker, expansion, hooks.as_deref_mut())?
            {
                return Ok(res);
            }
        }
        if ce.expression().kind() == Kind::PropertyAccessExpression {
            return self.data_method_call(e, args, env, checker, expansion, hooks);
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "expression has no first-order lowering",
            e,
        ))
    }

    fn data_norn_call(
        &mut self,
        spec: &CallSpec<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Option<Expr>, CompileError> {
        if (spec.id == "@norn/authoring#Norn.evaluate"
            || spec.id == "@norn/authoring#Norn.orderedFold")
            && let Some(h) = hooks.as_deref_mut()
        {
            let Some(&a1) = spec.args.get(1) else {
                return Err(CompileError::new(
                    "NORN-FLOW-EXPRESSION",
                    "sub-decision call requires input argument",
                    spec.node,
                ));
            };
            let input_expr = self.data(a1, spec.env, checker, expansion, Some(&mut *h))?;
            return h
                .on_sub_decision(spec.node, input_expr, checker, expansion)
                .map(Some);
        }
        if spec.id == "@norn/authoring#Norn.unsafeZen" {
            return self
                .data_unsafe_zen(spec.node, spec.args, spec.env, checker, expansion, hooks)
                .map(Some);
        }
        match spec.id {
            "@norn/authoring#Norn.byteLength" => {
                let Some(&a0) = spec.args.first() else {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.byteLength takes one argument",
                        spec.node,
                    ));
                };
                Ok(Some(Expr::call(
                    CallFn::Len,
                    vec![self.data(a0, spec.env, checker, expansion, hooks)?],
                )))
            }
            "@norn/authoring#Norn.take" => {
                let (Some(&a0), Some(&a1)) = (spec.args.first(), spec.args.get(1)) else {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.take takes a string and an integer literal cap",
                        spec.node,
                    ));
                };
                if spec.args.len() != 2 {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.take takes a string and an integer literal cap",
                        spec.node,
                    ));
                }
                let x0 = self.data(a0, spec.env, checker, expansion, hooks.as_deref_mut())?;
                let x1 = self.data(a1, spec.env, checker, expansion, hooks)?;
                Ok(Some(Expr::call(CallFn::Take, vec![x0, x1])))
            }
            "@norn/authoring#Norn.floor" => {
                if spec.args.len() != 1 {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.floor takes one number",
                        spec.node,
                    ));
                }
                let Some(&a0) = spec.args.first() else {
                    return Ok(None);
                };
                Ok(Some(Expr::call(
                    CallFn::Floor,
                    vec![self.data(a0, spec.env, checker, expansion, hooks)?],
                )))
            }
            "@norn/authoring#Norn.sum" => {
                let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
                if lowered.len() != 1 {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.sum takes one list",
                        spec.node,
                    ));
                }
                Ok(Some(Expr::call(CallFn::Sum, lowered)))
            }
            _ => self.data_temporal_call(spec, checker, expansion, hooks),
        }
    }

    fn data_temporal_call(
        &mut self,
        spec: &CallSpec<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Option<Expr>, CompileError> {
        match spec.id {
            "@norn/authoring#Norn.parseDate" | "@norn/authoring#Norn.parseTimestamp" => {
                if spec.args.len() != 1 {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "temporal conversion needs exactly one argument",
                        spec.node,
                    ));
                }
                let Some(&a0) = spec.args.first() else {
                    return Ok(None);
                };
                let func = if spec.id.ends_with("parseDate") {
                    CallFn::Date
                } else {
                    CallFn::Timestamp
                };
                Ok(Some(Expr::call(
                    func,
                    vec![self.data(a0, spec.env, checker, expansion, hooks)?],
                )))
            }
            "@norn/authoring#Norn.dateBefore" => {
                let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
                Ok(Some(Expr::call(CallFn::DateBefore, lowered)))
            }
            "@norn/authoring#Norn.timestampBefore" | "@norn/authoring#Norn.timestampAtOrBefore" => {
                let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
                let mut iter = lowered.into_iter();
                let (Some(a), Some(b), None) = (iter.next(), iter.next(), iter.next()) else {
                    let short = spec.id.split_once('#').map_or(spec.id, |(_, r)| r);
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        format!("{short} takes two arguments"),
                        spec.node,
                    ));
                };
                let func = if spec.id.ends_with(".timestampBefore") {
                    CallFn::TimestampBefore
                } else {
                    CallFn::TimestampAtOrBefore
                };
                Ok(Some(Expr::call(
                    func,
                    vec![a.into_timestamp(), b.into_timestamp()],
                )))
            }
            "@norn/authoring#Norn.daysBetween"
            | "@norn/authoring#Norn.addDays"
            | "@norn/authoring#Norn.secondsBetween"
            | "@norn/authoring#Norn.addSeconds" => {
                self.data_temporal_binary(spec, checker, expansion, hooks)
            }
            "@norn/authoring#Norn.intervalsOverlap"
            | "@norn/authoring#Norn.intervalGapSeconds"
            | "@norn/authoring#Norn.intervalIntersectionSeconds" => {
                self.data_interval_call(spec, checker, expansion, hooks)
            }
            "@norn/authoring#Norn.resolveLocalTimestamp" => {
                let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
                if lowered.len() != 3 {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.resolveLocalTimestamp takes three arguments",
                        spec.node,
                    ));
                }
                Ok(Some(Expr::call(CallFn::ResolveLocalTimestamp, lowered)))
            }
            _ => Ok(None),
        }
    }

    fn data_temporal_binary(
        &mut self,
        spec: &CallSpec<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Option<Expr>, CompileError> {
        let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
        let mut iter = lowered.into_iter();
        let (Some(a), Some(b), None) = (iter.next(), iter.next(), iter.next()) else {
            let short = spec.id.split_once('#').map_or(spec.id, |(_, r)| r);
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                format!("{short} takes two arguments"),
                spec.node,
            ));
        };
        let expr = match spec.id {
            "@norn/authoring#Norn.daysBetween" => {
                Expr::call(CallFn::DaysBetween, vec![a.into_date(), b.into_date()])
            }
            "@norn/authoring#Norn.addDays" => Expr::call(CallFn::AddDays, vec![a.into_date(), b]),
            "@norn/authoring#Norn.secondsBetween" => Expr::call(
                CallFn::SecondsBetween,
                vec![a.into_timestamp(), b.into_timestamp()],
            ),
            _ => Expr::call(CallFn::AddSeconds, vec![a.into_timestamp(), b]),
        };
        Ok(Some(expr))
    }

    fn data_interval_call(
        &mut self,
        spec: &CallSpec<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Option<Expr>, CompileError> {
        let lowered = self.lower_args(spec.args, spec.env, checker, expansion, hooks)?;
        let mut iter = lowered.into_iter();
        let (Some(a), Some(b), Some(c), Some(d), None) = (
            iter.next(),
            iter.next(),
            iter.next(),
            iter.next(),
            iter.next(),
        ) else {
            let short = spec.id.split_once('#').map_or(spec.id, |(_, r)| r);
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                format!("{short} takes four arguments"),
                spec.node,
            ));
        };
        let func = match spec.id {
            "@norn/authoring#Norn.intervalsOverlap" => CallFn::IntervalsOverlap,
            "@norn/authoring#Norn.intervalGapSeconds" => CallFn::IntervalGapSeconds,
            _ => CallFn::IntervalIntersectionSeconds,
        };
        Ok(Some(Expr::call(
            func,
            vec![
                a.into_timestamp(),
                b.into_timestamp(),
                c.into_timestamp(),
                d.into_timestamp(),
            ],
        )))
    }

    fn data_unsafe_zen(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Expr, CompileError> {
        let type_args = list_nodes(e.as_call_expression().type_arguments());
        let (Some(&a0), Some(&t0)) = (args.first(), type_args.first()) else {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "unsafeZen<T> requires its declared result type and literal ZEN text",
                e,
            ));
        };
        let Value::String(text) = self.scalar(a0, checker, expansion)? else {
            return Err(CompileError::new(
                "NORN-FLOW-TYPE",
                "unsafeZen<T> requires its declared result type and literal ZEN text",
                e,
            ));
        };
        if self.forbid_unsafe_zen {
            return Err(CompileError::new(
                "NORN-AUTHORING-UNSAFE-ZEN",
                "unsafeZen is forbidden (--forbid-unsafe-zen); replace it with typed TypeScript authoring",
                e,
            ));
        }
        if env.values().any(|b| matches!(b, Expr::Bound { .. })) {
            return Err(CompileError::new(
                "NORN-AUTHORING-UNSAFE-ZEN",
                "a raw decision escape cannot refer to lexical closure bindings",
                e,
            ));
        }
        let Some(h) = hooks.filter(|h| h.supports_escape()) else {
            return Err(CompileError::new(
                "NORN-AUTHORING-UNSAFE-ZEN",
                "raw ZEN cannot be embedded in typed expressions",
                e,
            ));
        };
        self.warnings.push(diagnostic_for_node(
            e,
            "NORN-AUTHORING-UNSAFE-ZEN",
            "unsafeZen bypasses typed authoring; replace it with plain TypeScript",
        ));
        let ty = checker.get_type_from_type_node_exported(t0);
        let schema = self
            .schema_budget
            .value_schema(checker, expansion, ty, e, &HashSet::new())?;
        h.on_escape(e, &text, schema)
    }

    fn data_method_call(
        &mut self,
        e: P<Node>,
        args: &[P<Node>],
        env: &Env,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        mut hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Expr, CompileError> {
        let pa = e
            .as_call_expression()
            .expression()
            .as_property_access_expression();
        let recv_node = pa.expression();
        let receiver = self.data(recv_node, env, checker, expansion, hooks.as_deref_mut())?;
        let name = pa.name().text();
        let closure_fn = match name {
            "some" => Some(ClosureFn::Some),
            "every" => Some(ClosureFn::All),
            "filter" => Some(ClosureFn::Filter),
            "map" => Some(ClosureFn::Map),
            _ => None,
        };
        if let Some(c_fn) = closure_fn {
            return self.data_closure_call(
                ClosureCallSpec {
                    call: e,
                    recv_node,
                    receiver,
                    c_fn,
                    name,
                    args,
                    env,
                },
                checker,
                expansion,
                hooks,
            );
        }
        let lowered_args = self.lower_args(args, env, checker, expansion, hooks)?;
        if name == "concat" {
            let mut all = Vec::with_capacity(lowered_args.len().saturating_add(1));
            all.push(receiver);
            all.extend(lowered_args);
            return Ok(Expr::call(CallFn::Concat, all));
        }
        if name == "includes" {
            let ty = checker.get_type_at_location(recv_node);
            if checker.is_array_type_exported(ty) || is_tuple_type_exported(ty) {
                let needle = lowered_args.into_iter().next().ok_or_else(|| {
                    CompileError::new("NORN-FLOW-EXPRESSION", "includes requires an argument", e)
                })?;
                return Ok(Expr::binary(BinaryOp::In, needle, receiver));
            }
            let mut all = Vec::with_capacity(lowered_args.len().saturating_add(1));
            all.push(receiver);
            all.extend(lowered_args);
            return Ok(Expr::call(CallFn::Contains, all));
        }
        let str_fn = match name {
            "toLowerCase" => Some(CallFn::Lower),
            "toUpperCase" => Some(CallFn::Upper),
            "trim" => Some(CallFn::Trim),
            "startsWith" => Some(CallFn::StartsWith),
            "endsWith" => Some(CallFn::EndsWith),
            _ => None,
        };
        if let Some(call_fn) = str_fn {
            let mut all = Vec::with_capacity(lowered_args.len().saturating_add(1));
            all.push(receiver);
            all.extend(lowered_args);
            return Ok(Expr::call(call_fn, all));
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "expression has no first-order lowering",
            e,
        ))
    }

    fn data_closure_call(
        &mut self,
        spec: ClosureCallSpec<'_>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut (dyn DecisionHooks + '_)>,
    ) -> Result<Expr, CompileError> {
        let ty = checker.get_type_at_location(spec.recv_node);
        if !(checker.is_array_type_exported(ty) || is_tuple_type_exported(ty)) {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                format!(".{} iterates an array", spec.name),
                spec.call,
            ));
        }
        let Some(&cb) = spec.args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                format!("pass .{} one inline arrow callback", spec.name),
                spec.call,
            ));
        };
        if spec.args.len() != 1 || cb.kind() != Kind::ArrowFunction {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                format!("pass .{} one inline arrow callback", spec.name),
                cb,
            ));
        }
        let params = cb.parameters();
        let Some(&param) = params.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!(
                    "a .{} callback takes one plain parameter and an expression body",
                    spec.name
                ),
                cb,
            ));
        };
        let pd = param.as_parameter_declaration();
        let Some(body) = cb.body() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!(
                    "a .{} callback takes one plain parameter and an expression body",
                    spec.name
                ),
                cb,
            ));
        };
        if params.len() != 1
            || pd.name().kind() != Kind::Identifier
            || param.initializer().is_some()
            || pd.dot_dot_dot_token().is_some()
            || body.kind() == Kind::Block
        {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!(
                    "a .{} callback takes one plain parameter and an expression body",
                    spec.name
                ),
                cb,
            ));
        }
        let sym = self.symbol(checker, pd.name())?;
        let binder = BinderId::fresh();
        let mut inner_env = spec.env.clone();
        inner_env.insert(
            sym,
            Expr::Bound {
                binder,
                path: Vec::new(),
            },
        );
        let lowered_body = self.data(body, &inner_env, checker, expansion, hooks)?;
        Ok(Expr::Closure {
            func: spec.c_fn,
            list: Box::new(spec.receiver),
            binder,
            body: Box::new(lowered_body),
        })
    }
}
