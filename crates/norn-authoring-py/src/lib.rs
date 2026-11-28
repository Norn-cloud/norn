//! Native Rust Python authoring compiler, AST sandbox, expression lowerer, and
//! formatter for Norn (`norn-authoring-py`).
//!
//! Embeds Astral's `ruff_python_parser`, `ruff_python_ast`, and
//! `ruff_python_formatter` (`0.0.23`, MIT) to statically validate Python
//! authoring modules, lower expression callbacks directly to `norn-zen/7`
//! declarative IR without executing Python, and format Python authoring
//! sources deterministically in-process.

mod budget;
pub mod decimal;
pub mod error;
pub mod expr;
mod expr_lower;
mod expr_validate;
pub mod fmt;
mod line_table;
pub mod sandbox;

pub use decimal::{MAX_DECIMAL_DIGITS, MAX_DECIMAL_EXPONENT, format_exact_decimal};
pub use error::{
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FORBIDDEN_ATTR, CODE_FORBIDDEN_CALL,
    CODE_FORBIDDEN_CONTROL_FLOW, CODE_FORBIDDEN_IMPORT, CODE_FORBIDDEN_NAME, CODE_FORBIDDEN_SYNTAX,
    CODE_FORMAT, CODE_RAW_EXPR, CODE_SYNTAX, CODE_UNBOUNDED, CODE_UNKNOWN_FIELD, PyAuthoringError,
    SourceSpan,
};
pub use expr::{
    ALLOWED_CALLBACK_FUNCS, ALLOWED_CALLBACK_METHODS, BinaryOp, CompareOp, LogicalOp, LowerContext,
    ZenExpr, ZenLiteral, lower_callback_source, lower_callback_source_with_limits,
    lower_module_callbacks, render_zen_string, validate_callback_source,
    validate_callback_source_with_limits,
};
pub use fmt::{
    FormatConfig, FormatQuoteStyle, format_python_source, format_python_source_with, is_formatted,
};
pub use sandbox::{
    ALLOWED_IMPORT_ROOTS, FORBIDDEN_ATTRS, FORBIDDEN_NAMES, MAX_AST_DEPTH, MAX_AST_NODES,
    MAX_SOURCE_BYTES, PreflightReport, SandboxLimits, allowed_import_symbols, is_forbidden_attr,
    is_forbidden_name, preflight_source, preflight_source_with_limits,
};
