//! Exact runtime values (#2770, T3-NUM-1).
//!
//! Definition IR keeps [`crate::JsonValue`] (an `f64` number) and its
//! historical hashes (D37). Values that flow through a running cell (fact
//! values, decision outputs, command and timer records, the store, the ABI,
//! and the API) are [`RuntimeValue`]s instead, whose numbers are exact
//! [`Decimal`]s: money and quantities never pass through `f64`.
//!
//! # Numbers
//!
//! A [`Decimal`] is the expression engine's decimal (`rust_decimal`): a 96-bit
//! integer mantissa with a scale of at most 28 fractional digits, the
//! `norn-zen/6` profile's number (`crates/norn-expr/PROFILE.md`). A value
//! outside that range is refused, never rounded. There is one numeric kind:
//! an integer is a decimal of scale 0.
//!
//! The canonical text of a decimal is its Norn1 number form: plain digits, no
//! exponent, no trailing fractional zeros, no negative zero (`1.50` and
//! `1.5e0` are both `1.5`; `-0.0` is `0`). Equality, ordering, hashing, and
//! serialization use that value, so `1.50 == 1.5`.
//!
//! # JSON
//!
//! With `serde_json` a decimal is written as a JSON number token with its
//! exact canonical text, and read back from the exact token text, through
//! `serde_json`'s raw-value support (not `arbitrary_precision`, which D37
//! keeps out of the kernel). Parsing JSON text with
//! [`RuntimeValue::from_json_text`] or `serde_json::from_str`/`from_slice`
//! is exact. Converting an in-memory `serde_json::Value` (for example from the
//! `json!` macro) reads each number from `serde_json`'s own rendering, which is
//! exact for integers and the shortest round-trip text for a float: that is a
//! construction convenience, not an ingress path. The other way round,
//! `serde_json::to_value` of a decimal that a binary `serde_json` number cannot
//! hold is an error, never a silent rounding.
//!
//! # Hashing
//!
//! [`RuntimeValue::canonical_json`] is Norn1 canonical JSON (strings and keys
//! NFC-normalized, keys sorted, numbers in canonical decimal form), so a
//! runtime value hashes like the same document parsed from text by
//! `norn_canonical`, natively and on `wasm32`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use norn_canonical::{CanonicalJsonError, JsonValue as CanonicalJson};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::JsonValue;

/// An exact decimal: the engine's 96-bit decimal, scale at most 28.
#[derive(Clone, Copy)]
pub struct Decimal(rust_decimal::Decimal);

/// A runtime object: properties in key order.
pub type RuntimeObject = BTreeMap<String, RuntimeValue>;

/// A JSON-shaped runtime value whose numbers are exact [`Decimal`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum RuntimeValue {
    /// `null`.
    #[default]
    Null,
    /// A boolean.
    Bool(bool),
    /// An exact decimal number.
    Number(Decimal),
    /// A string, kept as given (hashing NFC-normalizes it).
    String(String),
    /// An ordered array.
    Array(Vec<RuntimeValue>),
    /// An object with unique keys, in key order.
    Object(RuntimeObject),
}

/// Why a value is not an exact runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeValueError {
    /// The text is not valid JSON (or not a JSON number, for a decimal).
    InvalidJson {
        /// What was wrong.
        message: String,
    },
    /// A number is outside the exact decimal range (96-bit mantissa, scale
    /// at most 28); it would have to be rounded.
    Inexact {
        /// The number as given.
        number: String,
    },
    /// An `f64` that is not finite.
    NonFinite,
}

impl fmt::Display for RuntimeValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { message } => write!(f, "invalid JSON: {message}"),
            Self::Inexact { number } => write!(
                f,
                "{number} is not an exact decimal (96-bit mantissa, at most 28 fractional digits); numbers are never rounded"
            ),
            Self::NonFinite => f.write_str("a non-finite number is not a decimal"),
        }
    }
}

impl std::error::Error for RuntimeValueError {}

impl From<CanonicalJsonError> for RuntimeValueError {
    fn from(error: CanonicalJsonError) -> Self {
        Self::InvalidJson {
            message: error.to_string(),
        }
    }
}

impl Decimal {
    /// Zero.
    pub const ZERO: Self = Self(rust_decimal::Decimal::ZERO);

