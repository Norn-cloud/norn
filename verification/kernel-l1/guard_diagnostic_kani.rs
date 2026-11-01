//! Bounded engine evidence for GUARD-DIAGNOSTIC-1 over the production selector.
#[path = "../../crates/norn-kernel/src/guard_diagnostic.rs"]
mod guard_diagnostic;

#[kani::proof]
fn registered_alias_or_underlying_code_is_preserved() {
    let aliased: bool = kani::any();
    let error: bool = kani::any();
    let alias = aliased.then_some("selected.guard.code");
    let actual = guard_diagnostic::guard_failure_code(alias, error);
    assert_eq!(
        actual,
        if aliased {
            "selected.guard.code"
        } else if error {
            "decision.guard_error"
        } else {
            "decision.guard_false"
        }
    );
}
