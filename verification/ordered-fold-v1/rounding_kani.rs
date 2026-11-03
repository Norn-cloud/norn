//! VAL-ROUND: bounded normalization and canonical-default omission.
use super::Rounding;

#[kani::proof]
fn omitted_and_explicit_refuse_have_one_canonical_mode() {
    let mode = match kani::any::<u8>() % 4 {
        0 => Rounding::Refuse,
        1 => Rounding::TowardZero,
        2 => Rounding::HalfEven,
        _ => Rounding::HalfAwayFromZero,
    };
    assert_eq!(
        Rounding::effective(None),
        Rounding::effective(Some(Rounding::Refuse))
    );
    assert_eq!(Rounding::effective(Some(mode)), mode);
    assert_eq!(mode.canonical_field().is_none(), mode == Rounding::Refuse);
}