    /// The decimal of a JSON number token (`12.50`, `-3`, `1.5e2`), exactly.
    ///
    /// # Errors
    ///
    /// [`RuntimeValueError::InvalidJson`] if `text` is not one JSON number;
    /// [`RuntimeValueError::Inexact`] if it is outside the decimal range.
    pub fn parse(text: &str) -> Result<Self, RuntimeValueError> {
        match norn_canonical::parse_json(text)? {
            CanonicalJson::Number(canonical) => Self::from_canonical_text(&canonical, text),
            CanonicalJson::Null
            | CanonicalJson::Bool(_)
            | CanonicalJson::String(_)
            | CanonicalJson::Array(_)
            | CanonicalJson::Object(_) => Err(RuntimeValueError::InvalidJson {
                message: format!("{text:?} is not a JSON number"),
            }),
        }
    }

    /// The decimal of a Norn1 canonical number string (already normalized:
    /// plain digits, no exponent). `original` names the input in errors.
    fn from_canonical_text(canonical: &str, original: &str) -> Result<Self, RuntimeValueError> {
        let inexact = || RuntimeValueError::Inexact {
            number: original.to_owned(),
        };
        let value = rust_decimal::Decimal::from_str_exact(canonical).map_err(|_error| inexact())?;
        let value = value.normalize();
        // `from_str_exact` refuses rounding; the round trip guards any other
        // representation change.
        if value.to_string() != canonical {
            return Err(inexact());
        }
        Ok(Self(value))
    }

    /// The canonical decimal text, for example `-12.5`.
    #[must_use]
    pub fn canonical(&self) -> String {
        self.0.to_string()
    }

    /// The number of fractional digits of the canonical form.
    #[must_use]
    pub const fn scale(&self) -> u32 {
        self.0.scale()
    }

    /// Whether the value is an integer.
    #[must_use]
    pub fn is_integer(&self) -> bool {
        self.0.is_integer()
    }

    /// The value as an `i64`, when it is an integer in range.
    #[must_use]
    pub fn to_i64(&self) -> Option<i64> {
        if !self.is_integer() {
            return None;
        }
        self.canonical().parse().ok()
    }

    /// The exact decimal of a finite `f64`, read from its shortest
    /// round-trip text (`0.1` is `0.1`, not the binary expansion). For
    /// in-memory construction only; ingress parses text.
    ///
    /// # Errors
    ///
    /// [`RuntimeValueError::NonFinite`] or [`RuntimeValueError::Inexact`].
    pub fn from_f64_shortest(value: f64) -> Result<Self, RuntimeValueError> {
        if !value.is_finite() {
            return Err(RuntimeValueError::NonFinite);
        }
        // `Display` for `f64` is the shortest text that parses back to the
        // same bits, never in exponent form.
        Self::parse(&format!("{value}"))
    }
}

impl From<i64> for Decimal {
    fn from(value: i64) -> Self {
        Self(rust_decimal::Decimal::from(value))
    }
}

impl From<u32> for Decimal {
    fn from(value: u32) -> Self {
        Self(rust_decimal::Decimal::from(value))
    }
}

impl From<u64> for Decimal {
    fn from(value: u64) -> Self {
        Self(rust_decimal::Decimal::from(value))
    }
}

impl FromStr for Decimal {
    type Err = RuntimeValueError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Decimal({})", self.0)
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hash for Decimal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Values are kept normalized, so the canonical text is the value.
        self.canonical().hash(state);
    }
}

impl Decimal {
    /// Serialize into an in-memory `serde_json::Value`, whose numbers are
    /// binary: an integer in `u64`/`i64` range, or the `f64` whose shortest
    /// round-trip text is this decimal, directly (never through
    /// `serde_json`'s own text parser); anything else is an error.
    fn serialize_binary<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = self.canonical();
        if self.is_integer() {
            if let Ok(value) = text.parse::<u64>() {
                return serializer.serialize_u64(value);
            }
            if let Ok(value) = text.parse::<i64>() {
                return serializer.serialize_i64(value);
            }
        }
        match text.parse::<f64>() {
            Ok(float)
                if float.is_finite()
                    && Self::parse(&format!("{float}")).is_ok_and(|back| back == *self) =>
            {
                serializer.serialize_f64(float)
            }
            _ => Err(serde::ser::Error::custom(format!(
                "{self} does not fit a serde_json::Value number exactly; serialize to text (serde_json::to_string, norn_ir::runtime::encode) instead"
            ))),
        }
    }
}

