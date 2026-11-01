//! VAL-UNIT-1: bounded checks of the production membership function.
#[path = "../../crates/norn-checker/src/unit_codes.rs"]
mod unit_codes;

#[kani::proof]
#[kani::unwind(8)]
fn membership_respects_system_and_vocabulary() {
    let bytes: [u8; 2] = kani::any();
    let is_ucum: bool = kani::any();
    let code = std::str::from_utf8(&bytes).unwrap_or("");
    assert_eq!(
        unit_codes::admitted(is_ucum, code, ["mg", "kg"].iter().copied()),
        is_ucum && matches!(bytes, [b'm', b'g'] | [b'k', b'g'])
    );
    assert!(!unit_codes::admitted(is_ucum, code, std::iter::empty()));
}
