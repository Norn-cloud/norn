//! PARAM-TAX-EXACT-1: bounded component proof of the evaluator's decimal operations.
//! The real evaluator uses this pinned rust_decimal checked multiplication/addition.
//! The native PARAM-TAX scenario supplies correspondence through commit and reopen.

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(128)]
fn exact_tax_on_one_hundred_dollars() {
    for percent in 0_u8..=100 {
        let net = rust_decimal::Decimal::from(100);
        let rate =
            rust_decimal::Decimal::from_parts(u32::from(percent), 0, 0, false, 2).normalize();
        let tax = net
            .checked_mul(rate)
            .expect("bounded tax cannot overflow")
            .normalize();
        let gross = net
            .checked_add(tax)
            .expect("bounded gross cannot overflow")
            .normalize();
        let factor = |scale| match scale {
            0 => 1_i128,
            1 => 10,
            2 => 100,
            _ => {
                assert!(false, "bounded arithmetic cannot increase scale");
                0
            }
        };
        assert_eq!(tax.mantissa(), i128::from(percent) * factor(tax.scale()));
        assert_eq!(
            gross.mantissa(),
            i128::from(100 + percent) * factor(gross.scale())
        );
    }
}
