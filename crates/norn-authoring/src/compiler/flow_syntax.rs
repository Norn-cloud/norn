//! Syntax-only flow and pure-expression compiler (`SyntaxCompiler` in `compiler/index.ts`).
//!
//! Compiles validated `Norn.flow` modules and decision/expression callbacks into
//! closure-free [`FlowDef`] and [`Expr`] trees directly from `tsrs` AST and type
//! information, without ever importing or executing authored code.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::Value;
use tsrs_ast::{Kind, Node, SourceFile, Symbol, SymbolFlags, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_compiler::Program;
use tsrs_core::{LanguageVariant, P, ScriptTarget};
use tsrs_scanner::Scanner;

use crate::checker::resolve::{node_text, resolve_symbol};
use crate::{BoundedVfs, ProfileDiagnostic};

use super::canonical::{is_snake_ident, sha256_hex};
use super::expr::Expr;
use super::flow_source_map::{
    FLOW_MAX_ROWS, FlowDef, FlowSourceMapAnnotation, FlowSourceMapStepKind, FnRef, SourceLocation,
    Step, TaggedErrorDecl,
};
use super::flow_syntax_steps::branch_statements;
use super::types::{CompileError, CompilerLimits, SchemaBudget, check_closure, source_location_of};
use super::value_schema::{BudgetExceeded, ExpansionBudget, ValueSchema};

/// Lexical symbol → compiled expression environment.
pub(crate) type Env = HashMap<P<Symbol>, Expr>;

/// One decision row produced by [`SyntaxCompiler::decision_rows`].
#[derive(Clone, Debug)]
pub(crate) struct DecisionRow {
    pub(crate) condition: Option<Expr>,
    pub(crate) value: Expr,
    pub(crate) at: SourceLocation,
}

/// Optional hooks supplied when compiling a `Norn.decision` callback.
pub(crate) trait DecisionHooks {
    fn on_sub_decision(
        &mut self,
        call: P<Node>,
        input: Expr,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<Expr, CompileError>;

    fn on_escape(
        &mut self,
        node: P<Node>,
        text: &str,
        schema: ValueSchema,
    ) -> Result<Expr, CompileError>;

    fn supports_escape(&self) -> bool {
        true
    }
}

/// Intermediate composition plan for an Effect expression.
pub(crate) struct Plan {
    pub(crate) steps: Vec<Step>,
    pub(crate) value: Expr,
    pub(crate) outputs: Vec<P<tsrs_checker::Type>>,
}

/// Configuration for constructing a [`SyntaxCompiler`].
pub(crate) struct SyntaxConfig<'a> {
    pub(crate) intrinsics_module: &'a str,
    pub(crate) effect_dist_prefix: Option<&'a str>,
    pub(crate) vfs: &'a BoundedVfs,
    pub(crate) program: &'static Program,
    pub(crate) source: P<SourceFile>,
    pub(crate) started: Instant,
    pub(crate) limits: CompilerLimits,
    pub(crate) source_root: &'a str,
    pub(crate) tool_root: &'a str,
    pub(crate) forbid_unsafe_zen: bool,
}

struct DecisionWalk<'a> {
    fn_node: P<Node>,
    rows: &'a mut Vec<DecisionRow>,
    steps: usize,
    checker: &'a mut Checker,
    expansion: &'a mut ExpansionBudget,
    hooks: Option<&'a mut dyn DecisionHooks>,
}

