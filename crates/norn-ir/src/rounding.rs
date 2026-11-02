//! Explicit quantization modes shared by exact value and computation contracts.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// FOLD-ARITHMETIC / VAL-ROUND: a rounding site declares its mode explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    Refuse,
    TowardZero,
    HalfEven,
    HalfAwayFromZero,
}
