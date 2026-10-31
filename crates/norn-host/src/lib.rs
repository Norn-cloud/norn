//! Native host perimeter. The pure wire contracts remain in `norn-api`.
pub mod refusal;
pub use refusal::{Refusal, RefusalCategory};
pub mod scope;
pub use scope::CellScope;