/// Syntax-only lowering engine for `Norn.flow` and decision/expression callbacks.
pub(crate) struct SyntaxCompiler<'a> {
    pub(crate) intrinsics_module: &'a str,
    pub(crate) effect_dist_prefix: Option<&'a str>,
    pub(crate) vfs: &'a BoundedVfs,
    pub(crate) program: &'static Program,
    pub(crate) source: P<SourceFile>,
    pub(crate) started: Instant,
    pub(crate) limits: CompilerLimits,
    pub(crate) source_root: &'a str,
    pub(crate) tool_root: &'a str,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) warnings: Vec<ProfileDiagnostic>,
    /// Tracks originating AST node for each lowered expression.
    pub(crate) expression_sources: Vec<(Expr, P<Node>)>,
    pub(super) names: HashSet<String>,
    pub(crate) constants: HashMap<P<Symbol>, Expr>,
    pub(crate) resolving: HashSet<P<Symbol>>,
    pub(super) handles: HashMap<P<Symbol>, FnRef>,
    pub(super) errors: HashMap<P<Symbol>, TaggedErrorDecl>,
    pub(super) source_map_occurrences: HashMap<String, usize>,
    pub(super) ordinal: usize,
    pub(crate) depth: usize,
    pub(crate) schema_budget: SchemaBudget,
}

impl<'a> SyntaxCompiler<'a> {
    pub(crate) fn new(config: SyntaxConfig<'a>) -> Self {
        Self {
            intrinsics_module: config.intrinsics_module,
            effect_dist_prefix: config.effect_dist_prefix,
            vfs: config.vfs,
            program: config.program,
            source: config.source,
            started: config.started,
            limits: config.limits,
            source_root: config.source_root,
            tool_root: config.tool_root,
            forbid_unsafe_zen: config.forbid_unsafe_zen,
            warnings: Vec::new(),
            expression_sources: Vec::new(),
            names: HashSet::new(),
            constants: HashMap::new(),
            resolving: HashSet::new(),
            handles: HashMap::new(),
            errors: HashMap::new(),
            source_map_occurrences: HashMap::new(),
            ordinal: 0,
            depth: 0,
            schema_budget: SchemaBudget::new(),
        }
    }

