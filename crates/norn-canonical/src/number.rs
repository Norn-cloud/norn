//! Representation-independent JSON number semantics (issue #2440).
//!
//! `serde_json` stores a [`serde_json::Number`] differently depending on a
//! Cargo feature that any crate in the build graph can switch on:
//!
//! - by default a number is eagerly parsed into `u64`, `i64`, or `f64`, and
//!   re-serialized from that binary value (`1.50` becomes `1.5`, `1e2`
//!   becomes `100.0`);
//! - with `arbitrary_precision` the source literal is kept verbatim, the
//!   `as_*` accessors parse it on demand, and self-describing deserialization
//!   (`deserialize_any`, `#[serde(untagged)]`, `#[serde(flatten)]`) sees a
//!   private single-entry map instead of a number.
//!
//! Resolver-v2 feature unification means kernel crates cannot choose which of
//! the two they get. [`JsonNumber`] classifies a number from its JSON literal
//! alone, with the default-profile rules, so every kernel gate, conversion,
//! and Legacy0 hash gives the same answer under either feature set. The
//! binary value comes from `serde_json`'s own primitive `f64` parser, which
//! is compiled identically in both profiles, so it reproduces the historical
//! default-profile value bit for bit (Legacy0 identities stay verifiable).
//!
//! Known residual: under `arbitrary_precision` `serde_json` reserves the
//! object key `$serde_json::private::Number`, so a literal object with only
//! that key decodes as a number. No Norn payload uses that key; exact-decimal
//! ingress (T3-NUM-1) removes the dependence altogether.
//!
//! This is a tolerance layer for the existing binary `f64`/`u64`/`i64` wire
//! types. The exact-decimal direction (ruling 2026-09-25 section 2, T3-NUM-1
//! #2770) is served by the Norn1 profile's exact decimal strings, which never
//! pass through `serde_json` numbers.

use crate::CanonicalJsonError;

/// The default-profile class of a JSON number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonNumberKind {
    /// An integer literal without fraction or exponent in `0..=u64::MAX`.
    NonNegativeInteger(u64),
    /// A negative, non-zero integer literal without fraction or exponent in
    /// `i64::MIN..0`.
    NegativeInteger(i64),
    /// Every other finite number.
    Float,
}

/// A JSON number classified by the `serde_json` default-profile rules, with
/// its binary64 value exactly as the historical default profile parses it.
///
/// One deliberate deviation: every negative-zero value (`-0`, `-0.0`, `-0e5`,
/// and literals such as `-1e-999` that underflow to `-0.0`)
/// collapses to the integer `0`. Under `arbitrary_precision` `serde_json`
/// eagerly parses the integer literal `-0` as `0` while keeping `-0.0`
/// verbatim; the default profile parses both as `-0.0`. Only collapsing all
/// of them to `0` gives one answer under both profiles, and it matches Norn1,
/// which already canonicalizes negative zero to `0`.
///
/// Compatibility note: this changes Legacy0 bytes for negative-zero inputs
/// (`-0.0` used to render as `-0.0`). No tracked fixture contains one, but an
/// external, pre-existing Legacy0 artifact with a negative zero would no
/// longer re-hash to its stored identity and needs compatibility handling
/// (verify against the stored hash with the pre-#2440 rendering) before it
/// is re-verified (OPEN-DECISIONS D37).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JsonNumber {
    kind: JsonNumberKind,
    value: f64,
}

