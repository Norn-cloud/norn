//! VAL-EXACT-1: bounded proof of production numeric-refusal classification.
use super::{INEXACT, runtime_error_code};
use norn_ir::RuntimeValueError;

#[kani::proof]
fn numeric_refusals_have_the_canonical_code() {
    let variant: u8 = kani::any();
    kani::assume(variant < 3);
    let error = match variant {
        0 => RuntimeValueError::Inexact {
            number: String::new(),
        },
        1 => RuntimeValueError::NonFinite,
        _ => RuntimeValueError::InvalidJson {
            message: String::new(),
        },
    };
    assert_eq!(
        runtime_error_code(&error),
        if variant < 2 { Some(INEXACT) } else { None }
    );
}
