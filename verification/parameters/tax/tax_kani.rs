//! PARAM-TAX-EXACT-1: bounded component proof of the evaluator's decimal operations.
//! The real evaluator uses this pinned rust_decimal checked multiplication/addition.
//! The native PARAM-TAX scenario supplies correspondence through commit and reopen.

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(32)]
fn exact_tax_on_one_hundred_dollars() {
    let percent: u32 = kani::any();
    kani::assume(percent <= 100);
    let net = rust_decimal::Decimal::from(100);
    let rate = rust_decimal::Decimal::from_parts(percent, 0, 0, false, 2);
    let tax = net.checked_mul(rate).expect("bounded tax cannot overflow");
    let gross = net.checked_add(tax).expect("bounded gross cannot overflow");
    assert_eq!(tax, rust_decimal::Decimal::from(percent));
    assert_eq!(gross, rust_decimal::Decimal::from(100 + percent));
}
