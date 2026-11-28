//! Fail-closed Python AST sandbox preflight mirroring `tools/norn-py/norn_py/sandbox.py`.
//!
//! Parses authored Python source in-process with `ruff_python_parser` and walks
//! the `ruff_python_ast` tree with zero Python execution, enforcing:
//! - closed module and symbol import allowlists (rejecting relative and `*` imports),
//! - bans on dunder names/attributes/definitions, internal frame attributes,
//!   string `.format`/`.format_map`, custom metaclasses, and dynamic builtins,
//! - bans on binary float/complex literals (`VAL-EXACT-1`),
//! - bans on imperative control flow (`if`, loops, comprehensions, `try`, `with`,
//!   `async`/`await`, `yield`, `match`, `raise`, `del`, `assert`, `global`/`nonlocal`),
//! - pre-lowering validation of every `def` and `lambda` callback AST,
//! - strict source byte, AST node count, and AST nesting depth budgets.

use std::collections::BTreeMap;

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Expr, ModModule, Number, Parameter, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

use crate::error::{
    CODE_FLOAT_LITERAL, CODE_FORBIDDEN_ATTR, CODE_FORBIDDEN_CALL, CODE_FORBIDDEN_CONTROL_FLOW,
    CODE_FORBIDDEN_IMPORT, CODE_FORBIDDEN_NAME, CODE_FORBIDDEN_SYNTAX, CODE_SYNTAX, CODE_UNBOUNDED,
    PyAuthoringError, SourceSpan,
};
use crate::expr_validate::{validate_function_callback, validate_lambda_callback};
use crate::line_table::LineTable;

/// Maximum admitted Python source size in UTF-8 bytes (1 MiB).
pub const MAX_SOURCE_BYTES: usize = 1_048_576;
/// Maximum admitted AST node count per module.
pub const MAX_AST_NODES: usize = 100_000;
/// Maximum admitted AST nesting depth per module.
pub const MAX_AST_DEPTH: usize = 128;

/// Closed allowlist of importable root modules in Norn Python authoring files.
pub const ALLOWED_IMPORT_ROOTS: &[&str] = &["__future__", "decimal", "enum", "norn_py", "typing"];

const ALLOWED_FUTURE_SYMBOLS: &[&str] = &["annotations"];
const ALLOWED_DECIMAL_SYMBOLS: &[&str] = &["Decimal"];
const ALLOWED_ENUM_SYMBOLS: &[&str] = &["Enum", "StrEnum", "auto"];
const ALLOWED_TYPING_SYMBOLS: &[&str] = &[
    "Annotated",
    "ClassVar",
    "Final",
    "Literal",
    "Optional",
    "Protocol",
    "Union",
];

/// Closed allowlist of authoring symbols importable from `norn_py`
/// (`norn_py.__all__` minus host-runner/sandbox-internal functions).
const ALLOWED_NORN_PY_SYMBOLS: &[&str] = &[
    "BoolTag",
    "CellHostKind",
    "Concept",
    "Date",
    "DateTag",
    "DecimalTag",
    "DependencyPin",
    "DeploymentSpec",
    "Duration",
    "DurationTag",
    "DynamicSym",
    "ElaborationResult",
    "EnvelopeOptions",
    "Expr",
    "Field",
    "FieldMeta",
    "FnRef",
    "IntTag",
    "InvariantSpec",
    "JsonValue",
    "KeySpec",
    "LawDecl",
    "LifecycleSpec",
    "Link",
    "LinkSpec",
    "Marking",
    "MarkingDecl",
    "MarkingsCatalog",
    "MaskSpec",
    "Money",
    "NORN1_DOMAIN_TAG",
    "NonJsonValueError",
    "NornFloatRefusalError",
    "NornTracingError",
    "PackCategory",
    "PackConflictPolicy",
    "PackManifest",
    "PropertyTypeSpec",
    "Quantity",
    "Rate",
    "SecretRef",
    "SourceSpan",
    "StrTag",
    "Sym",
    "Timestamp",
    "TimestampTag",
    "TransitionSpec",
    "UnitRef",
    "add_days",
    "add_seconds",
    "all_of",
    "any_of",
    "between",
    "boolean",
    "canonical_json",
    "canonical_norn1_bytes",
    "canonical_norn1_sha256",
    "compose_packs",
    "contains",
    "date_before",
    "date_type",
    "days_between",
    "decimal_type",
    "duration_type",
    "enum_type",
    "field_default",
    "field_ref",
    "fn_ref",
    "if_else",
    "integer",
    "interval_gap_seconds",
    "interval_intersection_seconds",
    "intervals_overlap",
    "invariant",
    "law",
    "lifecycle",
    "link",
    "lit",
    "mask_last_n",
    "mask_null",
    "mask_pseudonym",
    "mask_redacted",
    "mask_refuse",
    "norn_len",
    "not_",
    "parse_date",
    "parse_timestamp",
    "rate_type",
    "ref_type",
    "seconds_between",
    "text",
    "timestamp_at_or_before",
    "timestamp_before",
    "timestamp_type",
    "to_json",
    "transition",
    "when",
];

