//! Bounded implementation check of the production origin predicate.
//! Cell inputs are 32 ASCII hex digits; worlds are main or fork. No hash proof.
#[path = "../../crates/norn-ir/src/boundary_context.rs"]
mod boundary_context;

#[kani::proof]
#[kani::unwind(66)]
fn foreign_cell_never_matches() {
    let a: [u8; 32] = kani::any();
    let b: [u8; 32] = kani::any();
    for byte in a.iter().chain(b.iter()) {
        kani::assume(byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
    }
    let first = std::str::from_utf8(&a).unwrap();
    let second = std::str::from_utf8(&b).unwrap();
    let world = if kani::any() { "main" } else { "fork" };
    assert_eq!(
        boundary_context::same_origin(first, world, second, world),
        a == b
    );
    assert!(!boundary_context::same_origin(first, "main", first, "fork"));
}
