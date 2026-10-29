//! Deterministic, explicitly bounded graph algorithms with exact quantities.
//!
//! See the crate README for edge orientation, arithmetic and metering contracts.
#![forbid(unsafe_code)]
// Deterministic core policy (#3299); host adapters retain their own arithmetic/collections.
#![deny(
    clippy::float_arithmetic,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::disallowed_types
)]

mod algorithms;
mod graph;
mod quantity;

pub use algorithms::{
    Aggregation, Bom, Composition, CriticalPath, Explosion, Part, Timing, critical_path, explode,
    hierarchical_rollup,
};
pub use graph::{
    Budget, BudgetExceeded, Direction, Error, Graph, Limit, reachability, topological_order,
};
pub use quantity::{ArithmeticError, Quantity};
pub use rust_decimal::Decimal;