impl JsonNumber {
    /// Classify one JSON number literal.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError::InvalidDecimal`] when `literal` is not a
    /// JSON number and [`CanonicalJsonError::NumberTooLarge`] when it
    /// overflows binary64.
    pub fn parse_literal(literal: &str) -> Result<Self, CanonicalJsonError> {
        let is_integer_literal =
            json_number_shape(literal).ok_or_else(|| CanonicalJsonError::InvalidDecimal {
                value: literal.to_owned(),
                offset: 0,
            })?;
        // `serde_json`'s primitive `f64` path (`deserialize_f64`) is the same
        // parser in both profiles; `str::parse` would differ from it on
        // literals the historical parser does not round correctly.
        let value = serde_json::from_str::<f64>(literal)
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| CanonicalJsonError::NumberTooLarge {
                value: literal.to_owned(),
                offset: 0,
            })?;
        if is_integer_literal {
            if let Ok(integer) = literal.parse::<u64>() {
                return Ok(Self {
                    kind: JsonNumberKind::NonNegativeInteger(integer),
                    value,
                });
            }
            if let Ok(integer) = literal.parse::<i64>() {
                return Ok(Self {
                    kind: JsonNumberKind::NegativeInteger(integer),
                    value,
                });
            }
        }
        Ok(Self::from_float(value))
    }

    /// Classify a [`serde_json::Number`] that was **constructed in memory**
    /// (`Number::from`, `Number::from_f64`, `json!`, `to_value`), with the
    /// same bits under both `serde_json` profiles.
    ///
    /// The stored value is taken as is: `as_u64`/`as_i64`/`as_f64` read the
    /// binary value in the default profile and parse the shortest-round-trip
    /// rendering exactly (Rust's correctly rounded parser) under
    /// `arbitrary_precision`. Use [`JsonNumber::from_parsed_serde`] for
    /// numbers that `serde_json` parsed from JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError::NumberTooLarge`] when the number is not a
    /// finite binary64 value.
    pub fn from_serde(number: &serde_json::Number) -> Result<Self, CanonicalJsonError> {
        if let Some(value) = number.as_u64() {
            return Ok(Self::from_non_negative(value));
        }
        if let Some(value) = number.as_i64() {
            return Ok(Self::from_negative(value));
        }
        number
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Self::from_float)
            .ok_or_else(|| CanonicalJsonError::NumberTooLarge {
                value: number.to_string(),
                offset: 0,
            })
    }

    /// Classify a [`serde_json::Number`] that `serde_json` **parsed from JSON
    /// text** (bytes or a string, including through a deserializer), with the
    /// value the historical default profile's parser produces under both
    /// profiles.
    ///
    /// In the default profile the number was already parsed once and its
    /// stored value is used. Under `arbitrary_precision` it is still the
    /// source literal, which is parsed exactly once with `serde_json`'s own
    /// primitive `f64` parser (compiled identically in both profiles).
    ///
    /// Do not use this for numbers constructed in memory: under
    /// `arbitrary_precision` their rendering would be re-parsed with the
    /// historical parser, which can move a near-halfway value by one bit
    /// (for example `Number::from_f64(9.1417e60)`). Use
    /// [`JsonNumber::from_serde`] for those.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`JsonNumber::parse_literal`].
    pub fn from_parsed_serde(number: &serde_json::Number) -> Result<Self, CanonicalJsonError> {
        if serde_json_keeps_literals() {
            Self::parse_literal(number.to_string().as_str())
        } else {
            Self::from_serde(number)
        }
    }

    fn from_non_negative(value: u64) -> Self {
        Self {
            kind: JsonNumberKind::NonNegativeInteger(value),
            value: u64_as_f64(value),
        }
    }

    fn from_i64(value: i64) -> Self {
        match u64::try_from(value) {
            Ok(value) => Self::from_non_negative(value),
            Err(_) => Self::from_negative(value),
        }
    }

    fn from_negative(value: i64) -> Self {
        if value == 0 {
            return Self::from_non_negative(0);
        }
        Self {
            kind: JsonNumberKind::NegativeInteger(value),
            value: i64_as_f64(value),
        }
    }

    fn from_float(value: f64) -> Self {
        if value == 0.0 && value.is_sign_negative() {
            Self::from_non_negative(0)
        } else {
            Self {
                kind: JsonNumberKind::Float,
                value,
            }
        }
    }

    /// Return the default-profile class.
    #[must_use]
    pub const fn kind(self) -> JsonNumberKind {
        self.kind
    }

    /// Return the binary64 value.
    #[must_use]
    pub const fn as_f64(self) -> f64 {
        self.value
    }

    /// Whether the literal was an integer admitted as `u64` or `i64`.
    #[must_use]
    pub const fn is_integer(self) -> bool {
        !matches!(self.kind, JsonNumberKind::Float)
    }

    /// Rebuild the `serde_json` number the default profile would have parsed.
    ///
    /// Serializing the result gives the same bytes under both feature sets.
    #[must_use]
    pub fn to_serde(self) -> serde_json::Number {
        match self.kind {
            JsonNumberKind::NonNegativeInteger(value) => serde_json::Number::from(value),
            JsonNumberKind::NegativeInteger(value) => serde_json::Number::from(value),
            // `from_f64` renders with the same formatter `serialize_f64`
            // uses, so both profiles serialize the result identically.
            // `value` is finite by construction, so `from_f64` cannot fail;
            // the zero fallback is unreachable and keeps this total.
            JsonNumberKind::Float => serde_json::Number::from_f64(self.value)
                .unwrap_or_else(|| serde_json::Number::from(0_u8)),
        }
    }
}

