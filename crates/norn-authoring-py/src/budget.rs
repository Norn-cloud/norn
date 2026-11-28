//! Shared AST node-count and nesting-depth budget enforcement matching `CPython`'s `ast.walk`.

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    Alias, Arguments, BoolOp, BytesLiteral, CmpOp, Comprehension, Decorator, ElifElseClause,
    ExceptHandler, Expr, ExprContext, FString, InterpolatedStringElement, Keyword, MatchCase,
    Operator, Parameter, Parameters, Pattern, PatternArguments, PatternKeyword, Stmt,
    StringLiteral, TString, TypeParam, TypeParams, UnaryOp, WithItem,
};
use ruff_text_size::Ranged;

use crate::error::{CODE_UNBOUNDED, PyAuthoringError, SourceSpan};
use crate::line_table::LineTable;
use crate::sandbox::SandboxLimits;

/// Summary returned by [`check_body_budget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BudgetStats {
    pub(crate) ast_node_count: usize,
    pub(crate) max_depth_seen: usize,
}

/// Checks AST node-count and nesting-depth budgets over a statement slice before recursion.
pub(crate) fn check_body_budget(
    body: &[Stmt],
    lines: &LineTable<'_>,
    limits: SandboxLimits,
) -> Result<BudgetStats, PyAuthoringError> {
    let mut visitor = AstBudgetVisitor::new(lines, limits);
    visitor.visit_body(body);
    visitor.finish()
}

struct AstBudgetVisitor<'a> {
    lines: &'a LineTable<'a>,
    limits: SandboxLimits,
    node_count: usize,
    depth: usize,
    max_depth_seen: usize,
    last_span: SourceSpan,
    error: Option<PyAuthoringError>,
}

impl<'a> AstBudgetVisitor<'a> {
    fn new(lines: &'a LineTable<'a>, limits: SandboxLimits) -> Self {
        Self {
            lines,
            limits,
            node_count: 0,
            depth: 0,
            max_depth_seen: 0,
            last_span: lines.file_start(),
            error: None,
        }
    }