/// Whether `S` builds an in-memory `serde_json::Value` (`serde_json::to_value`
/// and the nested serializers it uses), whose numbers are binary.
fn builds_serde_json_value<S>() -> bool {
    std::any::type_name::<S>().starts_with("serde_json::value::")
}

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // An in-memory `serde_json::Value` holds numbers as `f64`/`u64`/`i64`
        // (without `arbitrary_precision`, D37): a decimal it cannot hold is
        // an error, never a silent rounding. Text serialization is exact.
        if builds_serde_json_value::<S>() {
            return self.serialize_binary(serializer);
        }
        // A raw JSON number token: exact with `serde_json`, independent of
        // `arbitrary_precision`.
        let raw = RawValue::from_string(self.canonical()).map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::parse(raw.get()).map_err(D::Error::custom)
    }
}

impl RuntimeValue {
    /// Parse one JSON document exactly: numbers become [`Decimal`]s from their
    /// token text; strings and keys are kept as given.
    ///
    /// # Errors
    ///
    /// Invalid JSON (including duplicate keys) or an inexact number.
    pub fn from_json_text(text: impl AsRef<[u8]>) -> Result<Self, RuntimeValueError> {
        Self::from_canonical(&norn_canonical::parse_json_verbatim(text)?)
    }

    /// The runtime value of a parsed Norn1 value (whose numbers are exact
    /// decimal strings).
    ///
    /// # Errors
    ///
    /// A number outside the decimal range.
    pub fn from_canonical(value: &CanonicalJson) -> Result<Self, RuntimeValueError> {
        Ok(match value {
            CanonicalJson::Null => Self::Null,
            CanonicalJson::Bool(flag) => Self::Bool(*flag),
            CanonicalJson::Number(text) => Self::Number(Decimal::from_canonical_text(text, text)?),
            CanonicalJson::String(text) => Self::String(text.clone()),
            CanonicalJson::Array(items) => Self::Array(
                items
                    .iter()
                    .map(Self::from_canonical)
                    .collect::<Result<_, _>>()?,
            ),
            CanonicalJson::Object(entries) => Self::Object(
                entries
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), Self::from_canonical(item)?)))
                    .collect::<Result<_, RuntimeValueError>>()?,
            ),
        })
    }

    /// The value as a Norn1 value (numbers as canonical decimal strings).
    #[must_use]
    pub fn to_canonical(&self) -> CanonicalJson {
        match self {
            Self::Null => CanonicalJson::Null,
            Self::Bool(flag) => CanonicalJson::Bool(*flag),
            Self::Number(number) => CanonicalJson::Number(number.canonical()),
            Self::String(text) => CanonicalJson::String(text.clone()),
            Self::Array(items) => {
                CanonicalJson::Array(items.iter().map(Self::to_canonical).collect())
            }
            Self::Object(entries) => CanonicalJson::Object(
                entries
                    .iter()
                    .map(|(key, item)| (key.clone(), item.to_canonical()))
                    .collect(),
            ),
        }
    }

    /// Norn1 canonical JSON bytes of the value: the bytes a runtime value is
    /// hashed over.
    ///
    /// # Errors
    ///
    /// Keys that collide after NFC normalization.
    pub fn canonical_json(&self) -> Result<Vec<u8>, RuntimeValueError> {
        Ok(norn_canonical::serialize_json(&self.to_canonical())?)
    }

    /// The runtime value of a definition-IR value. IR numbers are `f64`;
    /// each is read from its shortest round-trip text. Definition defaults
    /// that must be exact are parsed from their source text at admission
    /// instead.
    ///
    /// # Errors
    ///
    /// A non-finite or out-of-range number.
    pub fn from_ir(value: &JsonValue) -> Result<Self, RuntimeValueError> {
        Ok(match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(flag) => Self::Bool(*flag),
            JsonValue::Number(number) => Self::Number(Decimal::from_f64_shortest(*number)?),
            JsonValue::String(text) => Self::String(text.clone()),
            JsonValue::Array(items) => {
                Self::Array(items.iter().map(Self::from_ir).collect::<Result<_, _>>()?)
            }
            JsonValue::Object(entries) => Self::Object(
                entries
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), Self::from_ir(item)?)))
                    .collect::<Result<_, RuntimeValueError>>()?,
            ),
        })
    }

    /// The decimal, if this is a number.
    #[must_use]
    pub const fn as_decimal(&self) -> Option<&Decimal> {
        match self {
            Self::Number(number) => Some(number),
            Self::Null | Self::Bool(_) | Self::String(_) | Self::Array(_) | Self::Object(_) => None,
        }
    }

    /// The text, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::Array(_) | Self::Object(_) => None,
        }
    }

    /// Whether this is `null`.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// The flag, if this is a boolean.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(flag) => Some(*flag),
            Self::Null | Self::Number(_) | Self::String(_) | Self::Array(_) | Self::Object(_) => {
                None
            }
        }
    }

    /// The integer, if this is a number that is an integer in `i64` range.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_decimal().and_then(Decimal::to_i64)
    }

    /// The entries, if this is an object.
    #[must_use]
    pub const fn as_object(&self) -> Option<&RuntimeObject> {
        match self {
            Self::Object(entries) => Some(entries),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) | Self::Array(_) => None,
        }
    }

    /// The entries, mutably, if this is an object.
    pub const fn as_object_mut(&mut self) -> Option<&mut RuntimeObject> {
        match self {
            Self::Object(entries) => Some(entries),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) | Self::Array(_) => None,
        }
    }

    /// The items, if this is an array.
    #[must_use]
    pub const fn as_array(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Array(items) => Some(items),
            Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) | Self::Object(_) => {
                None
            }
        }
    }

    /// Member `key`, if this is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object().and_then(|entries| entries.get(key))
    }

    /// The value at a JSON Pointer (RFC 6901: `/a/0/b`, `~1` for `/`, `~0`
    /// for `~`); `""` is the value itself.
    #[must_use]
    pub fn pointer(&self, pointer: &str) -> Option<&Self> {
        if pointer.is_empty() {
            return Some(self);
        }
        let path = pointer.strip_prefix('/')?;
        path.split('/').try_fold(self, |value, token| {
            let token = token.replace("~1", "/").replace("~0", "~");
            match value {
                Self::Object(entries) => entries.get(&token),
                Self::Array(items) => token
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| items.get(index)),
                Self::Null | Self::Bool(_) | Self::Number(_) | Self::String(_) => None,
            }
        })
    }

    /// The runtime value of an in-memory `serde_json::Value` (for example a
    /// `json!` literal): integers exactly, other numbers by `serde_json`'s
    /// own text. A construction convenience, not an ingress path.
    ///
    /// # Errors
    ///
    /// A number outside the decimal range.
    pub fn from_json(value: serde_json::Value) -> Result<Self, RuntimeValueError> {
        serde_json::from_value(value).map_err(|error| RuntimeValueError::InvalidJson {
            message: error.to_string(),
        })
    }
}