/// Whether `serde_json` keeps number literals verbatim, i.e. whether
/// `arbitrary_precision` is enabled somewhere in the build graph. Probed once
/// at runtime because a dependency's features are not visible to `cfg!`.
fn serde_json_keeps_literals() -> bool {
    static KEEPS_LITERALS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KEEPS_LITERALS.get_or_init(|| {
        serde_json::from_str::<serde_json::Number>("1.50")
            .is_ok_and(|number| number.to_string() == "1.50")
    })
}

fn u64_as_f64(value: u64) -> f64 {
    if let Ok(small) = u32::try_from(value) {
        return f64::from(small);
    }
    integer_as_f64_display(value)
}

fn i64_as_f64(value: i64) -> f64 {
    if let Ok(small) = i32::try_from(value) {
        return f64::from(small);
    }
    integer_as_f64_display(value)
}

/// Binary64 value of an integer outside `u32`/`i32` via `serde_json`'s
/// primitive parser over a stack buffer (zero heap allocation, bit-identical
/// to the historical default profile).
fn integer_as_f64_display(value: impl std::fmt::Display) -> f64 {
    let mut buf = [0_u8; 24];
    let mut cursor = std::io::Cursor::new(buf.as_mut_slice());
    if std::io::Write::write_fmt(&mut cursor, format_args!("{value}")).is_err() {
        return 0.0;
    }
    let len = usize::try_from(cursor.position()).unwrap_or(0);
    buf.get(..len)
        .and_then(|bytes| serde_json::from_slice::<f64>(bytes).ok())
        .unwrap_or(0.0)
}

/// Validate the RFC 8259 number grammar. Returns `Some(true)` for an integer
/// literal (no fraction, no exponent), `Some(false)` for any other valid
/// number, and `None` for invalid input.
fn json_number_shape(literal: &str) -> Option<bool> {
    let mut bytes = literal.bytes().peekable();
    bytes.next_if_eq(&b'-');
    match bytes.next()? {
        b'0' => {}
        b'1'..=b'9' => while bytes.next_if(u8::is_ascii_digit).is_some() {},
        _ => return None,
    }
    let mut is_integer = true;
    if bytes.next_if_eq(&b'.').is_some() {
        is_integer = false;
        bytes.next_if(u8::is_ascii_digit)?;
        while bytes.next_if(u8::is_ascii_digit).is_some() {}
    }
    if bytes.next_if(|byte| matches!(byte, b'e' | b'E')).is_some() {
        is_integer = false;
        bytes.next_if(|byte| matches!(byte, b'+' | b'-'));
        bytes.next_if(u8::is_ascii_digit)?;
        while bytes.next_if(u8::is_ascii_digit).is_some() {}
    }
    bytes.next().is_none().then_some(is_integer)
}

/// A caller-owned JSON tree that [`deserialize_json_tree`] can build.
///
/// This lets crates without a `serde_json` dependency (for example
/// `norn-ir`) decode generic JSON with the representation-independent
/// number rule instead of a derived `#[serde(untagged)]` visitor, which sees
/// numbers as a private map under `serde_json/arbitrary_precision`.
pub trait JsonTree: Sized {
    /// Build `null`.
    fn null() -> Self;
    /// Build a boolean.
    fn boolean(value: bool) -> Self;
    /// Build a number.
    fn number(value: JsonNumber) -> Self;
    /// Build a string.
    fn string(value: String) -> Self;
    /// Build an array.
    fn array(values: Vec<Self>) -> Self;
    /// Build an object from its entries. With duplicate keys the last entry
    /// wins, as in `serde_json`.
    fn object(entries: Vec<(String, Self)>) -> Self;
}

