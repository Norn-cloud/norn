//! Versioned canonical JSON and hashing profiles for Norn artifact identities.
//!
//! [`HashProfile::Norn1`] is the new identity format. [`HashProfile::Legacy0`]
//! is intentionally a compatibility adapter for the JSON hashing that existed
//! before this crate: compact Serde JSON with recursively ordered object keys,
//! the historical artifact null pruning, and an unprefixed SHA-256 digest.

// Deterministic core policy (#3299); host adapters retain their own arithmetic/collections.
#![deny(
    clippy::float_arithmetic,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::disallowed_types
)]

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub mod number;

use number::NumberOrigin;
pub use number::{
    JsonNumber, JsonNumberKind, JsonTree, deserialize_json_tree, json_tree_from_value,
};

/// The Norn1 domain separator, including a trailing NUL as part of the tag.
///
/// The tag is deliberately a stable, human-readable byte string with an
/// explicit profile name and version. It is prepended verbatim to Norn1
/// canonical bytes before SHA-256; it is not used by Legacy0.
pub const NORN1_DOMAIN_TAG: &[u8] = b"norn.canonical.json/norn1\0";

/// Maximum size of one normalized Norn1 number, in UTF-8 bytes.
///
/// This keeps an otherwise valid, adversarial exponent from forcing an
/// unbounded allocation during exact decimal expansion. It is part of the
/// Norn1 ingress profile; callers may impose a smaller document limit.
pub const MAX_CANONICAL_NUMBER_BYTES: usize = 1_048_576;

/// Maximum nesting depth for Norn1 JSON values.
///
/// The root value starts at depth zero. The limit is deliberately broader than
/// the kernel's 32-level schema guards while still bounding recursive parser
/// and serializer stack use for generic JSON.
pub const MAX_JSON_NESTING_DEPTH: usize = 256;

/// The registered hash profiles, in stable identifier order.
pub const HASH_PROFILE_REGISTRY: &[HashProfile] = &[HashProfile::Legacy0, HashProfile::Norn1];

/// Versioned identity and canonicalization profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashProfile {
    /// The pre-registry JSON artifact hash behavior.
    Legacy0,
    /// The first versioned Norn canonical JSON profile.
    Norn1,
}

impl HashProfile {
    /// Return the wire identifier for this profile.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Legacy0 => "legacy0",
            Self::Norn1 => "norn1",
        }
    }

    /// Return the numeric profile version.
    #[must_use]
    pub const fn version(self) -> u8 {
        match self {
            Self::Legacy0 => 0,
            Self::Norn1 => 1,
        }
    }

    /// Return the hash domain tag, if this profile has one.
    #[must_use]
    pub const fn domain_tag(self) -> Option<&'static [u8]> {
        match self {
            Self::Legacy0 => None,
            Self::Norn1 => Some(NORN1_DOMAIN_TAG),
        }
    }

    /// Canonicalize one JSON document according to this profile.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] when the input is invalid for the
    /// selected profile.
    pub fn canonicalize_json(self, input: impl AsRef<[u8]>) -> Result<Vec<u8>, CanonicalJsonError> {
        match self {
            Self::Legacy0 => legacy_canonicalize_json(input.as_ref()),
            Self::Norn1 => canonicalize_json(input.as_ref()),
        }
    }

    /// Return the lower-case hexadecimal SHA-256 identity of one JSON
    /// document according to this profile.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] when the input cannot be canonicalized.
    pub fn hash_json(self, input: impl AsRef<[u8]>) -> Result<String, CanonicalJsonError> {
        let bytes = self.canonicalize_json(input)?;
        Ok(self.hash_canonical_bytes(bytes.as_slice()))
    }

    /// Return the raw SHA-256 digest of one JSON document according to this
    /// profile.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] when the input cannot be canonicalized.
    pub fn digest_json(self, input: impl AsRef<[u8]>) -> Result<[u8; 32], CanonicalJsonError> {
        let bytes = self.canonicalize_json(input)?;
        Ok(self.digest_canonical_bytes(bytes.as_slice()))
    }

    /// Hash bytes that have already been canonicalized for this profile.
    ///
    /// This method does not parse or validate `bytes`. Callers must obtain the
    /// bytes from [`HashProfile::canonicalize_json`] or [`serialize`]. Norn1
    /// hashes `NORN1_DOMAIN_TAG || bytes`; Legacy0 hashes `bytes` exactly.
    #[must_use]
    pub fn hash_canonical_bytes(self, bytes: &[u8]) -> String {
        let digest = self.digest_canonical_bytes(bytes);
        lower_hex(digest)
    }

    /// Return the digest for bytes that have already been canonicalized for
    /// this profile.
    #[must_use]
    pub fn digest_canonical_bytes(self, bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        if let Some(domain_tag) = self.domain_tag() {
            hasher.update(domain_tag);
        }
        hasher.update(bytes);
        hasher.finalize().into()
    }
}

impl Display for HashProfile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.identifier())
    }
}

/// Compatibility hasher for the structured `norn-ir` Legacy0 commitments.
///
/// Each part is encoded as `SHA-256(label) || SHA-256(value)` and the final
/// digest is unprefixed. This is the byte-for-byte scheme used by the private
/// `StableHash` helper in `norn-ir`'s quantity-ledger boundary code. It is
/// separate from Norn1 canonical JSON and exists so those existing commitments
/// can be read without recomputation under a new profile.
pub struct LegacyStableHasher {
    hasher: Sha256,
}

impl LegacyStableHasher {
    /// Start a Legacy0 structured commitment with its domain value.
    #[must_use]
    pub fn new(domain: &[u8]) -> Self {
        let mut hasher = Self {
            hasher: Sha256::new(),
        };
        hasher.part(b"domain", domain);
        hasher
    }

    /// Add a marker part using the historical `marker` label.
    pub fn marker(&mut self, marker: &[u8]) {
        self.part(b"marker", marker);
    }

    /// Add a UTF-8 text part with its caller-supplied label.
    pub fn text(&mut self, label: &[u8], value: &str) {
        self.part(label, value.as_bytes());
    }

    /// Add one byte with its caller-supplied label.
    pub fn byte(&mut self, label: &[u8], value: u8) {
        self.part(label, &[value]);
    }