impl From<Decimal> for RuntimeValue {
    fn from(value: Decimal) -> Self {
        Self::Number(value)
    }
}

impl From<i64> for RuntimeValue {
    fn from(value: i64) -> Self {
        Self::Number(Decimal::from(value))
    }
}

impl From<bool> for RuntimeValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<&str> for RuntimeValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for RuntimeValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl Serialize for RuntimeValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(flag) => serializer.serialize_bool(*flag),
            Self::Number(number) => number.serialize(serializer),
            Self::String(text) => serializer.serialize_str(text),
            Self::Array(items) => items.serialize(serializer),
            Self::Object(entries) => entries.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RuntimeValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The raw text of the whole value, then an exact parse: numbers never
        // reach `serde_json`'s binary number types.
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::from_json_text(raw.get()).map_err(D::Error::custom)
    }
}

impl JsonSchema for Decimal {
    fn schema_name() -> Cow<'static, str> {
        "Decimal".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({ "type": "number" })
    }
}

/// The schema of a runtime value is the schema of any JSON value: the same
/// subschema the definition IR's [`JsonValue`] has, so schemas that switch
/// from IR values to runtime values are unchanged.
impl JsonSchema for RuntimeValue {
    fn schema_name() -> Cow<'static, str> {
        "RuntimeValue".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<JsonValue>()
    }
}

