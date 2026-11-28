//! Typed diagnostics and source spans for `norn-authoring-py`.

use std::fmt;

/// Stable diagnostic code when source bytes, AST node count, or AST depth exceed budgets.
pub const CODE_UNBOUNDED: &str = "NORN-PY-UNBOUNDED";
/// Stable diagnostic code for Python syntax or unsupported-syntax errors.
pub const CODE_SYNTAX: &str = "NORN-PY-SYNTAX";
/// Stable diagnostic code for forbidden module or symbol imports.
pub const CODE_FORBIDDEN_IMPORT: &str = "NORN-PY-FORBIDDEN-IMPORT";
/// Stable diagnostic code for forbidden dunder or shadowed names/aliases/parameters.
pub const CODE_FORBIDDEN_NAME: &str = "NORN-PY-FORBIDDEN-NAME";
/// Stable diagnostic code for forbidden builtins or non-allowlisted callback calls.
pub const CODE_FORBIDDEN_CALL: &str = "NORN-PY-FORBIDDEN-CALL";
/// Stable diagnostic code for forbidden dunder or internal frame/formatting attributes.
pub const CODE_FORBIDDEN_ATTR: &str = "NORN-PY-FORBIDDEN-ATTR";
/// Stable diagnostic code for binary float or complex literals refused under `VAL-EXACT-1`.
pub const CODE_FLOAT_LITERAL: &str = "NORN-PY-FLOAT-LITERAL";
/// Stable diagnostic code for forbidden imperative statements or metaclass/dunder definitions.
pub const CODE_FORBIDDEN_SYNTAX: &str = "NORN-PY-FORBIDDEN-SYNTAX";
/// Stable diagnostic code for forbidden Python control flow, boolean short-circuiting, or callback shapes.
pub const CODE_FORBIDDEN_CONTROL_FLOW: &str = "NORN-PY-FORBIDDEN-CONTROL-FLOW";
/// Stable diagnostic code for invalid ZEN expressions, literals, or `Decimal` bounds.
pub const CODE_EXPRESSION: &str = "NORN-PY-EXPRESSION";
/// Stable diagnostic code when a callback or boolean combinator receives a raw literal instead of a closed `Expr`.
pub const CODE_RAW_EXPR: &str = "NORN-PY-RAW-EXPR";
/// Stable diagnostic code when a callback references an undeclared field on a symbolic subject.
pub const CODE_UNKNOWN_FIELD: &str = "NORN-PY-UNKNOWN-FIELD";
/// Stable diagnostic code for formatter configuration, layout, or printing errors.
pub const CODE_FORMAT: &str = "NORN-PY-FORMAT";
/// Stable diagnostic code for schema structure, metadata, or cardinality errors.
pub const CODE_SCHEMA: &str = "NORN-PY-SCHEMA";
/// Stable diagnostic code when raw IR dictionaries or subclassing of closed IR specs are attempted.
pub const CODE_RAW_IR: &str = "NORN-PY-RAW-IR";
/// Stable diagnostic code when `Annotated[base, meta...]` base types conflict with metadata.
pub const CODE_TYPE_MISMATCH: &str = "NORN-PY-TYPE-MISMATCH";
/// Stable diagnostic code for unsafe, unknown, or overly deep field type annotations.
pub const CODE_UNSAFE_ANNOTATION: &str = "NORN-PY-UNSAFE-ANNOTATION";
/// Stable diagnostic code when an interface declares a lifecycle block.
pub const CODE_INTERFACE_LIFECYCLE: &str = "NORN-PY-INTERFACE-LIFECYCLE";
/// Stable diagnostic code for invalid dotted object-type names or disallowed interface defaults.
pub const CODE_FLOW_LITERAL: &str = "NORN-FLOW-LITERAL";
/// Stable diagnostic code for canonical JSON or `Norn1` serialization refusals.
pub const CODE_CANONICAL: &str = "NORN-PY-CANONICAL";
/// Stable diagnostic code for duplicate pack identifiers during composition.
pub const CODE_PACK_DUPLICATE: &str = "NORN-PY-PACK-DUPLICATE";
/// Stable diagnostic code when a composed pack requires a missing dependency pack.
pub const CODE_PACK_MISSING_DEP: &str = "NORN-PY-PACK-MISSING-DEP";
/// Stable diagnostic code when pack `requires` edges form a cycle.
pub const CODE_PACK_CYCLE: &str = "NORN-PY-PACK-CYCLE";
/// Stable diagnostic code when packs collide under `fail_closed` conflict policy or pass an invalid policy.
pub const CODE_PACK_CONFLICT: &str = "NORN-PY-PACK-CONFLICT";
/// Stable diagnostic code when `DeploymentSpec` omits non-empty `org_id` or `environment_id`.
pub const CODE_DEPLOYMENT_SCOPE: &str = "NORN-PY-DEPLOYMENT-SCOPE";
/// Stable diagnostic code for invalid `DeploymentSpec` worker endpoint keys or URLs.
pub const CODE_DEPLOYMENT_ENDPOINT: &str = "NORN-PY-DEPLOYMENT-ENDPOINT";
/// Stable diagnostic code when `DeploymentSpec.initial_settings` contains a secret-like key name.
pub const CODE_RAW_SECRET: &str = "NORN-PY-RAW-SECRET";
/// Stable diagnostic code for module elaboration errors (such as disallowed attribute lookups or statements).
pub const CODE_ELABORATION: &str = "NORN-PY-ELABORATION";
/// Stable diagnostic code for bounded file I/O refusals (symlinks, non-regular files, read errors).
pub const CODE_IO: &str = "NORN-PY-IO";

