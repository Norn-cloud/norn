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

#[cfg(test)]
mod tests {
    use super::guard_failure_code;

    #[test]
    fn guard_failure_code_preserves_alias_or_underlying_code() {
        for aliased in [false, true] {
            for error in [false, true] {
                let alias = aliased.then_some("selected.guard.code");
                let expected = if aliased {
                    "selected.guard.code"
                } else if error {
                    "decision.guard_error"
                } else {
                    "decision.guard_false"
                };
                assert_eq!(guard_failure_code(alias, error), expected);
            }
        }
    }
}