/// Decode typed data from a runtime value, exactly: through the value's
/// exact JSON text, so decimals reach the typed data's own decoders (and the
/// exact [`RuntimeValue`] fields inside it) without passing through `f64`.
///
/// # Errors
///
/// The data does not decode (the message names the reason).
pub fn decode<T: DeserializeOwned>(value: &RuntimeValue) -> Result<T, String> {
    let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Encode typed data as a runtime value, exactly (the inverse of [`decode`]).
///
/// # Errors
///
/// The data does not serialize, or holds a number outside the decimal range.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<RuntimeValue, String> {
    let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
    RuntimeValue::from_json_text(text).map_err(|error| error.to_string())
}

/// A member of a decoded record that may be absent or present (including
/// present as `null`): `#[serde(default)]` gives [`Self::Absent`] for a
/// missing member, and a present one decodes as a [`RuntimeValue`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Member {
    /// The member is not in the record.
    #[default]
    Absent,
    /// The member's value.
    Present(RuntimeValue),
}

impl<'de> Deserialize<'de> for Member {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RuntimeValue::deserialize(deserializer).map(Self::Present)
    }
}

/// Exact conversion of plain data to a [`RuntimeValue`], for building runtime
/// records in code ([`crate::runtime_object!`]) without going through
/// `serde_json`'s binary numbers.
pub trait ToRuntime {
    /// The value as an exact runtime value.
    fn to_runtime(&self) -> RuntimeValue;
}

impl ToRuntime for RuntimeValue {
    fn to_runtime(&self) -> RuntimeValue {
        self.clone()
    }
}

impl ToRuntime for RuntimeObject {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Object(self.clone())
    }
}

impl ToRuntime for Decimal {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Number(*self)
    }
}

impl ToRuntime for str {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::String(self.to_owned())
    }
}

impl ToRuntime for String {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::String(self.clone())
    }
}

impl ToRuntime for bool {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Bool(*self)
    }
}

impl ToRuntime for i64 {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Number(Decimal::from(*self))
    }
}

impl ToRuntime for u64 {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Number(Decimal::from(*self))
    }
}

impl ToRuntime for u32 {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Number(Decimal::from(*self))
    }
}

impl ToRuntime for usize {
    fn to_runtime(&self) -> RuntimeValue {
        // `usize` is at most 64 bits on every supported target.
        u64::try_from(*self).map_or(RuntimeValue::Null, |value| value.to_runtime())
    }
}

impl<T: ToRuntime + ?Sized> ToRuntime for &T {
    fn to_runtime(&self) -> RuntimeValue {
        (**self).to_runtime()
    }
}

impl<T: ToRuntime> ToRuntime for Option<T> {
    fn to_runtime(&self) -> RuntimeValue {
        self.as_ref()
            .map_or(RuntimeValue::Null, ToRuntime::to_runtime)
    }
}

impl<T: ToRuntime> ToRuntime for [T] {
    fn to_runtime(&self) -> RuntimeValue {
        RuntimeValue::Array(self.iter().map(ToRuntime::to_runtime).collect())
    }
}

impl<T: ToRuntime> ToRuntime for Vec<T> {
    fn to_runtime(&self) -> RuntimeValue {
        self.as_slice().to_runtime()
    }
}

/// A runtime object from `key => value` pairs, each value converted exactly
/// with [`runtime::ToRuntime`](crate::runtime::ToRuntime): the exact
/// counterpart of `serde_json::json!({...})` for runtime records.
///
/// ```
/// use norn_ir::runtime_object;
/// let seq: u64 = 7;
/// let record = runtime_object! { "type" => "timer.fired", "seq" => seq };
/// assert_eq!(record.get("seq").and_then(norn_ir::RuntimeValue::as_i64), Some(7));
/// ```
#[macro_export]
macro_rules! runtime_object {
    ($($key:expr => $value:expr),* $(,)?) => {
        $crate::RuntimeValue::Object($crate::RuntimeObject::from([
            $((::std::string::String::from($key), $crate::runtime::ToRuntime::to_runtime(&$value))),*
        ]))
    };
}
