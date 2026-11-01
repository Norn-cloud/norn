//! VAL-UNIT-1: bounded checks of the production membership function.
#[path = "../../crates/norn-checker/src/unit_codes.rs"]
mod unit_codes;

#[kani::proof]
#[kani::unwind(8)]
fn missing_vocabulary_and_unknown_system_refuse() {
    let bytes: [u8; 2] = kani::any();
    let code = std::str::from_utf8(&bytes).unwrap_or("");
    assert!(!unit_codes::admitted(false, code, ["mg", "kg"].into_iter()));
    assert!(!unit_codes::admitted(true, code, [].into_iter()));
}

#[kani::proof]
#[kani::unwind(8)]
fn membership_preserves_exact_spelling() {
    let bytes: [u8; 2] = kani::any();
    if let Ok(code) = std::str::from_utf8(&bytes) {
        assert_eq!(
            unit_codes::admitted(true, code, ["mg", "kg"].into_iter()),
            matches!(bytes, [b'm', b'g'] | [b'k', b'g'])
        );
    }
}