    /// Add a big-endian `u64` with its caller-supplied label.
    pub fn u64(&mut self, label: &[u8], value: u64) {
        self.part(label, value.to_be_bytes().as_slice());
    }

    /// Finish the commitment as lower-case hexadecimal SHA-256.
    #[must_use]
    pub fn finish(self) -> String {
        lower_hex(self.hasher.finalize().into())
    }

    fn part(&mut self, label: &[u8], value: &[u8]) {
        self.hasher.update(Sha256::digest(label));
        self.hasher.update(Sha256::digest(value));
    }
}

/// Return the complete profile registry.
#[must_use]
pub const fn profile_registry() -> &'static [HashProfile] {
    HASH_PROFILE_REGISTRY
}

/// A parsed Norn1 JSON value.
///
/// Numbers are kept as normalized exact decimal strings rather than floating
/// point values. Object keys and string values are NFC-normalized when a value
/// is parsed or serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    /// The JSON null value.
    Null,
    /// A JSON boolean.
    Bool(bool),
    /// An exact decimal JSON number in Norn1 form.
    Number(String),
    /// A Unicode JSON string.
    String(String),
    /// An ordered JSON array.
    Array(Vec<JsonValue>),
    /// A JSON object. Keys are unique in this representation.
    Object(BTreeMap<String, JsonValue>),
}

/// Errors returned while parsing, canonicalizing, or hashing JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalJsonError {
    /// The input was not valid UTF-8.
    InvalidUtf8 {
        /// The first byte that could not be decoded.
        offset: usize,
    },
    /// The input was not valid JSON.
    InvalidJson {
        /// The byte offset at which parsing stopped.
        offset: usize,
        /// A stable, human-readable error description.
        message: String,
    },
    /// An object contained duplicate keys, including keys that collide after
    /// NFC normalization.
    DuplicateKey {
        /// The normalized duplicate key.
        key: String,
        /// The byte offset of the duplicate key.
        offset: usize,
    },
    /// A non-finite number was supplied to the Norn1 serializer.
    NonFiniteNumber {
        /// The supplied spelling.
        value: String,
        /// The byte offset of the value when parsing source JSON.
        offset: usize,
    },
    /// A number did not follow the JSON decimal grammar.
    InvalidDecimal {
        /// The supplied spelling.
        value: String,
        /// The byte offset of the value when parsing source JSON.
        offset: usize,
    },
    /// A decimal exponent or expansion could not be represented safely.
    NumberTooLarge {
        /// The supplied spelling.
        value: String,
        /// The byte offset of the value when parsing source JSON.
        offset: usize,
    },
    /// A JSON value exceeded the profile's recursive nesting limit.
    NestingTooDeep {
        /// The attempted depth, counting the root value as zero.
        depth: usize,
        /// The maximum permitted depth.
        limit: usize,
        /// The byte offset of the attempted value, or zero for programmatic values.
        offset: usize,
    },
    /// Legacy0 could not parse its input with Serde JSON.
    LegacyParse {
        /// The parser's error description.
        message: String,
    },
    /// Legacy0 could not serialize its normalized Serde value.
    LegacySerialize {
        /// The serializer's error description.
        message: String,
    },
    /// A typed value could not be serialized to JSON before canonicalization
    /// ([`canonical_json_of`]).
    Serialize {
        /// The serializer's error description.
        message: String,
    },
}

impl Display for CanonicalJsonError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUtf8 { offset } => {
                write!(formatter, "invalid UTF-8 at byte offset {offset}")
            }
            Self::InvalidJson { offset, message } => {
                write!(formatter, "invalid JSON at byte offset {offset}: {message}")
            }
            Self::DuplicateKey { key, offset } => {
                write!(
                    formatter,
                    "duplicate JSON key {key:?} at byte offset {offset}"
                )
            }
            Self::NonFiniteNumber { value, offset } => {
                write!(
                    formatter,
                    "non-finite number {value:?} at byte offset {offset}"
                )
            }
            Self::InvalidDecimal { value, offset } => {
                write!(
                    formatter,
                    "invalid decimal {value:?} at byte offset {offset}"
                )
            }
            Self::NumberTooLarge { value, offset } => {
                write!(
                    formatter,
                    "decimal {value:?} is too large at byte offset {offset}"
                )
            }
            Self::NestingTooDeep {
                depth,
                limit,
                offset,
            } => {
                write!(
                    formatter,
                    "JSON nesting depth {depth} exceeds limit {limit} at byte offset {offset}"
                )
            }
            Self::LegacyParse { message } => {
                write!(formatter, "Legacy0 JSON parse failed: {message}")
            }
            Self::LegacySerialize { message } => {
                write!(formatter, "Legacy0 JSON serialization failed: {message}")
            }
            Self::Serialize { message } => {
                write!(formatter, "JSON serialization failed: {message}")
            }
        }
    }
}

impl std::error::Error for CanonicalJsonError {}

/// Parse and normalize one Norn1 JSON document.
///
/// # Errors
///
/// Returns [`CanonicalJsonError`] for invalid UTF-8, invalid JSON, duplicate
/// keys, non-finite numbers, or an unsupported exact decimal.
pub fn parse(input: impl AsRef<[u8]>) -> Result<JsonValue, CanonicalJsonError> {
    let bytes = input.as_ref();
    if let Err(error) = str::from_utf8(bytes) {
        return Err(CanonicalJsonError::InvalidUtf8 {
            offset: error.valid_up_to(),
        });
    }

    Parser::new(bytes).parse_document()
}

/// Parse one JSON document like [`parse`], with numbers normalized to exact
/// Norn1 decimal strings, but strings and object keys kept verbatim (no NFC
/// normalization): the exact ingress parse for runtime values (#2770), which
/// must not change user text. Hash the result with [`serialize`], which
/// normalizes.
///
/// # Errors
///
/// Returns the same errors as [`parse`].
pub fn parse_json_verbatim(input: impl AsRef<[u8]>) -> Result<JsonValue, CanonicalJsonError> {
    let bytes = input.as_ref();
    if let Err(error) = str::from_utf8(bytes) {
        return Err(CanonicalJsonError::InvalidUtf8 {
            offset: error.valid_up_to(),
        });
    }
    let mut parser = Parser::new(bytes);
    parser.normalize_strings = false;
    parser.parse_document()
}