/// Deserialize self-describing JSON input into a [`JsonTree`] with the same
/// numbers whether or not `serde_json/arbitrary_precision` is enabled.
///
/// Numbers keep the origin `serde_json` reports for them:
///
/// - `visit_u64`/`visit_i64`/`visit_f64`: a binary value (every number in
///   the default profile; integers and in-memory `Value` numbers under
///   `arbitrary_precision`, whose `Number::deserialize_any` visits `f64` for
///   a shortest-round-trip rendering). The stored bits are kept.
/// - the private number map, which `arbitrary_precision` uses only for a
///   source literal it has not parsed (JSON text, or a `Value` still holding
///   such a literal). The literal is parsed once, as the historical default
///   profile parses text ([`JsonNumber::parse_literal`]).
///
/// So JSON text decodes like the default profile's text parse and an
/// in-memory `Value` (`serde_json::from_value`) keeps its constructed bits.
///
/// # Errors
///
/// Returns the deserializer's error for malformed input, or a custom error
/// for a number that overflows binary64.
pub fn deserialize_json_tree<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: JsonTree,
{
    deserialize_json_tree_at(deserializer, 0)
}

fn deserialize_json_tree_at<'de, D, T>(deserializer: D, depth: usize) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: JsonTree,
{
    if depth > crate::MAX_JSON_NESTING_DEPTH {
        return Err(serde::de::Error::custom(
            CanonicalJsonError::NestingTooDeep {
                depth,
                limit: crate::MAX_JSON_NESTING_DEPTH,
                offset: 0,
            },
        ));
    }
    deserializer.deserialize_any(JsonTreeVisitor {
        depth,
        marker: std::marker::PhantomData,
    })
}

/// The map key `serde_json` uses for an unparsed number literal under
/// `arbitrary_precision` (`serde_json::number::TOKEN`, not exported). It is
/// honoured only when that feature is actually on, so in the default profile
/// an object with this key stays an object; `tests::private_number_token_matches`
/// pins it against the running `serde_json`.
const SERDE_JSON_NUMBER_TOKEN: &str = "$serde_json::private::Number";

struct JsonTreeVisitor<T> {
    depth: usize,
    marker: std::marker::PhantomData<T>,
}

impl<'de, T: JsonTree> serde::de::Visitor<'de> for JsonTreeVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<T, E> {
        Ok(T::null())
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<T, E> {
        Ok(T::null())
    }