/// Exact author source span captured from Python source text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    /// Logical file path or source name.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// 0-based column offset (matching Python `ast` `col_offset`).
    pub column: usize,
}

impl SourceSpan {
    /// Constructs a new [`SourceSpan`].
    #[must_use]
    pub fn new(file: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            file: file.into(),
            line,
            column,
        }
    }

    /// Formats the span identically to `norn_py.canonical.SourceSpan.format()`.
    #[must_use]
    pub fn format_location(&self) -> String {
        if self.column > 0 {
            format!("{}:{}:{}", self.file, self.line, self.column)
        } else {
            format!("{}:{}", self.file, self.line)
        }
    }
}

impl fmt::Display for SourceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.column > 0 {
            write!(f, "{}:{}:{}", self.file, self.line, self.column)
        } else {
            write!(f, "{}:{}", self.file, self.line)
        }
    }
}

/// Typed error returned by the native Python authoring compiler, AST sandbox,
/// expression lowerer, and formatter.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PyAuthoringError {
    /// Structured diagnostic anchored at a Python source span.
    #[error("{span}: [{code}] {message}")]
    Spanned {
        /// Stable `NORN-PY-*` diagnostic code.
        code: &'static str,
        /// Actionable human-readable diagnostic message.
        message: String,
        /// Exact source location where the violation occurred.
        span: SourceSpan,
    },
    /// Structured diagnostic without a specific source span.
    #[error("[{code}] {message}")]
    Unspanned {
        /// Stable `NORN-PY-*` diagnostic code.
        code: &'static str,
        /// Actionable human-readable diagnostic message.
        message: String,
    },
}

impl PyAuthoringError {
    /// Constructs a spanned diagnostic error.
    #[must_use]
    pub fn spanned(code: &'static str, message: impl Into<String>, span: SourceSpan) -> Self {
        Self::Spanned {
            code,
            message: message.into(),
            span,
        }
    }

    /// Constructs an unspanned diagnostic error.
    #[must_use]
    pub fn unspanned(code: &'static str, message: impl Into<String>) -> Self {
        Self::Unspanned {
            code,
            message: message.into(),
        }
    }

    /// Constructs a diagnostic error with an optional span.
    #[must_use]
    pub fn with_optional_span(
        code: &'static str,
        message: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        match span {
            Some(span) => Self::spanned(code, message, span),
            None => Self::unspanned(code, message),
        }
    }

    /// Returns the stable `NORN-PY-*` diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Spanned { code, .. } | Self::Unspanned { code, .. } => code,
        }
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Spanned { message, .. } | Self::Unspanned { message, .. } => message,
        }
    }

    /// Returns the associated [`SourceSpan`], if present.
    #[must_use]
    pub const fn span(&self) -> Option<&SourceSpan> {
        match self {
            Self::Spanned { span, .. } => Some(span),
            Self::Unspanned { .. } => None,
        }
    }
}