/// Alias for [`parse`].
///
/// # Errors
///
/// Returns the same errors as [`parse`].
pub fn parse_json(input: impl AsRef<[u8]>) -> Result<JsonValue, CanonicalJsonError> {
    parse(input)
}

/// Serialize a parsed or programmatically constructed value using Norn1.
///
/// # Errors
///
/// Returns [`CanonicalJsonError`] when a programmatically supplied number is
/// not a valid finite JSON decimal or when normalized object keys collide.
pub fn serialize(value: &JsonValue) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut output = String::new();
    write_value(value, &mut output, 0)?;
    Ok(output.into_bytes())
}

/// Alias for [`serialize`].
///
/// # Errors
///
/// Returns the same errors as [`serialize`].
pub fn serialize_json(value: &JsonValue) -> Result<Vec<u8>, CanonicalJsonError> {
    serialize(value)
}

/// Parse and serialize one document using Norn1.
///
/// # Errors
///
/// Returns the same errors as [`parse`] and [`serialize`].
pub fn canonicalize_json(input: impl AsRef<[u8]>) -> Result<Vec<u8>, CanonicalJsonError> {
    let value = parse(input)?;
    serialize(&value)
}

/// Norn1 canonical bytes of a typed value.
///
/// The value is encoded with Serde JSON and then parsed and re-serialized under
/// Norn1 (sorted keys, NFC strings, exact numbers), so the bytes never depend
/// on `serde_json`'s map backend. `preserve_order` (insertion order) is
/// feature-unified into any build that links `cedar-policy` (#2882). This is
/// the only sanctioned way to turn Serde data into bytes that are hashed; the
/// skeleton guard rejects raw `serde_json::to_vec`/`to_writer` outside this
/// crate.
///
/// # Errors
///
/// [`CanonicalJsonError::Serialize`] if Serde JSON cannot encode the value,
/// or any Norn1 canonicalization error (for example a non-finite number).
pub fn canonical_json_of<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, CanonicalJsonError> {
    let bytes = serde_json::to_vec(value).map_err(|error| CanonicalJsonError::Serialize {
        message: error.to_string(),
    })?;
    canonicalize_json(bytes)
}

/// Lower-case hexadecimal SHA-256 of `domain || canonical_json_of(value)`.
///
/// `domain` is the caller's own separator (for example
/// `b"norn.action-effect.v1\0"`); the Norn1 profile tag is not added, so the
/// separator alone names the commitment.
///
/// # Errors
///
/// As [`canonical_json_of`].
pub fn domain_hash_hex<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<String, CanonicalJsonError> {
    let bytes = canonical_json_of(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes.as_slice());
    Ok(lower_hex(hasher.finalize().into()))
}

/// Hash one JSON document using an explicit profile.
///
/// # Errors
///
/// Returns [`CanonicalJsonError`] when the selected profile cannot
/// canonicalize the input.
pub fn hash_json(
    profile: HashProfile,
    input: impl AsRef<[u8]>,
) -> Result<String, CanonicalJsonError> {
    profile.hash_json(input)
}

/// Apply the historical artifact null pruning, key ordering, and compact
/// Serde JSON serialization used by Legacy0.
///
/// # Errors
///
/// Returns [`CanonicalJsonError::LegacyParse`] or
/// [`CanonicalJsonError::LegacySerialize`] when Serde JSON rejects the input
/// or cannot serialize the normalized value.
pub fn legacy_canonicalize_json(input: &[u8]) -> Result<Vec<u8>, CanonicalJsonError> {
    let value = serde_json::from_slice::<serde_json::Value>(input).map_err(|error| {
        CanonicalJsonError::LegacyParse {
            message: error.to_string(),
        }
    })?;
    legacy_canonicalize(&value, NumberOrigin::Parsed)
}

/// Apply the historical Legacy0 transformation to an in-memory Serde JSON
/// value.
///
/// Numbers keep the value they were constructed with
/// ([`JsonNumber::from_serde`]), so a value built with `Number::from_f64`,
/// `json!`, or `serde_json::to_value` hashes identically whether or not
/// `serde_json/arbitrary_precision` is enabled. For JSON text use
/// [`legacy_canonicalize_json`], which reproduces the historical parse. A
/// value produced by `serde_json::from_slice` under `arbitrary_precision`
/// and passed here is treated as constructed and can differ by one bit from
/// the historical parse on near-halfway literals.
///
/// # Errors
///
/// Returns [`CanonicalJsonError::LegacyParse`] if the value contains a
/// number that is not a finite binary64 value.
pub fn legacy_canonicalize_value(value: &serde_json::Value) -> Result<Vec<u8>, CanonicalJsonError> {
    legacy_canonicalize(value, NumberOrigin::Constructed)
}

fn legacy_canonicalize(
    value: &serde_json::Value,
    origin: NumberOrigin,
) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut value = value.clone();
    prune_legacy_optional_nulls(&mut value);
    let value = sort_legacy_json_value(value, origin)?;
    serde_json::to_vec(&value).map_err(|error| CanonicalJsonError::LegacySerialize {
        message: error.to_string(),
    })
}

