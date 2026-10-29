mod conv;
mod de;
mod ser;

pub use de::RcValueDeserializer;
use rust_decimal::Decimal;
use std::rc::Rc;

/// The [`RcValue::Object`] map: insertion-ordered (Norn #3404).
pub type RcObject = indexmap::IndexMap<Rc<str>, RcValue, ahash::RandomState>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RcValue {
    #[default]
    Null,
    Bool(bool),
    Number(Decimal),
    String(Rc<str>),
    Array(Vec<RcValue>),
    /// Norn (#3404): insertion-ordered, so a serde round trip keeps key order
    /// independent of hash seeds.
    Object(RcObject),
}