    fn visit_some<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
        deserialize_json_tree_at(deserializer, self.depth)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<T, E> {
        Ok(T::boolean(value))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<T, E> {
        Ok(T::number(JsonNumber::from_non_negative(value)))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<T, E> {
        Ok(T::number(JsonNumber::from_i64(value)))
    }

    fn visit_u128<E: serde::de::Error>(self, value: u128) -> Result<T, E> {
        JsonNumber::parse_literal(value.to_string().as_str())
            .map(T::number)
            .map_err(E::custom)
    }

    fn visit_i128<E: serde::de::Error>(self, value: i128) -> Result<T, E> {
        JsonNumber::parse_literal(value.to_string().as_str())
            .map(T::number)
            .map_err(E::custom)
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<T, E> {
        if value.is_finite() {
            Ok(T::number(JsonNumber::from_float(value)))
        } else {
            Err(E::custom(CanonicalJsonError::NonFiniteNumber {
                value: value.to_string(),
                offset: 0,
            }))
        }
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<T, E> {
        Ok(T::string(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<T, E> {
        Ok(T::string(value))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut access: A) -> Result<T, A::Error> {
        let mut values = Vec::new();
        let next = self.depth.saturating_add(1);
        while let Some(value) = access.next_element_seed(JsonTreeSeed::<T> {
            depth: next,
            marker: std::marker::PhantomData,
        })? {
            values.push(value);
        }
        Ok(T::array(values))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut access: A) -> Result<T, A::Error> {
        let mut entries = Vec::new();
        let next = self.depth.saturating_add(1);
        while let Some(key) = access.next_key::<String>()? {
            if entries.is_empty() && key == SERDE_JSON_NUMBER_TOKEN && serde_json_keeps_literals() {
                let literal = access.next_value::<String>()?;
                if access.next_key::<String>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "serde_json number literal map has extra entries",
                    ));
                }
                return JsonNumber::parse_literal(literal.as_str())
                    .map(T::number)
                    .map_err(serde::de::Error::custom);
            }
            let value = access.next_value_seed(JsonTreeSeed::<T> {
                depth: next,
                marker: std::marker::PhantomData,
            })?;
            entries.push((key, value));
        }
        Ok(T::object(entries))
    }
}

struct JsonTreeSeed<T> {
    depth: usize,
    marker: std::marker::PhantomData<T>,
}

impl<'de, T: JsonTree> serde::de::DeserializeSeed<'de> for JsonTreeSeed<T> {
    type Value = T;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
        deserialize_json_tree_at(deserializer, self.depth)
    }
}

/// Convert an **in-memory** `serde_json::Value` (built with `json!`,
/// `to_value`, `Number::from_f64`, ...) into a [`JsonTree`], keeping every
/// number's stored value ([`JsonNumber::from_serde`]) under both
/// `serde_json` profiles. Use [`deserialize_json_tree`] for JSON text.
///
/// # Errors
///
/// Returns [`CanonicalJsonError::NumberTooLarge`] for a number that is not a
/// finite binary64 value, or [`CanonicalJsonError::NestingTooDeep`] if nesting
/// exceeds [`crate::MAX_JSON_NESTING_DEPTH`].
pub fn json_tree_from_value<T: JsonTree>(
    value: serde_json::Value,
) -> Result<T, CanonicalJsonError> {
    json_tree_from(value, NumberOrigin::Constructed, 0)
}

/// Where the numbers of a `serde_json` value came from; see
/// [`JsonNumber::from_serde`] and [`JsonNumber::from_parsed_serde`].
#[derive(Clone, Copy)]
pub(crate) enum NumberOrigin {
    /// Parsed from JSON text.
    Parsed,
    /// Built in memory.
    Constructed,
}

impl NumberOrigin {
    pub(crate) fn classify(
        self,
        number: &serde_json::Number,
    ) -> Result<JsonNumber, CanonicalJsonError> {
        match self {
            Self::Parsed => JsonNumber::from_parsed_serde(number),
            Self::Constructed => JsonNumber::from_serde(number),
        }
    }
}

fn json_tree_from<T: JsonTree>(
    value: serde_json::Value,
    origin: NumberOrigin,
    depth: usize,
) -> Result<T, CanonicalJsonError> {
    if depth > crate::MAX_JSON_NESTING_DEPTH {
        return Err(CanonicalJsonError::NestingTooDeep {
            depth,
            limit: crate::MAX_JSON_NESTING_DEPTH,
            offset: 0,
        });
    }
    Ok(match value {
        serde_json::Value::Null => T::null(),
        serde_json::Value::Bool(value) => T::boolean(value),
        serde_json::Value::Number(number) => T::number(origin.classify(&number)?),
        serde_json::Value::String(value) => T::string(value),
        serde_json::Value::Array(values) => {
            let child_depth = if values.is_empty() {
                depth
            } else {
                crate::next_depth(depth, 0)?
            };
            T::array(
                values
                    .into_iter()
                    .map(|value| json_tree_from(value, origin, child_depth))
                    .collect::<Result<_, _>>()?,
            )
        }
        serde_json::Value::Object(entries) => {
            let child_depth = if entries.is_empty() {
                depth
            } else {
                crate::next_depth(depth, 0)?
            };
            T::object(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        json_tree_from(value, origin, child_depth).map(|value| (key, value))
                    })
                    .collect::<Result<_, _>>()?,
            )
        }
    })
}

pub(crate) fn legacy_canonicalize(
    mut value: serde_json::Value,
    origin: NumberOrigin,
) -> Result<Vec<u8>, CanonicalJsonError> {
    prune_legacy_optional_nulls(&mut value);
    let value = sort_legacy_json_value(value, origin, 0)?;
    serde_json::to_vec(&value).map_err(|error| CanonicalJsonError::LegacySerialize {
        message: error.to_string(),
    })
}

fn prune_legacy_optional_nulls(value: &mut serde_json::Value) {
    if let Some(scope) = value
        .get_mut("scope")
        .and_then(serde_json::Value::as_object_mut)
    {
        scope.retain(|_, item| !item.is_null());
    }

    if let Some(provenance) = value
        .get_mut("provenance")
        .and_then(serde_json::Value::as_object_mut)
    {
        provenance.retain(|_, item| !item.is_null());
    }

    if let Some(dependencies) = value
        .get_mut("dependencies")
        .and_then(serde_json::Value::as_array_mut)
    {
        for dependency in dependencies {
            if let Some(dependency) = dependency.as_object_mut() {
                dependency.retain(|_, item| !item.is_null());
            }
        }
    }
}

/// Sort object keys recursively and rewrite every number to the form the
/// `serde_json` default profile produces, so Legacy0 bytes do not depend on
/// whether `serde_json/arbitrary_precision` is enabled (issue #2440).
fn sort_legacy_json_value(
    value: serde_json::Value,
    origin: NumberOrigin,
    depth: usize,
) -> Result<serde_json::Value, CanonicalJsonError> {
    if depth > crate::MAX_JSON_NESTING_DEPTH {
        return Err(CanonicalJsonError::NestingTooDeep {
            depth,
            limit: crate::MAX_JSON_NESTING_DEPTH,
            offset: 0,
        });
    }
    Ok(match value {
        serde_json::Value::Array(values) => {
            let child_depth = if values.is_empty() {
                depth
            } else {
                crate::next_depth(depth, 0)?
            };
            serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|value| sort_legacy_json_value(value, origin, child_depth))
                    .collect::<Result<_, _>>()?,
            )
        }
        serde_json::Value::Object(object) => {
            let child_depth = if object.is_empty() {
                depth
            } else {
                crate::next_depth(depth, 0)?
            };
            let mut entries = object
                .into_iter()
                .map(|(key, value)| {
                    sort_legacy_json_value(value, origin, child_depth).map(|value| (key, value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));

            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, value);
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Number(number) => serde_json::Value::Number(
            origin
                .classify(&number)
                .map_err(|error| CanonicalJsonError::LegacyParse {
                    message: error.to_string(),
                })?
                .to_serde(),
        ),
        other @ (serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::String(_)) => other,
    })
}

#[cfg(test)]
mod tests {
    use super::{JsonNumber, JsonNumberKind};