/// Hash an already parsed Serde JSON value using Legacy0.
///
/// # Errors
///
/// Returns the same serialization errors as [`legacy_canonicalize_value`].
pub fn legacy_hash_value(value: &serde_json::Value) -> Result<String, CanonicalJsonError> {
    let bytes = legacy_canonicalize_value(value)?;
    Ok(HashProfile::Legacy0.hash_canonical_bytes(bytes.as_slice()))
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
) -> Result<serde_json::Value, CanonicalJsonError> {
    Ok(match value {
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| sort_legacy_json_value(value, origin))
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(object) => {
            let mut entries = object
                .into_iter()
                .map(|(key, value)| sort_legacy_json_value(value, origin).map(|value| (key, value)))
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

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    /// Whether strings and keys are NFC-normalized (Norn1) or kept verbatim.
    normalize_strings: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            position: 0,
            normalize_strings: true,
        }
    }

    fn parse_document(mut self) -> Result<JsonValue, CanonicalJsonError> {
        self.skip_whitespace();
        if self.peek().is_none() {
            return Err(self.invalid("a JSON value is required"));
        }
        let value = self.parse_value(0)?;
        self.skip_whitespace();
        if self.peek().is_some() {
            return Err(self.invalid("trailing characters"));
        }
        Ok(value)
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, CanonicalJsonError> {
        self.skip_whitespace();
        if depth > MAX_JSON_NESTING_DEPTH {
            return Err(CanonicalJsonError::NestingTooDeep {
                depth,
                limit: MAX_JSON_NESTING_DEPTH,
                offset: self.position,
            });
        }
        if self.starts_with(b"NaN")
            || self.starts_with(b"+NaN")
            || self.starts_with(b"-NaN")
            || self.starts_with(b"Infinity")
            || self.starts_with(b"-Infinity")
            || self.starts_with(b"+Infinity")
        {
            let start = self.position;
            let value = self.take_token();
            return Err(CanonicalJsonError::NonFiniteNumber {
                value,
                offset: start,
            });
        }

        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(JsonValue::Null)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'"') => self.parse_string().map(JsonValue::String),
            Some(b'[') => self.parse_array(depth),
            Some(b'{') => self.parse_object(depth),
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(JsonValue::Number),
            Some(_) | None => Err(self.invalid("expected a JSON value")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, CanonicalJsonError> {
        self.expect_byte(b'[')?;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.expect_byte(b']')?;
            return Ok(JsonValue::Array(values));
        }

        loop {
            let child_depth = next_depth(depth, self.position)?;
            values.push(self.parse_value(child_depth)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.expect_byte(b',')?;
                    self.skip_whitespace();
                }
                Some(b']') => {
                    self.expect_byte(b']')?;
                    return Ok(JsonValue::Array(values));
                }
                Some(_) => return Err(self.invalid("expected ',' or ']' in array")),
                None => return Err(self.invalid("unterminated array")),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, CanonicalJsonError> {
        self.expect_byte(b'{')?;
        self.skip_whitespace();
        let mut values = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.expect_byte(b'}')?;
            return Ok(JsonValue::Object(values));
        }

        loop {
            self.skip_whitespace();
            let key_offset = self.position;
            if self.peek() != Some(b'"') {
                return Err(self.invalid("object keys must be strings"));
            }
            let key = self.parse_string()?;
            if values.contains_key(&key) {
                return Err(CanonicalJsonError::DuplicateKey {
                    key,
                    offset: key_offset,
                });
            }
            self.skip_whitespace();
            self.expect_byte(b':')?;
            let child_depth = next_depth(depth, self.position)?;
            let value = self.parse_value(child_depth)?;
            values.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.expect_byte(b',')?;
                    self.skip_whitespace();
                }
                Some(b'}') => {
                    self.expect_byte(b'}')?;
                    return Ok(JsonValue::Object(values));
                }
                Some(_) => return Err(self.invalid("expected ',' or '}' in object")),
                None => return Err(self.invalid("unterminated object")),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, CanonicalJsonError> {
        self.expect_byte(b'"')?;
        let mut value = String::new();
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.expect_byte(b'"')?;
                    return Ok(if self.normalize_strings {
                        normalize_unicode(&value)
                    } else {
                        value
                    });
                }
                Some(b'\\') => {
                    self.expect_byte(b'\\')?;
                    self.parse_escape(&mut value)?;
                }
                Some(byte) if byte < 0x20 => {
                    return Err(self.invalid("unescaped control character in string"));
                }
                Some(byte) if byte < 0x80 => {
                    self.expect_byte(byte)?;
                    value.push(char::from(byte));
                }
                Some(byte) => {
                    // The entry point validates the document once. Rechecking
                    // its entire remaining suffix for each non-ASCII scalar
                    // makes multilingual strings quadratic in document size.
                    let width = match byte {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(self.invalid("invalid UTF-8 while reading string")),
                    };
                    let end = self
                        .position
                        .checked_add(width)
                        .ok_or_else(|| self.invalid("string position overflow"))?;
                    let encoded = self
                        .input
                        .get(self.position..end)
                        .ok_or_else(|| self.invalid("invalid UTF-8 while reading string"))?;
                    let encoded = str::from_utf8(encoded)
                        .map_err(|_error| self.invalid("invalid UTF-8 while reading string"))?;
                    let character = encoded
                        .chars()
                        .next()
                        .ok_or_else(|| self.invalid("invalid UTF-8 while reading string"))?;
                    self.position = end;
                    value.push(character);
                }
                None => return Err(self.invalid("unterminated string")),
            }
        }
    }

    fn parse_escape(&mut self, output: &mut String) -> Result<(), CanonicalJsonError> {
        match self.bump()? {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{8}'),
            b'f' => output.push('\u{c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => {
                let first = self.parse_hex_u16()?;
                let character = if (0xD800..=0xDBFF).contains(&first) {
                    if self.bump()? != b'\\' || self.bump()? != b'u' {
                        return Err(
                            self.invalid("high surrogate must be followed by low surrogate")
                        );
                    }
                    let second = self.parse_hex_u16()?;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        return Err(
                            self.invalid("high surrogate must be followed by low surrogate")
                        );
                    }
                    let high = u32::from(first)
                        .checked_sub(0xD800)
                        .ok_or_else(|| self.invalid("invalid high surrogate"))?;
                    let low = u32::from(second)
                        .checked_sub(0xDC00)
                        .ok_or_else(|| self.invalid("invalid low surrogate"))?;
                    let codepoint = 0x1_0000_u32
                        .checked_add(
                            high.checked_mul(0x400)
                                .ok_or_else(|| self.invalid("Unicode code point overflow"))?,
                        )
                        .and_then(|value| value.checked_add(low))
                        .ok_or_else(|| self.invalid("Unicode code point overflow"))?;
                    char::from_u32(codepoint)
                        .ok_or_else(|| self.invalid("invalid Unicode code point"))?
                } else if (0xDC00..=0xDFFF).contains(&first) {
                    return Err(self.invalid("unpaired low surrogate"));
                } else {
                    char::from_u32(u32::from(first))
                        .ok_or_else(|| self.invalid("invalid Unicode code point"))?
                };
                output.push(character);
            }
            _ => return Err(self.invalid("invalid string escape")),
        }
        Ok(())
    }

    fn parse_hex_u16(&mut self) -> Result<u16, CanonicalJsonError> {
        let start = self.position;
        let end = start
            .checked_add(4)
            .ok_or_else(|| self.invalid("Unicode escape position overflow"))?;
        let digits = self
            .input
            .get(start..end)
            .ok_or_else(|| self.invalid("incomplete Unicode escape"))?;
        let mut value = 0_u16;
        for digit in digits {
            let nibble = hex_value(*digit)
                .ok_or_else(|| self.invalid("Unicode escape must contain four hex digits"))?;
            value = value
                .checked_mul(16)
                .and_then(|value| value.checked_add(u16::from(nibble)))
                .ok_or_else(|| self.invalid("Unicode escape overflow"))?;
        }
        self.position = end;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<String, CanonicalJsonError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.expect_byte(b'-')?;
        }

        match self.peek() {
            Some(b'0') => {
                self.expect_byte(b'0')?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(self.invalid("leading zero in number"));
                }
            }
            Some(b'1'..=b'9') => self.consume_digits(),
            Some(_) | None => return Err(self.invalid("invalid number integer")),
        }

        if self.peek() == Some(b'.') {
            self.expect_byte(b'.')?;
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(self.invalid("number fraction requires a digit"));
            }
            self.consume_digits();
        }

        if self.peek().is_some_and(|byte| byte == b'e' || byte == b'E') {
            self.bump()?;
            if self.peek().is_some_and(|byte| byte == b'+' || byte == b'-') {
                self.bump()?;
            }
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(self.invalid("number exponent requires a digit"));
            }
            self.consume_digits();
        }

        let raw = self
            .input
            .get(start..self.position)
            .ok_or_else(|| self.invalid("number range overflow"))?;
        let raw = str::from_utf8(raw)
            .map_err(|_error| CanonicalJsonError::InvalidUtf8 { offset: start })?;
        normalize_number(raw, start)
    }

    fn consume_digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position = self.position.saturating_add(1);
        }
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), CanonicalJsonError> {
        for expected in literal {
            if self.bump()? != *expected {
                return Err(self.invalid("invalid JSON literal"));
            }
        }
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position = self.position.saturating_add(1);
        }
    }

    fn take_token(&mut self) -> String {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b',' | b']' | b'}'))
        {
            self.position = self.position.saturating_add(1);
        }
        self.input
            .get(start..self.position)
            .and_then(|bytes| str::from_utf8(bytes).ok())
            .map_or_else(String::new, str::to_owned)
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), CanonicalJsonError> {
        let actual = self.bump()?;
        if actual == expected {
            Ok(())
        } else {
            Err(self.invalid("unexpected JSON character"))
        }
    }

    fn bump(&mut self) -> Result<u8, CanonicalJsonError> {
        let byte = self
            .input
            .get(self.position)
            .copied()
            .ok_or_else(|| self.invalid("unexpected end of JSON"))?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or_else(|| self.invalid("JSON position overflow"))?;
        Ok(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.input
            .get(self.position..)
            .is_some_and(|remaining| remaining.starts_with(prefix))
    }

    fn invalid(&self, message: &str) -> CanonicalJsonError {
        CanonicalJsonError::InvalidJson {
            offset: self.position,
            message: message.to_owned(),
        }
    }
}

