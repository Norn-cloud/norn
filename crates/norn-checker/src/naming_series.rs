//! Shared tokenizer for the closed `EntityIR` naming-series pattern grammar.
//!
//! The kernel uses the same parser after definitions have been admitted so it
//! does not maintain a second interpretation of date, scope, and counter
//! placeholders. This parses syntax only; entity-specific checks remain in
//! `object_types`.

use std::fmt;

/// One token parsed from a naming-series pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamingPatternToken<'a> {
    /// Literal text copied to the generated key.
    Literal(&'a str),
    /// Four-digit local calendar year (`{YYYY}`).
    YearFour,
    /// Two-digit local calendar year (`{YY}`).
    YearTwo,
    /// Two-digit local calendar month (`{MM}`).
    Month,
    /// Two-digit local calendar day (`{DD}`).
    Day,
    /// A required scope property (`{property_id}`).
    ScopeField(&'a str),
    /// A zero-padded counter with this exact width (`{#####}`).
    Counter {
        /// Number of decimal digits; overflow is refused by the allocator.
        width: u8,
    },
}

/// A syntax error in an `EntityIR` naming-series pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamingPatternError {
    /// The pattern is empty or whitespace only.
    Empty,
    /// A closing brace has no opening brace.
    UnmatchedClosingBrace,
    /// An opening brace has no matching closing brace.
    UnclosedToken,
    /// A token is not a date, counter, or property-id placeholder.
    UnknownToken,
    /// Counter width must be between one and eighteen `#` characters.
    InvalidCounterWidth,
    /// Exactly one counter token is required.
    MissingCounter,
    /// More than one counter token is present.
    MultipleCounters,
    /// A boundary involving a scope token has no safe literal delimiter.
    AdjacentVariables,
}

impl fmt::Display for NamingPatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "pattern must not be empty",
            Self::UnmatchedClosingBrace => "closing brace has no matching opening brace",
            Self::UnclosedToken => "opening brace has no matching closing brace",
            Self::UnknownToken => "token must be a supported date, counter, or scope field",
            Self::InvalidCounterWidth => "counter width must be one to eighteen # characters",
            Self::MissingCounter => "pattern must contain exactly one counter token",
            Self::MultipleCounters => "pattern contains more than one counter token",
            Self::AdjacentVariables => {
                "a boundary involving a scope token requires a delimiter outside its value alphabet"
            }
        })
    }
}

impl std::error::Error for NamingPatternError {}

/// Parses the closed token syntax shared by the checker and kernel.
///
/// Scope field names are returned as text; `norn-checker` verifies that each
/// one is declared and that its property is admissible for a generated key.
///
/// # Errors
///
/// Returns a [`NamingPatternError`] for malformed braces, unsupported tokens,
/// invalid counter width, missing or multiple counters, or an ambiguous scope
/// boundary. Adjacent fixed-width date and counter tokens are allowed, while
/// each boundary involving a scope token needs a literal delimiter outside
/// the `[A-Za-z0-9_]` scope alphabet.
pub fn parse_naming_series_pattern(
    pattern: &str,
) -> Result<Vec<NamingPatternToken<'_>>, NamingPatternError> {
    if pattern.trim().is_empty() {
        return Err(NamingPatternError::Empty);
    }

    let mut remaining = pattern;
    let mut tokens = Vec::new();
    let mut counter_count = 0_u8;
    let mut previous_token = false;
    let mut previous_scope = false;
    let mut has_safe_delimiter = false;

    while !remaining.is_empty() {
        let open = remaining.find('{');
        let close = remaining.find('}');
        if close.is_some_and(|close| open.is_none_or(|open| close < open)) {
            return Err(NamingPatternError::UnmatchedClosingBrace);
        }

        let Some(open) = open else {
            tokens.push(NamingPatternToken::Literal(remaining));
            break;
        };
        let literal = remaining
            .get(..open)
            .ok_or(NamingPatternError::UnknownToken)?;
        if !literal.is_empty() {
            tokens.push(NamingPatternToken::Literal(literal));
            has_safe_delimiter |= literal
                .chars()
                .any(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        }

        let token_start = open
            .checked_add(1)
            .ok_or(NamingPatternError::UnknownToken)?;
        let tail = remaining
            .get(token_start..)
            .ok_or(NamingPatternError::UnknownToken)?;
        let token_end = tail.find('}').ok_or(NamingPatternError::UnclosedToken)?;
        let token_text = tail
            .get(..token_end)
            .ok_or(NamingPatternError::UnknownToken)?;
        let next = token_start
            .checked_add(token_end)
            .and_then(|index| index.checked_add(1))
            .ok_or(NamingPatternError::UnknownToken)?;
        remaining = remaining
            .get(next..)
            .ok_or(NamingPatternError::UnknownToken)?;

        let token = match token_text {
            "YYYY" => NamingPatternToken::YearFour,
            "YY" => NamingPatternToken::YearTwo,
            "MM" => NamingPatternToken::Month,
            "DD" => NamingPatternToken::Day,
            value if !value.is_empty() && value.bytes().all(|byte| byte == b'#') => {
                let width = u8::try_from(value.len())
                    .map_err(|_error| NamingPatternError::InvalidCounterWidth)?;
                if !(1..=18).contains(&width) {
                    return Err(NamingPatternError::InvalidCounterWidth);
                }
                counter_count = counter_count.saturating_add(1);
                NamingPatternToken::Counter { width }
            }
            value if is_member_id(value) => NamingPatternToken::ScopeField(value),
            _ => return Err(NamingPatternError::UnknownToken),
        };

        let current_scope = matches!(token, NamingPatternToken::ScopeField(_));
        if previous_token && (previous_scope || current_scope) && !has_safe_delimiter {
            return Err(NamingPatternError::AdjacentVariables);
        }
        previous_token = true;
        previous_scope = current_scope;
        has_safe_delimiter = false;
        tokens.push(token);
    }

    match counter_count {
        0 => Err(NamingPatternError::MissingCounter),
        1 => Ok(tokens),
        _ => Err(NamingPatternError::MultipleCounters),
    }
}

