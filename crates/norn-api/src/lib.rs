//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
pub mod clock;
pub mod contracts;
pub mod openapi;

pub use clock::{Clock, ManualClock};