fn normalize_unicode(value: &str) -> String {
    nfc(value)
}

/// Unicode Normalization Form C of `value`: the form Norn1 canonicalizes
/// strings to, also an `EntityIR` v2 identity-key normalization step (#2848).
#[must_use]
pub fn nfc(value: &str) -> String {
    value.nfc().collect()
}

/// Unicode Normalization Form KC (compatibility composition) of `value`: an
/// `EntityIR` v2 identity-key normalization step (#2848).
#[must_use]
pub fn nfkc(value: &str) -> String {
    value.nfkc().collect()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => value.checked_sub(b'0'),
        b'a'..=b'f' => value
            .checked_sub(b'a')
            .and_then(|digit| digit.checked_add(10)),
        b'A'..=b'F' => value
            .checked_sub(b'A')
            .and_then(|digit| digit.checked_add(10)),
        _ => None,
    }
}

struct DecimalParts<'a> {
    negative: bool,
    integer: &'a str,
    fraction: &'a str,
    exponent: i64,
}

fn decimal_parts(raw: &str, offset: usize) -> Result<DecimalParts<'_>, CanonicalJsonError> {
    if matches!(
        raw,
        "NaN" | "+NaN" | "-NaN" | "Infinity" | "-Infinity" | "+Infinity"
    ) {
        return Err(CanonicalJsonError::NonFiniteNumber {
            value: raw.to_owned(),
            offset,
        });
    }

    let magnitude = raw.strip_prefix('-').unwrap_or(raw);
    if magnitude.is_empty() {
        return Err(CanonicalJsonError::InvalidDecimal {
            value: raw.to_owned(),
            offset,
        });
    }

    let (mantissa, exponent_text) = match magnitude.find(['e', 'E']) {
        Some(index) => {
            let mantissa = magnitude
                .get(..index)
                .ok_or_else(|| invalid_decimal(raw, offset))?;
            let exponent = magnitude
                .get(
                    index
                        .checked_add(1)
                        .ok_or_else(|| invalid_decimal(raw, offset))?..,
                )
                .ok_or_else(|| invalid_decimal(raw, offset))?;
            (mantissa, Some(exponent))
        }
        None => (magnitude, None),
    };

    let exponent = match exponent_text {
        Some(text) => text
            .parse::<i64>()
            .map_err(|_error| CanonicalJsonError::NumberTooLarge {
                value: raw.to_owned(),
                offset,
            })?,
        None => 0,
    };
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (mantissa, ""),
    };
    if !valid_integer(integer)
        || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        || (mantissa.contains('.') && fraction.is_empty())
    {
        return Err(invalid_decimal(raw, offset));
    }

    Ok(DecimalParts {
        negative: raw.starts_with('-'),
        integer,
        fraction,
        exponent,
    })
}