fn is_member_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::{NamingPatternError, NamingPatternToken, parse_naming_series_pattern as parse};

    #[test]
    fn parses_supported_tokens_and_literals() {
        assert_eq!(
            parse("INV-{company}-{YYYY}-{#####}"),
            Ok(vec![
                NamingPatternToken::Literal("INV-"),
                NamingPatternToken::ScopeField("company"),
                NamingPatternToken::Literal("-"),
                NamingPatternToken::YearFour,
                NamingPatternToken::Literal("-"),
                NamingPatternToken::Counter { width: 5 },
            ])
        );
        assert_eq!(
            parse("X/{YY}/{MM}/{DD}/{#}"),
            Ok(vec![
                NamingPatternToken::Literal("X/"),
                NamingPatternToken::YearTwo,
                NamingPatternToken::Literal("/"),
                NamingPatternToken::Month,
                NamingPatternToken::Literal("/"),
                NamingPatternToken::Day,
                NamingPatternToken::Literal("/"),
                NamingPatternToken::Counter { width: 1 },
            ])
        );
    }

    #[test]
    fn rejects_ambiguous_or_malformed_patterns() {
        for (pattern, expected) in [
            (" ", NamingPatternError::Empty),
            ("INV-}", NamingPatternError::UnmatchedClosingBrace),
            ("INV-{YYYY", NamingPatternError::UnclosedToken),
            ("INV-{unknown-token}-{#}", NamingPatternError::UnknownToken),
            (
                "INV-{###################}",
                NamingPatternError::InvalidCounterWidth,
            ),
            ("INV-{YYYY}", NamingPatternError::MissingCounter),
            ("INV-{#}-{#}", NamingPatternError::MultipleCounters),
            ("INV-{company}_{#}", NamingPatternError::AdjacentVariables),
            (
                "INV-{company}{YYYY}-{#}",
                NamingPatternError::AdjacentVariables,
            ),
        ] {
            assert_eq!(parse(pattern), Err(expected), "{pattern}");
        }
    }

    #[test]
    fn permits_adjacent_fixed_width_date_and_counter_tokens() {
        assert_eq!(
            parse("INV-{YYYY}{MM}{DD}-{#####}"),
            Ok(vec![
                NamingPatternToken::Literal("INV-"),
                NamingPatternToken::YearFour,
                NamingPatternToken::Month,
                NamingPatternToken::Day,
                NamingPatternToken::Literal("-"),
                NamingPatternToken::Counter { width: 5 },
            ])
        );
    }
}
