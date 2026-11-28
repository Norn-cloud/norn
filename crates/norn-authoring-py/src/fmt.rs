//! Deterministic in-process Python formatting powered by `ruff_python_formatter`.

use ruff_formatter::{IndentWidth, LineWidth};
use ruff_python_formatter::{FormatModuleError, PyFormatOptions, QuoteStyle, format_module_source};

use crate::error::{
    CODE_FORMAT, CODE_SYNTAX, CODE_UNBOUNDED, PyAuthoringError, SourceSpan,
};
use crate::line_table::LineTable;
use crate::sandbox::MAX_SOURCE_BYTES;

/// Quote style preference for [`FormatConfig`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormatQuoteStyle {
    /// Prefer double quotes (Ruff default).
    #[default]
    Double,
    /// Prefer single quotes.
    Single,
    /// Preserve existing quote styles.
    Preserve,
}

impl From<FormatQuoteStyle> for QuoteStyle {
    fn from(value: FormatQuoteStyle) -> Self {
        match value {
            FormatQuoteStyle::Double => Self::Double,
            FormatQuoteStyle::Single => Self::Single,
            FormatQuoteStyle::Preserve => Self::Preserve,
        }
    }
}

/// Deterministic formatting configuration for [`format_python_source_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatConfig {
    /// Maximum target line width in columns (must be non-zero; default `88`).
    pub line_width: u16,
    /// Spaces per indentation level (must be non-zero; default `4`).
    pub indent_width: u8,
    /// Quote style preference (default [`FormatQuoteStyle::Double`]).
    pub quote_style: FormatQuoteStyle,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            line_width: 88,
            indent_width: 4,
            quote_style: FormatQuoteStyle::Double,
        }
    }
}

/// Formats Python source text deterministically in-process using `ruff_python_formatter`
/// with default [`FormatConfig`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] if `source` exceeds [`MAX_SOURCE_BYTES`], fails
/// to parse as valid Python, or fails formatting/printing.
pub fn format_python_source(source: &str) -> Result<String, PyAuthoringError> {
    format_python_source_with(source, FormatConfig::default(), "<memory>")
}

/// Formats Python source text deterministically in-process using explicit
/// [`FormatConfig`] and logical `filename` for diagnostics.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] if `source` exceeds [`MAX_SOURCE_BYTES`],
/// `config` has zero `line_width` or `indent_width`, parsing fails, or
/// formatting/printing fails.
pub fn format_python_source_with(
    source: &str,
    config: FormatConfig,
    filename: &str,
) -> Result<String, PyAuthoringError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {MAX_SOURCE_BYTES} byte limit"),
            SourceSpan::new(filename, 1, 0),
        ));
    }

    let line_width = LineWidth::try_from(config.line_width).map_err(|err| {
        PyAuthoringError::unspanned(CODE_FORMAT, format!("invalid line_width: {err}"))
    })?;
    let indent_width = IndentWidth::try_from(config.indent_width).map_err(|err| {
        PyAuthoringError::unspanned(CODE_FORMAT, format!("invalid indent_width: {err}"))
    })?;

    let options = PyFormatOptions::default()
        .with_line_width(line_width)
        .with_indent_width(indent_width)
        .with_quote_style(QuoteStyle::from(config.quote_style));

    let lines = LineTable::new(filename, source);
    let printed = format_module_source(source, options).map_err(|err| match err {
        FormatModuleError::ParseError(parse_err) => PyAuthoringError::spanned(
            CODE_SYNTAX,
            parse_err.error.to_string(),
            lines.span_at(parse_err.location.start()),
        ),
        FormatModuleError::FormatError(fmt_err) => {
            PyAuthoringError::unspanned(CODE_FORMAT, fmt_err.to_string())
        }
        FormatModuleError::PrintError(print_err) => {
            PyAuthoringError::unspanned(CODE_FORMAT, print_err.to_string())
        }
    })?;

    Ok(printed.into_code())
}

/// Returns `true` if `source` is already canonically formatted under default [`FormatConfig`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] if formatting fails.
pub fn is_formatted(source: &str) -> Result<bool, PyAuthoringError> {
    let formatted = format_python_source(source)?;
    Ok(formatted == source)
}