fn normalize_number(raw: &str, offset: usize) -> Result<String, CanonicalJsonError> {
    let parts = decimal_parts(raw, offset)?;
    let mut digits = String::new();
    digits.push_str(parts.integer);
    digits.push_str(parts.fraction);
    let significant_all = digits.trim_start_matches('0');
    if significant_all.is_empty() {
        return Ok("0".to_owned());
    }
    let significant = significant_all.trim_end_matches('0');
    let trailing_zero_count = significant_all
        .len()
        .checked_sub(significant.len())
        .ok_or_else(|| invalid_decimal(raw, offset))?;
    let fraction_len =
        i64::try_from(parts.fraction.len()).map_err(|_error| number_too_large(raw, offset))?;
    let trailing_zero_count =
        i64::try_from(trailing_zero_count).map_err(|_error| number_too_large(raw, offset))?;
    let scale = parts
        .exponent
        .checked_sub(fraction_len)
        .and_then(|value| value.checked_add(trailing_zero_count))
        .ok_or_else(|| number_too_large(raw, offset))?;
    format_decimal(raw, offset, parts.negative, significant, scale)
}

fn format_decimal(
    raw: &str,
    offset: usize,
    negative: bool,
    significant: &str,
    scale: i64,
) -> Result<String, CanonicalJsonError> {
    let significant_len =
        i64::try_from(significant.len()).map_err(|_error| number_too_large(raw, offset))?;
    let decimal_position = significant_len
        .checked_add(scale)
        .ok_or_else(|| number_too_large(raw, offset))?;

    let sign_len = usize::from(negative);
    let output_len = if decimal_position <= 0 {
        let zero_count = decimal_position
            .checked_neg()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| number_too_large(raw, offset))?;
        sign_len
            .checked_add(2)
            .and_then(|length| length.checked_add(zero_count))
            .and_then(|length| length.checked_add(significant.len()))
    } else {
        let position =
            usize::try_from(decimal_position).map_err(|_error| number_too_large(raw, offset))?;
        if position < significant.len() {
            sign_len
                .checked_add(significant.len())
                .and_then(|length| length.checked_add(1))
        } else {
            sign_len.checked_add(position)
        }
    }
    .ok_or_else(|| number_too_large(raw, offset))?;
    if output_len > MAX_CANONICAL_NUMBER_BYTES {
        return Err(number_too_large(raw, offset));
    }

    let mut output = String::with_capacity(output_len);
    if negative {
        output.push('-');
    }
    if decimal_position <= 0 {
        output.push('0');
        output.push('.');
        let zero_count = decimal_position
            .checked_neg()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| number_too_large(raw, offset))?;
        append_zeros(&mut output, zero_count);
        output.push_str(significant);
    } else {
        let position =
            usize::try_from(decimal_position).map_err(|_error| number_too_large(raw, offset))?;
        if position < significant.len() {
            let left = significant
                .get(..position)
                .ok_or_else(|| invalid_decimal(raw, offset))?;
            let right = significant
                .get(position..)
                .ok_or_else(|| invalid_decimal(raw, offset))?;
            output.push_str(left);
            output.push('.');
            output.push_str(right);
        } else {
            output.push_str(significant);
            let zero_count = position
                .checked_sub(significant.len())
                .ok_or_else(|| invalid_decimal(raw, offset))?;
            append_zeros(&mut output, zero_count);
        }
    }
    Ok(output)
}

