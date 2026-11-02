//! GUARD-DIAGNOSTIC-1: select a registered alias without changing guard behavior.

/// Keep the underlying diagnostic when no registered alias was selected.
pub(crate) const fn guard_failure_code(
    diagnostic: Option<&'static str>,
    error: bool,
) -> &'static str {
    match diagnostic {
        Some(code) => code,
        None if error => "decision.guard_error",
        None => "decision.guard_false",
    }
}