/// Dynamic builtins and escape hatches forbidden in Norn Python authoring modules.
pub const FORBIDDEN_NAMES: &[&str] = &[
    "__build_class__",
    "__builtins__",
    "__import__",
    "__loader__",
    "__spec__",
    "breakpoint",
    "classmethod",
    "compile",
    "delattr",
    "dir",
    "eval",
    "exec",
    "exit",
    "float",
    "getattr",
    "globals",
    "help",
    "input",
    "locals",
    "memoryview",
    "object",
    "open",
    "property",
    "quit",
    "setattr",
    "staticmethod",
    "super",
    "type",
    "vars",
];

/// Internal frame/code attributes forbidden in both callbacks and modules.
pub(crate) const FORBIDDEN_INTERNAL_ATTRS: &[&str] = &[
    "ag_frame",
    "co_code",
    "cr_frame",
    "f_back",
    "f_builtins",
    "f_globals",
    "f_locals",
    "func_globals",
    "gi_frame",
    "tb_frame",
];

/// Module-level forbidden attributes (internal frame attributes plus `format`/`format_map`).
pub const FORBIDDEN_ATTRS: &[&str] = &[
    "ag_frame",
    "co_code",
    "cr_frame",
    "f_back",
    "f_builtins",
    "f_globals",
    "f_locals",
    "format",
    "format_map",
    "func_globals",
    "gi_frame",
    "tb_frame",
];

/// Configurable AST and source-size budgets for [`preflight_source_with_limits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SandboxLimits {
    /// Maximum UTF-8 byte length of the Python source text.
    pub max_source_bytes: usize,
    /// Maximum number of AST nodes visited.
    pub max_ast_nodes: usize,
    /// Maximum AST nesting depth.
    pub max_ast_depth: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_ast_nodes: MAX_AST_NODES,
            max_ast_depth: MAX_AST_DEPTH,
        }
    }
}

/// Result of a passing Layer-1 AST sandbox preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    /// Deterministic map of top-level definition names to exact author [`SourceSpan`]s.
    pub spans: BTreeMap<String, SourceSpan>,
    /// Total AST nodes inspected during preflight.
    pub ast_node_count: usize,
    /// Maximum AST nesting depth encountered during preflight.
    pub max_depth_seen: usize,
}

/// Returns `true` if `name` is in [`FORBIDDEN_NAMES`].
#[must_use]
pub fn is_forbidden_name(name: &str) -> bool {
    FORBIDDEN_NAMES.binary_search(&name).is_ok()
}

/// Returns `true` if `attr` is a dunder or in [`FORBIDDEN_ATTRS`].
#[must_use]
pub fn is_forbidden_attr(attr: &str) -> bool {
    attr.starts_with("__") || FORBIDDEN_ATTRS.binary_search(&attr).is_ok()
}

