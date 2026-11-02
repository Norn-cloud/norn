//! CELL-QUALIFY-1: a local identity is meaningful only in its cell and world.

/// Equality of the originating cell and world, before evidence can be used.
#[must_use]
pub fn same_origin(cell: &str, world: &str, issued_cell: &str, issued_world: &str) -> bool {
    cell == issued_cell && world == issued_world
}