    #[derive(Debug, PartialEq)]
    enum Tree {
        Null,
        Bool(bool),
        Number(u64),
        String(String),
        Array(Vec<Tree>),
        Object(Vec<(String, Tree)>),
    }

    impl super::JsonTree for Tree {
        fn null() -> Self {
            Self::Null
        }
        fn boolean(value: bool) -> Self {
            Self::Bool(value)
        }
        fn number(value: JsonNumber) -> Self {
            Self::Number(value.as_f64().to_bits())
        }
        fn string(value: String) -> Self {
            Self::String(value)
        }
        fn array(values: Vec<Self>) -> Self {
            Self::Array(values)
        }
        fn object(entries: Vec<(String, Self)>) -> Self {
            Self::Object(entries)
        }
    }

    fn bits(text: &str) -> Tree {
        Tree::Number(text.parse::<f64>().expect("float").to_bits())
    }

    /// #2440 review: text keeps the historical default-profile parse, an
    /// in-memory `Value` keeps its constructed bits, under both profiles.
    #[test]
    fn json_tree_keeps_number_origin_under_both_profiles() {
        let text = r#"[1.50,914.17e58,7,-3,{"k":0.1}]"#;
        let from_text: Tree =
            super::deserialize_json_tree(&mut serde_json::Deserializer::from_str(text))
                .expect("text");
        assert_eq!(
            from_text,
            Tree::Array(vec![
                bits("1.5"),
                bits("9.141700000000001e60"),
                bits("7"),
                bits("-3"),
                Tree::Object(vec![("k".to_owned(), bits("0.1"))]),
            ])
        );

        let constructed = serde_json::json!([9.1417e60, 7_841_115_271_120_439.0, { "k": -3_618_697_969_859_606.0 }]);
        let from_value: Tree = super::deserialize_json_tree(constructed.clone()).expect("value");
        let converted: Tree = super::json_tree_from_value(constructed).expect("value");
        let expected = Tree::Array(vec![
            bits("9.1417e60"),
            bits("7841115271120439"),
            Tree::Object(vec![("k".to_owned(), bits("-3618697969859606"))]),
        ]);
        assert_eq!(from_value, expected);
        assert_eq!(converted, expected);

        // The private token is honoured only when serde_json itself uses it.
        let token = r#"{"$serde_json::private::Number":"1"}"#;
        let decoded: Result<Tree, _> =
            super::deserialize_json_tree(&mut serde_json::Deserializer::from_str(token));
        if super::serde_json_keeps_literals() {
            assert_eq!(decoded.expect("literal"), bits("1"));
        } else {
            assert_eq!(
                decoded.expect("object"),
                Tree::Object(vec![(
                    super::SERDE_JSON_NUMBER_TOKEN.to_owned(),
                    Tree::String("1".to_owned())
                )])
            );
        }
    }

