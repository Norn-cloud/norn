//! Flow metadata, tagged-error declarations, Function handles, and Schedule
//! retry policies split out of [`super::flow_syntax`].

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use tsrs_ast::{Kind, Node, Symbol};
use tsrs_checker::{Checker, Type, TypeFlags};
use tsrs_core::P;

use crate::checker::resolve::list_nodes;

use super::expr::Expr;
use super::flow_source_map::{
    FLOW_MAX_RECURS, FlowMeta, FnRef, RetryDelay, RetryPolicy, TaggedErrorDecl,
};
use super::flow_syntax::SyntaxCompiler;
use super::types::CompileError;
use super::value_schema::{ExpansionBudget, ObjectSchema, ValueSchema};

fn expr_string_literal(expr: &Expr) -> Option<String> {
    if let Expr::Literal(Value::String(s)) = expr {
        Some(s.clone())
    } else {
        None
    }
}

impl SyntaxCompiler<'_> {
    pub(super) fn flow_metadata(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<FlowMeta, CompileError> {
        let fields = self.record_literal(expr, checker, expansion)?;
        let id = fields
            .get("id")
            .and_then(expr_string_literal)
            .ok_or_else(|| {
                CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "id and version must be literal strings",
                    expr,
                )
            })?;
        let version = fields
            .get("version")
            .and_then(expr_string_literal)
            .ok_or_else(|| {
                CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "id and version must be literal strings",
                    expr,
                )
            })?;
        let title = fields.get("title").and_then(expr_string_literal);
        Ok(FlowMeta { id, version, title })
    }

    pub(super) fn tagged_error(
        &mut self,
        symbol: P<Symbol>,
        node: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<TaggedErrorDecl, CompileError> {
        if let Some(known) = self.errors.get(&symbol) {
            return Ok(known.clone());
        }
        let decl = symbol
            .declarations()
            .iter()
            .copied()
            .find(|d| d.kind() == Kind::ClassDeclaration);
        let heritage = decl
            .and_then(|d| {
                list_nodes(d.as_class_declaration().heritage_clauses())
                    .first()
                    .copied()
            })
            .and_then(|clause| clause.as_heritage_clause().types().nodes().first().copied());
        let Some(heritage) = heritage else {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "error must be a registered Data.TaggedError class",
                node,
            ));
        };
        let hwta = heritage.as_expression_with_type_arguments();
        if hwta.expression().kind() != Kind::CallExpression
            || self.call_id(checker, hwta.expression()).as_deref()
                != Some("effect/Data#TaggedError")
        {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "error must be a registered Data.TaggedError class",
                node,
            ));
        }
        let call_args = hwta.expression().as_call_expression().arguments().nodes();
        let Some(&tag_arg) = call_args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "tagged errors need a literal tag and record fields",
                node,
            ));
        };
        let Value::String(tag) = self.scalar(tag_arg, checker, expansion)? else {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "tagged errors need a literal tag and record fields",
                node,
            ));
        };
        let schema = match list_nodes(hwta.type_arguments()).first().copied() {
            Some(fields_node) => {
                let ty = checker.get_type_from_type_node_exported(fields_node);
                self.schema_budget.value_schema(
                    checker,
                    expansion,
                    ty,
                    fields_node,
                    &HashSet::new(),
                )?
            }
            None => ValueSchema::object(ObjectSchema::record(BTreeMap::new(), &[])),
        };
        let ValueSchema::Object(fields) = schema else {
            return Err(CompileError::new(
                "NORN-FLOW-TAG",
                "tagged errors need a literal tag and record fields",
                node,
            ));
        };
        let result = TaggedErrorDecl {
            tag,
            fields: *fields,
            at: decl.map(|d| self.at(d)),
        };
        self.errors.insert(symbol, result.clone());
        Ok(result)
    }

    pub(super) fn fn_handle(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<FnRef, CompileError> {
        let sym = self.symbol(checker, expr)?;
        if let Some(known) = self.handles.get(&sym) {
            return Ok(known.clone());
        }
        let init = Self::fn_handle_init(sym, expr)?;
        let type_args = list_nodes(init.as_call_expression().type_arguments());
        let (Some(&in_node), Some(&out_node)) = (type_args.first(), type_args.get(1)) else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "Norn.fn needs explicit structural I/O types",
                expr,
            ));
        };
        if self.call_id(checker, init).as_deref() != Some("@norn/authoring#Norn.fn") {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "Norn.fn needs explicit structural I/O types",
                expr,
            ));
        }
        let in_ty = checker.get_type_from_type_node_exported(in_node);
        let in_schema =
            self.schema_budget
                .value_schema(checker, expansion, in_ty, in_node, &HashSet::new())?;
        let ValueSchema::Object(input) = in_schema else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "Function input must be a closed record",
                in_node,
            ));
        };
        let out_ty = checker.get_type_from_type_node_exported(out_node);
        let output = self.schema_budget.value_schema(
            checker,
            expansion,
            out_ty,
            out_node,
            &HashSet::new(),
        )?;
        let errors = match type_args.get(2) {
            Some(&err_node) => self.fn_handle_errors(err_node, checker, expansion)?,
            None => Vec::new(),
        };
        let Some(&meta_arg) = init.as_call_expression().arguments().nodes().first() else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "Norn.fn requires metadata { id, version }",
                init,
            ));
        };
        let meta = self.flow_metadata(meta_arg, checker, expansion)?;
        let result = FnRef {
            id: meta.id,
            version: meta.version,
            input: *input,
            output,
            errors,
            at: Some(self.at(init)),
        };
        self.handles.insert(sym, result.clone());
        Ok(result)
    }

    fn fn_handle_init(sym: P<Symbol>, expr: P<Node>) -> Result<P<Node>, CompileError> {
        let Some(decl) = sym.value_declaration() else {
            return Err(CompileError::new(
                "NORN-FLOW-SYMBOL",
                "expected a declared Function handle",
                expr,
            ));
        };
        if decl.kind() != Kind::VariableDeclaration {
            return Err(CompileError::new(
                "NORN-FLOW-SYMBOL",
                "expected a declared Function handle",
                expr,
            ));
        }
        let Some(init) = decl.as_variable_declaration().initializer() else {
            return Err(CompileError::new(
                "NORN-FLOW-SYMBOL",
                "expected a declared Function handle",
                expr,
            ));
        };
        if init.kind() != Kind::CallExpression {
            return Err(CompileError::new(
                "NORN-FLOW-SYMBOL",
                "expected a declared Function handle",
                expr,
            ));
        }
        Ok(init)
    }

    fn fn_handle_errors(
        &mut self,
        err_node: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<TaggedErrorDecl>, CompileError> {
        let mut source_order: HashMap<P<Symbol>, usize> = HashMap::new();
        let mut visited_aliases: HashSet<P<Symbol>> = HashSet::new();
        self.collect_type_symbol_order(
            err_node,
            checker,
            &mut source_order,
            &mut visited_aliases,
            0,
        );

        let err_ty = checker.get_type_from_type_node_exported(err_node);
        let members: Vec<P<Type>> = if err_ty.is_union() {
            err_ty.types().to_vec()
        } else {
            vec![err_ty]
        };
        let mut collected: Vec<(usize, usize, TaggedErrorDecl)> = Vec::new();
        let mut seen_syms: HashSet<P<Symbol>> = HashSet::new();
        for (idx, member) in members.into_iter().enumerate() {
            if member.flags().intersects(TypeFlags::Never) {
                continue;
            }
            let Some(err_sym) = member.symbol() else {
                return Err(CompileError::new(
                    "NORN-FLOW-TAG",
                    "Function failures must be tagged error declarations",
                    err_node,
                ));
            };
            if !seen_syms.insert(err_sym) {
                continue;
            }
            let decl = self.tagged_error(err_sym, err_node, checker, expansion)?;
            let ord = source_order.get(&err_sym).copied().unwrap_or(usize::MAX);
            collected.push((ord, idx, decl));
        }
        collected.sort_by(|(ord_a, idx_a, decl_a), (ord_b, idx_b, decl_b)| {
            ord_a
                .cmp(ord_b)
                .then_with(|| {
                    let loc_a = decl_a
                        .at
                        .as_ref()
                        .map(|a| (a.file.as_str(), a.line, a.column.unwrap_or(0)));
                    let loc_b = decl_b
                        .at
                        .as_ref()
                        .map(|b| (b.file.as_str(), b.line, b.column.unwrap_or(0)));
                    loc_a.cmp(&loc_b)
                })
                .then_with(|| idx_a.cmp(idx_b))
        });
        Ok(collected.into_iter().map(|(_, _, decl)| decl).collect())
    }

    fn collect_type_symbol_order(
        &self,
        node: P<Node>,
        checker: &mut Checker,
        order: &mut HashMap<P<Symbol>, usize>,
        visited_aliases: &mut HashSet<P<Symbol>>,
        depth: usize,
    ) {
        if depth > self.limits.max_depth {
            return;
        }
        if node.kind() == Kind::Identifier {
            if let Ok(sym) = self.symbol(checker, node) {
                let next_ord = order.len();
                order.entry(sym).or_insert(next_ord);
                if visited_aliases.insert(sym)
                    && let Some(decl) = sym
                        .declarations()
                        .iter()
                        .copied()
                        .find(|d| d.kind() == Kind::TypeAliasDeclaration)
                {
                    self.collect_type_symbol_order(
                        decl,
                        checker,
                        order,
                        visited_aliases,
                        depth.saturating_add(1),
                    );
                }
            }
            return;
        }
        let mut children = Vec::new();
        node.for_each_child(&mut |child| {
            children.push(child);
            false
        });
        let next_depth = depth.saturating_add(1);
        for child in children {
            self.collect_type_symbol_order(child, checker, order, visited_aliases, next_depth);
        }
    }

    pub(super) fn duration(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<f64, CompileError> {
        let value = self.scalar(expr, checker, expansion)?;
        match value {
            Value::Number(n) => n.as_f64().ok_or_else(|| {
                CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "duration must be numeric milliseconds or a finite duration string",
                    expr,
                )
            }),
            Value::String(s) => parse_duration_string(&s).ok_or_else(|| {
                CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "duration must be numeric milliseconds or a finite duration string",
                    expr,
                )
            }),
            Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
                Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "duration must be numeric milliseconds or a finite duration string",
                    expr,
                ))
            }
        }
    }

    pub(super) fn policy(
        &mut self,
        expr: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<RetryPolicy, CompileError> {
        let call = Self::unwrap(expr);
        if call.kind() != Kind::CallExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected literal Schedule policy",
                expr,
            ));
        }
        let args = call.as_call_expression().arguments().nodes();
        match self.call_id(checker, call).as_deref() {
            Some("effect/Schedule#recurs") => self.policy_recurs(call, args, checker, expansion),
            Some("effect/Schedule#spaced") => {
                let Some(&arg0) = args.first() else {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "Schedule.spaced requires a duration",
                        call,
                    ));
                };
                Ok(RetryPolicy {
                    recurs: None,
                    delay: Some(RetryDelay::Spaced {
                        millis: self.duration(arg0, checker, expansion)?,
                    }),
                })
            }
            Some("effect/Schedule#exponential") => {
                self.policy_exponential(call, args, checker, expansion)
            }
            Some("effect/Schedule#max") => self.policy_max(expr, args, checker, expansion),
            Some(_) | None => Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "unsupported Schedule policy",
                expr,
            )),
        }
    }

    fn policy_recurs(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<RetryPolicy, CompileError> {
        let Some(&arg0) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "each finite schedule count must satisfy the retry bound",
                call,
            ));
        };
        let val = self.scalar(arg0, checker, expansion)?;
        let recurs_f = val.as_f64().unwrap_or(f64::NAN);
        let recurs_u32 = val
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| (1..=FLOW_MAX_RECURS).contains(&n));
        if !recurs_f.is_finite() || recurs_f.fract().abs().to_bits() != 0 {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "each finite schedule count must satisfy the retry bound",
                call,
            ));
        }
        let Some(recurs) = recurs_u32 else {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "each finite schedule count must satisfy the retry bound",
                call,
            ));
        };
        Ok(RetryPolicy {
            recurs: Some(recurs),
            delay: None,
        })
    }

    fn policy_exponential(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<RetryPolicy, CompileError> {
        let Some(&arg0) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "Schedule.exponential requires a base duration",
                call,
            ));
        };
        let base_millis = self.duration(arg0, checker, expansion)?;
        let factor = match args.get(1) {
            Some(&arg1) => self
                .scalar(arg1, checker, expansion)?
                .as_f64()
                .unwrap_or(f64::NAN),
            None => 2.0,
        };
        Ok(RetryPolicy {
            recurs: None,
            delay: Some(RetryDelay::Exponential {
                base_millis,
                factor,
            }),
        })
    }

    fn policy_max(
        &mut self,
        expr: P<Node>,
        args: &[P<Node>],
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<RetryPolicy, CompileError> {
        let Some(&list) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "unsupported Schedule policy",
                expr,
            ));
        };
        if list.kind() != Kind::ArrayLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "unsupported Schedule policy",
                expr,
            ));
        }
        let mut policy = RetryPolicy::default();
        for &item in list.as_array_literal_expression().elements.nodes() {
            let part = self.policy(item, checker, expansion)?;
            let recurs = match (policy.recurs, part.recurs) {
                (None, r) => r,
                (l, None) => l,
                (Some(l), Some(r)) => Some(l.min(r)),
            };
            let delay = match (policy.delay, part.delay) {
                (None, d) => d,
                (d, None) => d,
                (Some(l), Some(r)) => Some(RetryDelay::Max(vec![l, r])),
            };
            policy = RetryPolicy { recurs, delay };
        }
        Ok(policy)
    }
}

fn parse_duration_string(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(trimmed.len());
    let num_str = trimmed.get(..split)?;
    let rest = trimmed.get(split..)?.trim();
    if num_str.is_empty() || rest.is_empty() {
        return None;
    }
    let amount: f64 = num_str.parse().ok()?;
    if !amount.is_finite() {
        return None;
    }
    let scale = match rest {
        "ms" | "milli" | "millis" | "millisecond" | "milliseconds" => 1.0,
        "s" | "second" | "seconds" => 1_000.0,
        "m" | "minute" | "minutes" => 60_000.0,
        "h" | "hour" | "hours" => 3_600_000.0,
        "d" | "day" | "days" => 86_400_000.0,
        _ => return None,
    };
    Some(amount * scale)
}