    pub(crate) fn check_budget(&self, node: P<Node>) -> Result<(), CompileError> {
        if self.started.elapsed() >= Duration::from_millis(self.limits.max_millis)
            || self.depth > self.limits.max_depth
        {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "compiler time or nesting bound exceeded",
                node,
            ));
        }
        Ok(())
    }

    pub(crate) fn at(&self, node: P<Node>) -> SourceLocation {
        source_location_of(node, self.tool_root)
    }

    pub(crate) fn source_at(&self, node: P<Node>) -> SourceLocation {
        source_location_of(node, self.source_root)
    }

    pub(crate) fn find_expression_source(&self, expr: &Expr) -> Option<P<Node>> {
        self.expression_sources
            .iter()
            .find(|(e, _)| e == expr)
            .map(|(_, node)| *node)
    }

    pub(crate) fn record_expression_source(&mut self, expr: &Expr, node: P<Node>) {
        if !self.expression_sources.iter().any(|(e, _)| e == expr) {
            self.expression_sources.push((expr.clone(), node));
        }
    }

    pub(crate) fn symbol(
        &self,
        checker: &mut Checker,
        node: P<Node>,
    ) -> Result<P<Symbol>, CompileError> {
        let resolved = resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            node,
        );
        resolved
            .target
            .ok_or_else(|| CompileError::new("NORN-FLOW-SYMBOL", "unresolved syntax symbol", node))
    }

    pub(crate) fn call_id(&self, checker: &mut Checker, call: P<Node>) -> Option<String> {
        let expr = call.as_call_expression().expression();
        let target = if expr.kind() == Kind::PropertyAccessExpression {
            expr.as_property_access_expression().name()
        } else {
            expr
        };
        resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            target,
        )
        .id
    }

    pub(crate) fn unwrap(mut expr: P<Node>) -> P<Node> {
        loop {
            let kind = expr.kind();
            if kind == Kind::ParenthesizedExpression {
                expr = expr.as_parenthesized_expression().expression();
            } else if kind == Kind::AsExpression {
                expr = expr.as_as_expression().expression;
            } else if kind == Kind::SatisfiesExpression {
                expr = expr.as_satisfies_expression().expression;
            } else if kind == Kind::NonNullExpression {
                expr = expr.as_non_null_expression().expression();
            } else if kind == Kind::TypeAssertionExpression {
                expr = expr.as_type_assertion().expression();
            } else {
                return expr;
            }
        }
    }

    pub(super) fn fresh(&mut self, hint: &str) -> String {
        let mut name = if is_snake_ident(hint) {
            hint.to_owned()
        } else {
            "value".to_owned()
        };
        if self.names.contains(&name) {
            loop {
                self.ordinal = self.ordinal.saturating_add(1);
                name = format!("value_{}", self.ordinal);
                if !self.names.contains(&name) {
                    break;
                }
            }
        }
        self.names.insert(name.clone());
        name
    }

    fn semantic_tokens(node: P<Node>) -> String {
        let Some(sf) = get_source_file_of_node(node) else {
            return String::new();
        };
        let mut scanner = Scanner::new();
        scanner.set_skip_trivia(true);
        scanner.set_script_target(ScriptTarget::ES2022);
        scanner.set_language_variant(LanguageVariant::Standard);
        let text = node_text(sf, node);
        scanner.set_text(&text);
        let mut tokens = Vec::new();
        loop {
            let kind = scanner.scan();
            if kind == Kind::EndOfFile {
                return tokens.join("\u{001f}");
            }
            let text = scanner.token_text();
            let id = kind_numeric_id(kind);
            tokens.push(format!("{id}:{}:{text}", text.len()));
        }
    }

    fn source_map_id(
        &mut self,
        kind: FlowSourceMapStepKind,
        node: P<Node>,
        semantic_id: Option<&str>,
    ) -> String {
        let kind_str = kind.wire_str();
        if let Some(sem) = semantic_id {
            let clean: String = sem
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            return format!("step_{kind_str}_{clean}");
        }
        let tokens = Self::semantic_tokens(node);
        let key = format!("{kind_str}\0{tokens}");
        let occurrence = self.source_map_occurrences.get(&key).copied().unwrap_or(0);
        self.source_map_occurrences
            .insert(key, occurrence.saturating_add(1));
        let digest = sha256_hex(tokens.as_bytes());
        let fingerprint = digest.get(..32).unwrap_or(digest.as_str());
        if occurrence == 0 {
            format!("step_{kind_str}_{fingerprint}")
        } else {
            format!(
                "step_{kind_str}_{fingerprint}_{}",
                occurrence.saturating_add(1)
            )
        }
    }

    pub(super) fn annotate(
        &mut self,
        mut step: Step,
        node: P<Node>,
        kind: FlowSourceMapStepKind,
        semantic_id: Option<&str>,
        branch_labels: Option<Vec<String>>,
        otherwise_label: Option<String>,
    ) -> Step {
        let id = self.source_map_id(kind, node, semantic_id);
        step.set_source_map(FlowSourceMapAnnotation {
            id,
            kind,
            at: Some(self.source_at(node)),
            branch_labels,
            otherwise_label,
        });
        step
    }

    /// Compiles the default-exported `Norn.flow(...)` definition of `self.source`.
    pub(crate) fn compile(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
    ) -> Result<FlowDef, CompileError> {
        check_closure(self.program, self.source, self.limits)?;
        let expr = self.resolve_default_flow_expr(checker)?;
        let args = expr.as_call_expression().arguments().nodes();
        let (Some(&meta_arg), Some(&body_arg)) = (args.first(), args.get(1)) else {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "Norn.flow requires metadata and a flow body",
                expr,
            ));
        };
        let meta = self.flow_metadata(meta_arg, checker, expansion)?;
        let empty = Env::new();
        let body = self.root(body_arg, &empty, checker, expansion)?;
        Ok(FlowDef {
            meta,
            body,
            at: Some(self.at(expr)),
        })
    }

    fn resolve_default_flow_expr(&self, checker: &mut Checker) -> Result<P<Node>, CompileError> {
        let module_sym = checker.get_symbol_at_location_exported(self.source.as_node());
        let exported = module_sym.and_then(|sym| {
            checker
                .get_exports_of_module_exported(sym)
                .into_iter()
                .find(|s| s.name() == "default")
        });
        let target = exported.map(|s| {
            if s.flags().intersects(SymbolFlags::Alias) {
                checker.get_aliased_symbol(s)
            } else {
                s
            }
        });
        let declaration = target.and_then(|s| s.declarations().first().copied());
        let expression = match declaration {
            Some(decl) if decl.kind() == Kind::ExportAssignment => {
                Some(decl.as_export_assignment().expression())
            }
            Some(decl) if decl.kind() == Kind::VariableDeclaration => {
                decl.as_variable_declaration().initializer()
            }
            Some(_) | None => None,
        };
        let Some(mut expr) = expression else {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "export one default Norn.flow definition",
                self.source.as_node(),
            ));
        };
        expr = Self::unwrap(expr);
        if expr.kind() == Kind::Identifier {
            let sym = self.symbol(checker, expr)?;
            if let Some(decl) = sym.value_declaration()
                && decl.kind() == Kind::VariableDeclaration
                && let Some(init) = decl.as_variable_declaration().initializer()
            {
                expr = Self::unwrap(init);
            }
        }
        if expr.kind() != Kind::CallExpression
            || self.call_id(checker, expr).as_deref() != Some("@norn/authoring#Norn.flow")
        {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "default export must be Norn.flow",
                expr,
            ));
        }
        Ok(expr)
    }

    /// Compiles a pure decision/expression callback into guarded [`DecisionRow`]s.
    pub(crate) fn decision_rows(
        &mut self,
        fn_node: P<Node>,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        hooks: Option<&mut dyn DecisionHooks>,
    ) -> Result<Vec<DecisionRow>, CompileError> {
        let mut env = Env::new();
        if let Some(&param) = fn_node.parameters().first() {
            let param_sym = self.symbol(checker, param.as_parameter_declaration().name())?;
            env.insert(param_sym, Expr::input("input", Vec::new()));
        }
        let Some(body) = fn_node.body() else {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "decision callback needs a body",
                fn_node,
            ));
        };
        let mut rows = Vec::new();
        if body.kind() == Kind::Block {
            let stmts = body.as_block().statements.nodes();
            let mut walk = DecisionWalk {
                fn_node,
                rows: &mut rows,
                steps: 0,
                checker,
                expansion,
                hooks,
            };
            self.visit_decision_statements(stmts, env, None, 0, &mut walk)?;
        } else {
            let val = self.data(body, &env, checker, expansion, hooks)?;
            rows.push(DecisionRow {
                condition: None,
                value: val,
                at: self.at(body),
            });
        }
        Ok(rows)
    }

    fn visit_decision_statements(
        &mut self,
        statements: &[P<Node>],
        mut env: Env,
        mut condition: Option<Expr>,
        branch_depth: usize,
        walk: &mut DecisionWalk<'_>,
    ) -> Result<(), CompileError> {
        let mut idx = 0_usize;
        while let Some(&statement) = statements.get(idx) {
            self.check_decision_step(statement, walk)?;
            let kind = statement.kind();
            if kind == Kind::VariableStatement {
                self.bind_decision_var(statement, &mut env, walk)?;
                idx = idx.saturating_add(1);
                continue;
            }
            if kind == Kind::ReturnStatement
                && let Some(ret_expr) = statement.as_return_statement().expression()
            {
                let value = self.data(
                    ret_expr,
                    &env,
                    walk.checker,
                    walk.expansion,
                    walk.hooks.as_deref_mut(),
                )?;
                walk.rows.push(DecisionRow {
                    condition,
                    value,
                    at: self.at(ret_expr),
                });
                return Ok(());
            }
            if kind == Kind::IfStatement {
                let rest = statements.get(idx.saturating_add(1)..).unwrap_or(&[]);
                let Some((next_env, else_cond)) =
                    self.visit_decision_if(statement, rest, env, condition, branch_depth, walk)?
                else {
                    return Ok(());
                };
                env = next_env;
                condition = Some(else_cond);
                idx = idx.saturating_add(1);
                continue;
            }
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "decision callbacks admit const, if/else and return; put effects in Norn.flow",
                statement,
            ));
        }
        Err(CompileError::new(
            "NORN-FLOW-RESULT",
            "decision must return a value on every path",
            walk.fn_node,
        ))
    }

    fn visit_decision_if(
        &mut self,
        statement: P<Node>,
        rest: &[P<Node>],
        env: Env,
        condition: Option<Expr>,
        branch_depth: usize,
        walk: &mut DecisionWalk<'_>,
    ) -> Result<Option<(Env, Expr)>, CompileError> {
        let next_depth = branch_depth.saturating_add(1);
        if next_depth > self.limits.max_depth {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "decision branch nesting exceeds the compiler bound",
                statement,
            ));
        }
        let if_stmt = statement.as_if_statement();
        let test = self.data(
            if_stmt.expression,
            &env,
            walk.checker,
            walk.expansion,
            walk.hooks.as_deref_mut(),
        )?;
        let mut then_seq = branch_statements(if_stmt.then_statement);
        then_seq.extend_from_slice(rest);
        let then_cond = match &condition {
            Some(prev) => Expr::and(prev.clone(), test.clone()),
            None => test.clone(),
        };
        self.visit_decision_statements(&then_seq, env.clone(), Some(then_cond), next_depth, walk)?;
        let neg = Expr::not(test);
        let else_cond = match condition {
            Some(prev) => Expr::and(prev, neg),
            None => neg,
        };
        if let Some(else_stmt) = if_stmt.else_statement {
            let mut else_seq = branch_statements(else_stmt);
            else_seq.extend_from_slice(rest);
            self.visit_decision_statements(&else_seq, env, Some(else_cond), next_depth, walk)?;
            return Ok(None);
        }
        Ok(Some((env, else_cond)))
    }

    fn check_decision_step(
        &self,
        statement: P<Node>,
        walk: &mut DecisionWalk<'_>,
    ) -> Result<(), CompileError> {
        self.check_budget(statement)?;
        walk.steps = walk.steps.saturating_add(1);
        if walk.steps > self.limits.max_expansion_work {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "decision statement walk exceeds the compiler step bound",
                statement,
            ));
        }
        walk.expansion.reserve(1).map_err(|BudgetExceeded(msg)| {
            CompileError::new("NORN-FLOW-UNBOUNDED", msg, statement)
        })?;
        if walk.rows.len() >= FLOW_MAX_ROWS {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "decision exceeds the 1024-row bound",
                walk.fn_node,
            ));
        }
        Ok(())
    }

    fn bind_decision_var(
        &mut self,
        statement: P<Node>,
        env: &mut Env,
        walk: &mut DecisionWalk<'_>,
    ) -> Result<(), CompileError> {
        let decls = statement
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
                    "use const name = value",
                    decl,
                ));
            };
            if vd.name().kind() != Kind::Identifier {
                return Err(CompileError::new(
                    "NORN-FLOW-CAPTURE",
                    "use const name = value",
                    decl,
                ));
            }
            let sym = self.symbol(walk.checker, vd.name())?;
            let val = self.data(
                init,
                env,
                walk.checker,
                walk.expansion,
                walk.hooks.as_deref_mut(),
            )?;
            env.insert(sym, val);
        }
        Ok(())
    }
}

pub(crate) fn json_scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn kind_numeric_id(kind: Kind) -> i16 {
    (0_i16..=400)
        .find(|&value| Kind::from_i16(value) == kind)
        .unwrap_or(-1)
}
