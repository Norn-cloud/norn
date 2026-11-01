//! PARAM-EXACT-1: unique active candidates in a half-open recorded-time slice.

/// A normalized interval; instants retain seconds and nanoseconds.
#[derive(Debug, Clone, Copy)]
pub struct ParameterWindow {
    /// Inclusive effective instant.
    pub from: (i64, u32),
    /// Exclusive effective instant, or unbounded.
    pub to: Option<(i64, u32)>,
    /// First recorded revision containing this candidate.
    pub revision: u64,
}

/// Parameter resolution refuses instead of guessing a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterError {
    /// Two active candidates at the same precedence.
    Ambiguous,
    /// No active candidate or explicit default.
    Missing,
    /// Invalid temporal or scope context.
    Invalid,
}

impl ParameterError {
    /// Stable transport diagnostic.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Ambiguous => "core.composition.parameter_ambiguous",
            Self::Missing => "core.composition.parameter_missing",
            Self::Invalid => "setting.validation.invalid_payload",
        }
    }
}

/// PARAM-EXACT-1: `[from,to)` has at most one winner at a recorded revision.
///
/// # Errors
/// Refuses invalid intervals and equal-precedence overlap, even for equal values.
pub fn unique_active(
    windows: &[ParameterWindow],
    at: (i64, u32),
    revision: u64,
) -> Result<Option<usize>, ParameterError> {
    if windows.iter().any(|w| w.to.is_some_and(|to| to <= w.from)) {
        return Err(ParameterError::Invalid);
    }
    let mut winner = None;
    for (index, window) in windows.iter().enumerate() {
        if window.revision <= revision && window.from <= at && window.to.is_none_or(|to| at < to) {
            if winner.is_some() {
                return Err(ParameterError::Ambiguous);
            }
            winner = Some(index);
        }
    }
    Ok(winner)
}