/// Returns the closed symbol allowlist for an allowed module, or `None` if the module is forbidden.
#[must_use]
pub fn allowed_import_symbols(module_name: &str) -> Option<&'static [&'static str]> {
    match module_name {
        "__future__" => Some(ALLOWED_FUTURE_SYMBOLS),
        "decimal" => Some(ALLOWED_DECIMAL_SYMBOLS),
        "enum" => Some(ALLOWED_ENUM_SYMBOLS),
        "typing" => Some(ALLOWED_TYPING_SYMBOLS),
        "norn_py" => Some(ALLOWED_NORN_PY_SYMBOLS),
        _ => None,
    }
}

/// Runs Layer-1 static AST sandbox preflight on Python source text using default limits.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on size/node/depth budget exhaustion, Python
/// syntax errors, forbidden imports, forbidden builtins/dunders/attributes,
/// float literals, imperative syntax/control flow, or invalid callback ASTs.
pub fn preflight_source(source: &str, filename: &str) -> Result<PreflightReport, PyAuthoringError> {
    preflight_source_with_limits(source, filename, SandboxLimits::default())
}

/// Runs Layer-1 static AST sandbox preflight with explicit [`SandboxLimits`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on any budget or sandbox policy violation.
pub fn preflight_source_with_limits(
    source: &str,
    filename: &str,
    limits: SandboxLimits,
) -> Result<PreflightReport, PyAuthoringError> {
    let lines = LineTable::new(filename, source);
    let parsed = parse_and_validate_module(source, &lines, limits)?;
    Ok(parsed.1)
}

pub(crate) fn parse_and_validate_module(
    source: &str,
    lines: &LineTable<'_>,
    limits: SandboxLimits,
) -> Result<(ModModule, PreflightReport), PyAuthoringError> {
    if source.len() > limits.max_source_bytes {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {} byte limit", limits.max_source_bytes),
            lines.file_start(),
        ));
    }

    let parsed = parse_module(source).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_SYNTAX,
            err.error.to_string(),
            lines.span_at(err.location.start()),
        )
    })?;

    if let Some(unsupported) = parsed.unsupported_syntax_errors().first() {
        return Err(PyAuthoringError::spanned(
            CODE_SYNTAX,
            unsupported.to_string(),
            lines.span_at(unsupported.range().start()),
        ));
    }

    let module = parsed.into_syntax();
    let mut walker = SandboxVisitor {
        lines,
        limits,
        node_count: 0,
        depth: 0,
        max_depth_seen: 0,
        error: None,
    };
    walker.visit_body(&module.body);

    if let Some(err) = walker.error {
        return Err(err);
    }

    let spans = collect_top_level_spans(&module.body, lines);
    let report = PreflightReport {
        spans,
        ast_node_count: walker.node_count,
        max_depth_seen: walker.max_depth_seen,
    };
    Ok((module, report))
}

fn collect_top_level_spans(body: &[Stmt], lines: &LineTable<'_>) -> BTreeMap<String, SourceSpan> {
    let mut spans = BTreeMap::new();
    for stmt in body {
        let span = lines.span_at(stmt.range().start());
        match stmt {
            Stmt::ClassDef(class_def) => {
                spans.insert(class_def.name.id.as_str().to_owned(), span);
            }
            Stmt::FunctionDef(func_def) => {
                spans.insert(func_def.name.id.as_str().to_owned(), span);
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Expr::Name(name) = target {
                        spans.insert(name.id.as_str().to_owned(), span.clone());
                    }
                }
            }
            Stmt::AnnAssign(ann_assign) => {
                if let Expr::Name(name) = ann_assign.target.as_ref() {
                    spans.insert(name.id.as_str().to_owned(), span);
                }
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::AugAssign(_)
            | Stmt::For(_)
            | Stmt::While(_)
            | Stmt::If(_)
            | Stmt::With(_)
            | Stmt::Match(_)
            | Stmt::Raise(_)
            | Stmt::Try(_)
            | Stmt::Assert(_)
            | Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }
    spans
}

fn check_import_module_and_names(
    module_name: &str,
    imported_names: Option<&[&str]>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let Some(allowed_syms) = allowed_import_symbols(module_name) else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_IMPORT,
            format!(
                "import '{module_name}' is forbidden in Norn authoring modules (allowed: {ALLOWED_IMPORT_ROOTS:?})"
            ),
            span.clone(),
        ));
    };

    if let Some(names) = imported_names {
        for &sym_name in names {
            if sym_name == "*" {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_IMPORT,
                    format!(
                        "wildcard `from {module_name} import *` is forbidden in Norn authoring modules"
                    ),
                    span.clone(),
                ));
            }
            if !allowed_syms.contains(&sym_name) {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_IMPORT,
                    format!(
                        "symbol '{sym_name}' cannot be imported from '{module_name}' in Norn authoring modules"
                    ),
                    span.clone(),
                ));
            }
        }
    }
    Ok(())
}