    #[test]
    fn classifies_literals_with_default_profile_rules() {
        let cases: &[(&str, JsonNumberKind, f64)] = &[
            ("0", JsonNumberKind::NonNegativeInteger(0), 0.0),
            ("3", JsonNumberKind::NonNegativeInteger(3), 3.0),
            (
                "18446744073709551615",
                JsonNumberKind::NonNegativeInteger(u64::MAX),
                2.0_f64.powi(64),
            ),
            ("-5", JsonNumberKind::NegativeInteger(-5), -5.0),
            (
                "-9223372036854775808",
                JsonNumberKind::NegativeInteger(i64::MIN),
                -(2.0_f64.powi(63)),
            ),
            ("-0", JsonNumberKind::NonNegativeInteger(0), 0.0),
            ("-0.0", JsonNumberKind::NonNegativeInteger(0), 0.0),
            ("-0e5", JsonNumberKind::NonNegativeInteger(0), 0.0),
            ("0.0", JsonNumberKind::Float, 0.0),
            ("1.0", JsonNumberKind::Float, 1.0),
            ("1.50", JsonNumberKind::Float, 1.5),
            ("1e2", JsonNumberKind::Float, 100.0),
            ("1E+2", JsonNumberKind::Float, 100.0),
            (
                "18446744073709551616",
                JsonNumberKind::Float,
                2.0_f64.powi(64),
            ),
            ("1e-99999999999999999999", JsonNumberKind::Float, 0.0),
            // The default profile already parses an underflowing negative
            // literal to `-0.0`, indistinguishable from `-0`, so it collapses
            // too; only value-based collapse is profile-independent.
            ("-1e-999", JsonNumberKind::NonNegativeInteger(0), 0.0),
        ];
        for (literal, kind, value) in cases {
            let actual = JsonNumber::parse_literal(literal).expect(literal);
            assert_eq!(actual.kind(), *kind, "{literal}");
            assert_eq!(actual.as_f64().to_bits(), value.to_bits(), "{literal}");
        }
    }

    #[test]
    fn rejects_non_json_and_overflowing_literals() {
        for literal in [
            "", "-", "+1", "01", "1.", ".5", "1e", "1e+", "NaN", "inf", "Infinity", "0x10", " 1",
            "1e400", "-1e400",
        ] {
            assert!(JsonNumber::parse_literal(literal).is_err(), "{literal:?}");
        }
    }

    #[test]
    fn serde_round_trip_is_feature_independent() {
        for (source, expected) in [
            ("1.50", "1.5"),
            ("1e2", "100.0"),
            ("-0", "0"),
            ("-0.0", "0"),
            ("0.0", "0.0"),
            ("-1e-999", "0"),
            ("914.17e58", "9.141700000000001e+60"),
            ("10", "10"),
            ("-7", "-7"),
            ("9007199254740993", "9007199254740993"),
            ("1e20", "1e+20"),
        ] {
            let parsed = serde_json::from_str::<serde_json::Number>(source).expect(source);
            let class = JsonNumber::from_parsed_serde(&parsed).expect(source);
            // Serialized bytes, not `Display`: Legacy0 hashes `serde_json::to_vec`.
            let serialized = serde_json::to_string(&class.to_serde()).expect(source);
            assert_eq!(serialized, expected, "{source}");
        }
    }
}