    fn finish(self) -> Result<BudgetStats, PyAuthoringError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        Ok(BudgetStats {
            ast_node_count: self.node_count,
            max_depth_seen: self.max_depth_seen,
        })
    }

    fn enter_with_span(&mut self, span: SourceSpan) -> bool {
        if self.error.is_some() {
            return false;
        }
        self.last_span = span.clone();
        self.node_count = self.node_count.saturating_add(1);
        if self.node_count > self.limits.max_ast_nodes {
            self.error = Some(PyAuthoringError::spanned(
                CODE_UNBOUNDED,
                format!(
                    "source exceeds {} AST node limit",
                    self.limits.max_ast_nodes
                ),
                span,
            ));
            return false;
        }
        self.depth = self.depth.saturating_add(1);
        if self.depth > self.max_depth_seen {
            self.max_depth_seen = self.depth;
        }
        if self.depth > self.limits.max_ast_depth {
            self.error = Some(PyAuthoringError::spanned(
                CODE_UNBOUNDED,
                format!(
                    "source exceeds {} AST depth limit",
                    self.limits.max_ast_depth
                ),
                span,
            ));
            return false;
        }
        true
    }

    fn enter_ranged<T: Ranged>(&mut self, node: &T) -> bool {
        let span = self.lines.span_at(node.range().start());
        self.enter_with_span(span)
    }

    fn enter_leaf(&mut self) -> bool {
        let span = self.last_span.clone();
        self.enter_with_span(span)
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

impl<'a> Visitor<'a> for AstBudgetVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if self.enter_ranged(stmt) {
            visitor::walk_stmt(self, stmt);
            self.leave();
        }
    }

    fn visit_decorator(&mut self, decorator: &'a Decorator) {
        if self.enter_ranged(decorator) {
            visitor::walk_decorator(self, decorator);
            self.leave();
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if self.enter_ranged(expr) {
            visitor::walk_expr(self, expr);
            self.leave();
        }
    }

    fn visit_expr_context(&mut self, expr_context: &'a ExprContext) {
        if self.enter_leaf() {
            visitor::walk_expr_context(self, expr_context);
            self.leave();
        }
    }

    fn visit_bool_op(&mut self, bool_op: &'a BoolOp) {
        if self.enter_leaf() {
            visitor::walk_bool_op(self, bool_op);
            self.leave();
        }
    }

    fn visit_operator(&mut self, operator: &'a Operator) {
        if self.enter_leaf() {
            visitor::walk_operator(self, operator);
            self.leave();
        }
    }

    fn visit_unary_op(&mut self, unary_op: &'a UnaryOp) {
        if self.enter_leaf() {
            visitor::walk_unary_op(self, unary_op);
            self.leave();
        }
    }

    fn visit_cmp_op(&mut self, cmp_op: &'a CmpOp) {
        if self.enter_leaf() {
            visitor::walk_cmp_op(self, cmp_op);
            self.leave();
        }
    }

    fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
        if self.enter_ranged(comprehension) {
            visitor::walk_comprehension(self, comprehension);
            self.leave();
        }
    }

    fn visit_except_handler(&mut self, except_handler: &'a ExceptHandler) {
        if self.enter_ranged(except_handler) {
            visitor::walk_except_handler(self, except_handler);
            self.leave();
        }
    }

    fn visit_arguments(&mut self, arguments: &'a Arguments) {
        if self.enter_ranged(arguments) {
            visitor::walk_arguments(self, arguments);
            self.leave();
        }
    }

    fn visit_parameters(&mut self, parameters: &'a Parameters) {
        if self.enter_ranged(parameters) {
            visitor::walk_parameters(self, parameters);
            self.leave();
        }
    }

    fn visit_parameter(&mut self, parameter: &'a Parameter) {
        if self.enter_ranged(parameter) {
            visitor::walk_parameter(self, parameter);
            self.leave();
        }
    }

    fn visit_keyword(&mut self, keyword: &'a Keyword) {
        if self.enter_ranged(keyword) {
            visitor::walk_keyword(self, keyword);
            self.leave();
        }
    }

    fn visit_alias(&mut self, alias: &'a Alias) {
        if self.enter_ranged(alias) {
            visitor::walk_alias(self, alias);
            self.leave();
        }
    }

    fn visit_with_item(&mut self, with_item: &'a WithItem) {
        if self.enter_ranged(with_item) {
            visitor::walk_with_item(self, with_item);
            self.leave();
        }
    }

    fn visit_type_params(&mut self, type_params: &'a TypeParams) {
        if self.enter_ranged(type_params) {
            visitor::walk_type_params(self, type_params);
            self.leave();
        }
    }

    fn visit_type_param(&mut self, type_param: &'a TypeParam) {
        if self.enter_ranged(type_param) {
            visitor::walk_type_param(self, type_param);
            self.leave();
        }
    }

    fn visit_match_case(&mut self, match_case: &'a MatchCase) {
        if self.enter_ranged(match_case) {
            visitor::walk_match_case(self, match_case);
            self.leave();
        }
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        if self.enter_ranged(pattern) {
            visitor::walk_pattern(self, pattern);
            self.leave();
        }
    }

    fn visit_pattern_arguments(&mut self, pattern_arguments: &'a PatternArguments) {
        if self.enter_ranged(pattern_arguments) {
            visitor::walk_pattern_arguments(self, pattern_arguments);
            self.leave();
        }
    }

    fn visit_pattern_keyword(&mut self, pattern_keyword: &'a PatternKeyword) {
        if self.enter_ranged(pattern_keyword) {
            visitor::walk_pattern_keyword(self, pattern_keyword);
            self.leave();
        }
    }

    fn visit_elif_else_clause(&mut self, elif_else_clause: &'a ElifElseClause) {
        if self.enter_ranged(elif_else_clause) {
            visitor::walk_elif_else_clause(self, elif_else_clause);
            self.leave();
        }
    }

    fn visit_f_string(&mut self, f_string: &'a FString) {
        if self.enter_ranged(f_string) {
            visitor::walk_f_string(self, f_string);
            self.leave();
        }
    }

    fn visit_interpolated_string_element(
        &mut self,
        interpolated_string_element: &'a InterpolatedStringElement,
    ) {
        if self.enter_ranged(interpolated_string_element) {
            visitor::walk_interpolated_string_element(self, interpolated_string_element);
            self.leave();
        }
    }

    fn visit_t_string(&mut self, t_string: &'a TString) {
        if self.enter_ranged(t_string) {
            visitor::walk_t_string(self, t_string);
            self.leave();
        }
    }

    fn visit_string_literal(&mut self, string_literal: &'a StringLiteral) {
        if self.enter_ranged(string_literal) {
            visitor::walk_string_literal(self, string_literal);
            self.leave();
        }
    }

    fn visit_bytes_literal(&mut self, bytes_literal: &'a BytesLiteral) {
        if self.enter_ranged(bytes_literal) {
            visitor::walk_bytes_literal(self, bytes_literal);
            self.leave();
        }
    }
}
