//! Bounded implementation check of the production origin predicate.
//! Cell byte strings have length 32 (including every valid hex CellId);
//! worlds are main or fork. No hash proof or UTF-8 assumption.
#[path = "../../crates/norn-ir/src/boundary_context.rs"]
mod boundary_context;

#[kani::proof]
#[kani::unwind(34)]
fn foreign_cell_never_matches() {
    let first: [u8; 32] = kani::any();
    let second: [u8; 32] = kani::any();
    let world = if kani::any() { b"main" } else { b"fork" };
    assert_eq!(
        boundary_context::same_origin(&first, world, &second, world),
        first == second
    );
    assert!(!boundary_context::same_origin(
        &first, b"main", &first, b"fork"
    ));
}