fn valid_integer(value: &str) -> bool {
    if value == "0" {
        return true;
    }
    let mut characters = value.chars();
    matches!(characters.next(), Some('1'..='9')) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn invalid_decimal(value: &str, offset: usize) -> CanonicalJsonError {
    CanonicalJsonError::InvalidDecimal {
        value: value.to_owned(),
        offset,
    }
}

fn number_too_large(value: &str, offset: usize) -> CanonicalJsonError {
    CanonicalJsonError::NumberTooLarge {
        value: value.to_owned(),
        offset,
    }
}

fn next_depth(depth: usize, offset: usize) -> Result<usize, CanonicalJsonError> {
    let child_depth = depth
        .checked_add(1)
        .ok_or(CanonicalJsonError::NestingTooDeep {
            depth,
            limit: MAX_JSON_NESTING_DEPTH,
            offset,
        })?;
    if child_depth > MAX_JSON_NESTING_DEPTH {
        return Err(CanonicalJsonError::NestingTooDeep {
            depth: child_depth,
            limit: MAX_JSON_NESTING_DEPTH,
            offset,
        });
    }
    Ok(child_depth)
}

fn append_zeros(output: &mut String, count: usize) {
    for _ in 0..count {
        output.push('0');
    }
}

fn lower_hex(digest: [u8; 32]) -> String {
    let mut output = String::new();
    for byte in digest {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{byte:02x}"));
    }
    output
}

fn write_value(
    value: &JsonValue,
    output: &mut String,
    depth: usize,
) -> Result<(), CanonicalJsonError> {
    if depth > MAX_JSON_NESTING_DEPTH {
        return Err(CanonicalJsonError::NestingTooDeep {
            depth,
            limit: MAX_JSON_NESTING_DEPTH,
            offset: 0,
        });
    }
    match value {
        JsonValue::Null => output.push_str("null"),
        JsonValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(value) => output.push_str(normalize_number(value, 0)?.as_str()),
        JsonValue::String(value) => write_string(value, output),
        JsonValue::Array(values) => {
            output.push('[');
            let mut first = true;
            for value in values {
                if !first {
                    output.push(',');
                }
                first = false;
                let child_depth = next_depth(depth, 0)?;
                write_value(value, output, child_depth)?;
            }
            output.push(']');
        }
        JsonValue::Object(values) => write_object(values, output, depth)?,
    }
    Ok(())
}

fn write_object(
    values: &BTreeMap<String, JsonValue>,
    output: &mut String,
    depth: usize,
) -> Result<(), CanonicalJsonError> {
    let mut entries = Vec::with_capacity(values.len());
    for (key, value) in values {
        let normalized_key = normalize_unicode(key);
        entries.push((normalized_key, value));
    }
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    for window in entries.windows(2) {
        if let [left, right] = window
            && left.0 == right.0
        {
            return Err(CanonicalJsonError::DuplicateKey {
                key: left.0.clone(),
                offset: 0,
            });
        }
    }

    output.push('{');
    let mut first = true;
    for (key, value) in entries {
        if !first {
            output.push(',');
        }
        first = false;
        write_string(key.as_str(), output);
        output.push(':');
        let child_depth = next_depth(depth, 0)?;
        write_value(value, output, child_depth)?;
    }
    output.push('}');
    Ok(())
}

fn write_string(value: &str, output: &mut String) {
    output.push('"');
    for character in normalize_unicode(value).chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "fmt::Write for String is infallible; its trait signature still returns Result"
                )]
                let _ = std::fmt::Write::write_fmt(
                    output,
                    format_args!("\\u{:04x}", u32::from(character)),
                );
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..8)
            .prop_map(|characters| characters.into_iter().collect())
    }

    fn arb_number() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "0".to_owned(),
            "1".to_owned(),
            "-1".to_owned(),
            "1.25".to_owned(),
            "10.0".to_owned(),
            "0.0010".to_owned(),
            "1e3".to_owned(),
            "-2.50e-2".to_owned(),
        ])
    }

    fn arb_json() -> impl Strategy<Value = JsonValue> {
        let leaf = prop_oneof![
            Just(JsonValue::Null),
            any::<bool>().prop_map(JsonValue::Bool),
            arb_number().prop_map(JsonValue::Number),
            arb_string().prop_map(JsonValue::String),
        ];
        leaf.prop_recursive(3, 32, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(JsonValue::Array),
                prop::collection::btree_map(arb_key(), inner, 0..4).prop_map(JsonValue::Object),
            ]
        })
    }

    fn arb_key() -> impl Strategy<Value = String> {
        prop::collection::vec(b'a'..=b'z', 0..8)
            .prop_map(|bytes| bytes.into_iter().map(char::from).collect())
    }

    fn nested_array_json(depth: usize) -> String {
        let mut value = String::new();
        for _ in 0..depth {
            value.push('[');
        }
        value.push('0');
        for _ in 0..depth {
            value.push(']');
        }
        value
    }

    fn nested_array_value(depth: usize) -> JsonValue {
        let mut value = JsonValue::Number("0".to_owned());
        for _ in 0..depth {
            value = JsonValue::Array(vec![value]);
        }
        value
    }

    #[test]
    fn profile_registry_is_explicit_and_stable() {
        assert_eq!(
            profile_registry(),
            &[HashProfile::Legacy0, HashProfile::Norn1]
        );
        assert_eq!(HashProfile::Legacy0.identifier(), "legacy0");
        assert_eq!(HashProfile::Norn1.identifier(), "norn1");
        assert_eq!(HashProfile::Legacy0.version(), 0);
        assert_eq!(HashProfile::Norn1.version(), 1);
        assert_eq!(HashProfile::Legacy0.domain_tag(), None);
        assert_eq!(HashProfile::Norn1.domain_tag(), Some(NORN1_DOMAIN_TAG));
        assert_eq!(
            serde_json::to_string(&HashProfile::Norn1).ok(),
            Some("\"norn1\"".to_owned())
        );
        assert_eq!(
            serde_json::from_str::<HashProfile>("\"legacy0\"").ok(),
            Some(HashProfile::Legacy0)
        );
    }

    proptest! {
        #[test]
        fn parse_serialize_parse_is_stable(value in arb_json()) {
            let bytes = serialize(&value);
            prop_assert!(bytes.is_ok());
            let bytes = bytes.unwrap_or_default();
            let parsed = parse(&bytes);
            prop_assert!(parsed.is_ok());
            let parsed = parsed.unwrap_or(JsonValue::Null);
            let roundtrip = serialize(&parsed);
            prop_assert!(roundtrip.is_ok());
            let roundtrip = roundtrip.unwrap_or_default();
            prop_assert_eq!(&roundtrip, &bytes);
            let reparsed = parse(&roundtrip);
            prop_assert!(reparsed.is_ok());
            prop_assert_eq!(reparsed.ok(), Some(parsed));
        }

        #[test]
        fn norn1_hash_is_stable_across_key_order_permutations(
            first in -100_000_i64..100_000_i64,
            second in -100_000_i64..100_000_i64,
        ) {
            let left = format!(r#"{{"b":{second},"a":{{"d":{first},"c":true}}}}"#);
            let right = format!(r#"{{"a":{{"c":true,"d":{first}}},"b":{second}}}"#);
            prop_assert_eq!(HashProfile::Norn1.hash_json(left), HashProfile::Norn1.hash_json(right));
        }
    }

    #[test]
    fn norn1_rejects_duplicate_keys_and_non_finite_numbers() {
        assert!(matches!(
            parse(br#"{"a":1,"a":2}"#),
            Err(CanonicalJsonError::DuplicateKey { .. })
        ));
        assert!(matches!(
            parse(b"NaN"),
            Err(CanonicalJsonError::NonFiniteNumber { .. })
        ));
        assert!(matches!(
            parse(b"-NaN"),
            Err(CanonicalJsonError::NonFiniteNumber { .. })
        ));
        assert!(matches!(
            serialize(&JsonValue::Number("Infinity".to_owned())),
            Err(CanonicalJsonError::NonFiniteNumber { .. })
        ));
    }

    #[test]
    fn norn1_normalizes_numbers_unicode_and_controls() {
        let input = br#"{
          "z": -0.0000,
          "e": 1.2300e+2,
          "decomposed": "e\u0301",
          "control": "\u0000\n\t",
          "nested": {"b": 1e-3, "a": -0}
        }"#;
        assert_eq!(
            canonicalize_json(input).ok(),
            Some(r#"{"control":"\u0000\n\t","decomposed":"é","e":123,"nested":{"a":0,"b":0.001},"z":0}"#.as_bytes().to_vec())
        );
    }

    #[test]
    fn norn1_rejects_nfc_key_collisions() {
        let result = parse("{\"é\":1,\"e\\u0301\":2}");
        assert!(matches!(
            result,
            Err(CanonicalJsonError::DuplicateKey { .. })
        ));
    }

    #[test]
    fn norn1_bounds_parse_and_serialize_nesting() {
        let at_limit = nested_array_json(MAX_JSON_NESTING_DEPTH);
        assert!(parse(at_limit).is_ok());

        let over_limit = nested_array_json(MAX_JSON_NESTING_DEPTH.saturating_add(1));
        assert!(matches!(
            parse(over_limit),
            Err(CanonicalJsonError::NestingTooDeep { depth, limit, .. })
                if depth == MAX_JSON_NESTING_DEPTH.saturating_add(1)
                    && limit == MAX_JSON_NESTING_DEPTH
        ));

        let at_limit = nested_array_value(MAX_JSON_NESTING_DEPTH);
        assert!(serialize(&at_limit).is_ok());

        let over_limit = nested_array_value(MAX_JSON_NESTING_DEPTH.saturating_add(1));
        assert!(matches!(
            serialize(&over_limit),
            Err(CanonicalJsonError::NestingTooDeep { depth, limit, .. })
                if depth == MAX_JSON_NESTING_DEPTH.saturating_add(1)
                    && limit == MAX_JSON_NESTING_DEPTH
        ));
    }

    #[test]
    fn multilingual_sweep_buffers_preserve_verbatim_and_normalized_text() {
        // Sweep cursors retain arrays of source-document JSON strings. Arabic
        // text near the front must not repeatedly scan every later document.
        let text = "عميل é 中 😀 e\u{301} \"\\\n".repeat(64);
        let normalized = "عميل é 中 😀 é \"\\\n".repeat(64);
        let rows = vec![text.clone(); 128];
        let encoded = serde_json::to_vec(&rows).unwrap_or_default();
        assert_eq!(
            parse_json_verbatim(&encoded).ok(),
            Some(JsonValue::Array(vec![JsonValue::String(text); 128]))
        );
        assert_eq!(
            parse(&encoded).ok(),
            Some(JsonValue::Array(vec![JsonValue::String(normalized); 128]))
        );
    }

    #[test]
    fn utf8_string_widths_and_invalid_encodings_remain_strict() {
        let text = "\u{80}\u{7ff}\u{800}\u{ffff}\u{10000}\u{10ffff}";
        let encoded = serde_json::to_vec(text).unwrap_or_default();
        assert_eq!(
            parse_json_verbatim(&encoded).ok(),
            Some(JsonValue::String(text.to_owned()))
        );
        for invalid in [
            vec![b'"', 0x80, b'"'],
            vec![b'"', 0xc0, 0x80, b'"'],
            vec![b'"', 0xc2, b'"'],
            vec![b'"', 0xed, 0xa0, 0x80, b'"'],
            vec![b'"', 0xf4, 0x90, 0x80, 0x80, b'"'],
        ] {
            for result in [parse(&invalid), parse_json_verbatim(&invalid)] {
                assert!(matches!(
                    result,
                    Err(CanonicalJsonError::InvalidUtf8 { offset: 1 })
                ));
            }
        }
    }

    #[test]
    fn norn1_large_objects_use_sorted_collision_detection() {
        let values = (0..4_096)
            .map(|index| (format!("key-{index:04}"), JsonValue::Number("1".to_owned())))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(values.len(), 4_096);
        assert!(serialize(&JsonValue::Object(values.clone())).is_ok());

        let mut collision = values;
        collision.insert("é".to_owned(), JsonValue::Null);
        collision.insert("e\u{301}".to_owned(), JsonValue::Null);
        assert!(matches!(
            serialize(&JsonValue::Object(collision)),
            Err(CanonicalJsonError::DuplicateKey { key, .. }) if key == "é"
        ));
    }

    #[test]
    fn norn1_rejects_unbounded_decimal_expansion() {
        let input = format!("1e{MAX_CANONICAL_NUMBER_BYTES}");
        assert!(matches!(
            parse(input),
            Err(CanonicalJsonError::NumberTooLarge { .. })
        ));
    }

    #[test]
    fn norn1_uses_domain_separated_sha256() {
        let bytes = b"{}";
        let mut hasher = Sha256::new();
        hasher.update(NORN1_DOMAIN_TAG);
        hasher.update(bytes);
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(HashProfile::Norn1.hash_canonical_bytes(bytes), expected);
        assert_ne!(
            HashProfile::Norn1.hash_canonical_bytes(bytes),
            HashProfile::Legacy0.hash_canonical_bytes(bytes)
        );
    }

    #[test]
    fn legacy_structured_hasher_matches_norn_ir_part_encoding() {
        let mut actual = LegacyStableHasher::new(b"example.domain.v1");
        actual.marker(b"start");
        actual.text(b"name", "value");
        actual.byte(b"version", 7);
        actual.u64(b"count", 9);

        let mut expected = Sha256::new();
        for (label, value) in [
            (b"domain".as_slice(), b"example.domain.v1".as_slice()),
            (b"marker".as_slice(), b"start".as_slice()),
            (b"name".as_slice(), b"value".as_slice()),
            (b"version".as_slice(), &[7_u8][..]),
            (b"count".as_slice(), &9_u64.to_be_bytes()[..]),
        ] {
            expected.update(Sha256::digest(label));
            expected.update(Sha256::digest(value));
        }
        let expected = lower_hex(expected.finalize().into());
        assert_eq!(actual.finish(), expected);
    }

    #[test]
    fn legacy0_matches_current_artifact_hash_fixture() {
        let input = br#"{
            "irVersion":"0.1",
            "artifactId":"retail.entity.sku",
            "kind":"entity",
            "namespace":"retail",
            "name":"Sku",
            "version":"0.1.0",
            "lifecycle":"approved",
            "scope":{"orgId":"org_demo","workspaceId":"workspace_demo","environmentId":"sandbox"},
            "dependencies":[],
            "provenance":{"source":"agent","actorId":"agent_1"},
            "payload":{"fields":{"sku":{"required":true,"type":"text"}}}
        }"#;
        assert_eq!(
            HashProfile::Legacy0.hash_json(input).ok(),
            Some("026f956e837ad090c704c1aca9211a2c4d859bdb14d1874c49f21d5ce648fca5".to_owned())
        );
    }

    #[test]
    fn legacy0_prunes_only_historical_optional_artifact_nulls() {
        let with_nulls = br#"{
          "scope":{"orgId":"org","tenantId":null},
          "provenance":{"source":"system","actorId":null},
          "dependencies":[{"artifactId":"dep","version":null,"required":true}],
          "payload":{"keep":null}
        }"#;
        let without_nulls = br#"{
          "scope":{"orgId":"org"},
          "provenance":{"source":"system"},
          "dependencies":[{"artifactId":"dep","required":true}],
          "payload":{"keep":null}
        }"#;
        assert_eq!(
            HashProfile::Legacy0.hash_json(with_nulls),
            HashProfile::Legacy0.hash_json(without_nulls)
        );
    }
}