fn check_alias_asname(asname: Option<&str>, span: &SourceSpan) -> Result<(), PyAuthoringError> {
    if let Some(alias) = asname
        && (alias.starts_with("__") || is_forbidden_name(alias))
    {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_NAME,
            format!("import alias '{alias}' is forbidden in Norn authoring modules"),
            span.clone(),
        ));
    }
    Ok(())
}

struct SandboxVisitor<'a> {
    lines: &'a LineTable<'a>,
    limits: SandboxLimits,
    node_count: usize,
    depth: usize,
    max_depth_seen: usize,
    error: Option<PyAuthoringError>,
}

impl SandboxVisitor<'_> {
    fn fail(&mut self, err: PyAuthoringError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }

    fn enter_node(&mut self, span: SourceSpan) -> bool {
        if self.error.is_some() {
            return false;
        }
        self.node_count = self.node_count.saturating_add(1);
        if self.node_count > self.limits.max_ast_nodes {
            self.fail(PyAuthoringError::spanned(
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
            self.fail(PyAuthoringError::spanned(
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

    fn leave_node(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn check_stmt(&mut self, stmt: &Stmt, span: &SourceSpan) {
        match stmt {
            Stmt::Import(import_stmt) => self.check_import(import_stmt, span),
            Stmt::ImportFrom(import_from) => self.check_import_from(import_from, span),
            Stmt::ClassDef(class_def) => self.check_class_def(class_def, span),
            Stmt::FunctionDef(func_def) => self.check_function_def(func_def, span),
            Stmt::Global(_) | Stmt::Nonlocal(_) => self.fail_syntax(
                "`global`/`nonlocal` mutation is forbidden in Norn authoring modules",
                span,
            ),
            Stmt::For(for_stmt) => {
                if for_stmt.is_async {
                    self.fail_async(span);
                } else {
                    self.fail_syntax(
                        "`For` is forbidden in declarative Norn authoring modules",
                        span,
                    );
                }
            }
            Stmt::With(with_stmt) => {
                if with_stmt.is_async {
                    self.fail_async(span);
                } else {
                    self.fail_syntax(
                        "`With` is forbidden in declarative Norn authoring modules",
                        span,
                    );
                }
            }
            Stmt::Try(_) => self.fail_syntax(
                "`try`/`except` is forbidden in declarative Norn authoring modules (prevents swallowing symbolic-tracing refusals)",
                span,
            ),
            Stmt::While(_) => self.fail_syntax(
                "`While` is forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::Raise(_) => self.fail_syntax(
                "`Raise` is forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::Delete(_) => self.fail_syntax(
                "`Delete` is forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::Assert(_) => self.fail_syntax(
                "`Assert` is forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::Match(_) => self.fail_syntax(
                "`Match` is forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::IpyEscapeCommand(_) => self.fail_syntax(
                "IPython escape commands are forbidden in declarative Norn authoring modules",
                span,
            ),
            Stmt::If(_) => self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "Python `if`/`else` is forbidden in declarative Norn authoring modules; use `when(cond, yes, no)` / `if_else(cond, yes, no)` inside expressions",
                span.clone(),
            )),
            Stmt::Return(_)
            | Stmt::TypeAlias(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_) => {}
        }
    }

    fn fail_syntax(&mut self, message: &str, span: &SourceSpan) {
        self.fail(PyAuthoringError::spanned(
            CODE_FORBIDDEN_SYNTAX,
            message,
            span.clone(),
        ));
    }

    fn fail_async(&mut self, span: &SourceSpan) {
        self.fail_syntax(
            "`async`/`await` is forbidden in declarative Norn authoring modules",
            span,
        );
    }

    fn check_import(&mut self, import_stmt: &ruff_python_ast::StmtImport, span: &SourceSpan) {
        if import_stmt.is_lazy {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_IMPORT,
                "lazy imports are not admitted in Norn authoring modules",
                span.clone(),
            ));
            return;
        }
        for alias in &import_stmt.names {
            if let Err(err) = check_import_module_and_names(alias.name.id.as_str(), None, span) {
                self.fail(err);
                return;
            }
            if let Err(err) = check_alias_asname(alias.asname.as_ref().map(|a| a.id.as_str()), span)
            {
                self.fail(err);
                return;
            }
        }
    }

    fn check_import_from(
        &mut self,
        import_from: &ruff_python_ast::StmtImportFrom,
        span: &SourceSpan,
    ) {
        if import_from.is_lazy {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_IMPORT,
                "lazy imports are not admitted in Norn authoring modules",
                span.clone(),
            ));
            return;
        }
        let Some(module) = import_from
            .module
            .as_ref()
            .filter(|_| import_from.level == 0)
        else {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_IMPORT,
                "relative imports are not admitted in Norn authoring modules",
                span.clone(),
            ));
            return;
        };
        let names: Vec<&str> = import_from
            .names
            .iter()
            .map(|alias| alias.name.id.as_str())
            .collect();
        if let Err(err) = check_import_module_and_names(module.id.as_str(), Some(&names), span) {
            self.fail(err);
            return;
        }
        for alias in &import_from.names {
            if let Err(err) = check_alias_asname(alias.asname.as_ref().map(|a| a.id.as_str()), span)
            {
                self.fail(err);
                return;
            }
        }
    }

    fn check_class_def(&mut self, class_def: &ruff_python_ast::StmtClassDef, span: &SourceSpan) {
        let name = class_def.name.id.as_str();
        if name.starts_with("__") || is_forbidden_name(name) {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_SYNTAX,
                format!("class name '{name}' is forbidden in Norn authoring modules"),
                span.clone(),
            ));
            return;
        }
        for kw in class_def.keywords() {
            if kw
                .arg
                .as_ref()
                .is_some_and(|arg| arg.id.as_str() == "metaclass")
            {
                self.fail(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_SYNTAX,
                    "custom `metaclass=` is forbidden in Norn authoring modules",
                    span.clone(),
                ));
                return;
            }
        }
        for item in &class_def.body {
            if let Stmt::FunctionDef(method) = item
                && method.name.id.as_str().starts_with("__")
            {
                self.fail(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_SYNTAX,
                    format!(
                        "dunder method '{}' is forbidden in Norn authoring classes",
                        method.name.id.as_str()
                    ),
                    self.lines.span_at(method.range().start()),
                ));
                return;
            }
        }
    }

    fn check_function_def(
        &mut self,
        func_def: &ruff_python_ast::StmtFunctionDef,
        span: &SourceSpan,
    ) {
        if func_def.is_async {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_SYNTAX,
                "`async`/`await` is forbidden in declarative Norn authoring modules",
                span.clone(),
            ));
            return;
        }
        let name = func_def.name.id.as_str();
        if name.starts_with("__") || is_forbidden_name(name) {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_SYNTAX,
                format!("function name '{name}' is forbidden in Norn authoring modules"),
                span.clone(),
            ));
            return;
        }
        if let Err(err) = validate_function_callback(func_def, self.lines) {
            self.fail(err);
        }
    }

    fn check_expr(&mut self, expr: &Expr, span: &SourceSpan) {
        match expr {
            Expr::NumberLiteral(num) => self.check_number_literal(&num.value, span),
            Expr::Attribute(attr) => self.check_attribute(attr.attr.id.as_str(), span),
            Expr::Name(name) => self.check_name(name.id.as_str(), span),
            Expr::Lambda(lambda) => {
                if let Err(err) = validate_lambda_callback(lambda, self.lines) {
                    self.fail(err);
                }
            }
            Expr::If(_) => self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "Python `if`/`else` is forbidden in declarative Norn authoring modules; use `when(cond, yes, no)` / `if_else(cond, yes, no)` inside expressions",
                span.clone(),
            )),
            Expr::ListComp(_) | Expr::SetComp(_) | Expr::DictComp(_) | Expr::Generator(_) => {
                self.fail(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "comprehensions and generator expressions are forbidden in declarative Norn authoring modules",
                    span.clone(),
                ));
            }
            Expr::Await(_) => self.fail_async(span),
            Expr::Yield(_) | Expr::YieldFrom(_) => self.fail_syntax(
                "`yield` generators are forbidden in declarative Norn authoring modules",
                span,
            ),
            Expr::IpyEscapeCommand(_) => self.fail_syntax(
                "IPython escape commands are forbidden in declarative Norn authoring modules",
                span,
            ),
            Expr::BoolOp(_)
            | Expr::Named(_)
            | Expr::BinOp(_)
            | Expr::UnaryOp(_)
            | Expr::Dict(_)
            | Expr::Set(_)
            | Expr::Compare(_)
            | Expr::Call(_)
            | Expr::FString(_)
            | Expr::TString(_)
            | Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::Subscript(_)
            | Expr::Starred(_)
            | Expr::List(_)
            | Expr::Tuple(_)
            | Expr::Slice(_) => {}
        }
    }

    fn check_number_literal(&mut self, number: &Number, span: &SourceSpan) {
        match number {
            Number::Float(val) => self.fail(PyAuthoringError::spanned(
                CODE_FLOAT_LITERAL,
                format!("float literal {val} is refused (VAL-EXACT-1); use Decimal('{val}')"),
                span.clone(),
            )),
            Number::Complex { .. } => self.fail(PyAuthoringError::spanned(
                CODE_FLOAT_LITERAL,
                "complex numeric literal is refused (VAL-EXACT-1); use exact Decimal instead",
                span.clone(),
            )),
            Number::Int(_) => {}
        }
    }

    fn check_attribute(&mut self, attr_name: &str, span: &SourceSpan) {
        if is_forbidden_attr(attr_name) {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_ATTR,
                format!("attribute access .{attr_name} is forbidden in Norn authoring modules"),
                span.clone(),
            ));
        }
    }

    fn check_name(&mut self, id: &str, span: &SourceSpan) {
        if is_forbidden_name(id) {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CALL,
                format!("reference to '{id}' is forbidden in Norn authoring modules"),
                span.clone(),
            ));
        } else if id.starts_with("__") {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_NAME,
                format!("reference to '{id}' is forbidden in Norn authoring modules"),
                span.clone(),
            ));
        }
    }
}

impl<'a> Visitor<'a> for SandboxVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        let span = self.lines.span_at(stmt.range().start());
        if !self.enter_node(span.clone()) {
            return;
        }
        self.check_stmt(stmt, &span);
        if self.error.is_none() {
            visitor::walk_stmt(self, stmt);
        }
        self.leave_node();
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        let span = self.lines.span_at(expr.range().start());
        if !self.enter_node(span.clone()) {
            return;
        }
        self.check_expr(expr, &span);
        if self.error.is_none() {
            visitor::walk_expr(self, expr);
        }
        self.leave_node();
    }

    fn visit_parameter(&mut self, parameter: &'a Parameter) {
        let span = self.lines.span_at(parameter.range().start());
        if !self.enter_node(span.clone()) {
            return;
        }
        let name = parameter.name.id.as_str();
        if name.starts_with("__") || is_forbidden_name(name) {
            self.fail(PyAuthoringError::spanned(
                CODE_FORBIDDEN_NAME,
                format!("parameter name '{name}' is forbidden in Norn authoring modules"),
                span,
            ));
        }
        if self.error.is_none() {
            visitor::walk_parameter(self, parameter);
        }
        self.leave_node();
    }
}
