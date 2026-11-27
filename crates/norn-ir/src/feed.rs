//! Declared feed semantics (`FeedMode`, `VersionKind`, parsed `SourceVersion`),
//! tri-state field patches (`FieldPatch`), and the pure observation reducer
//! (`FEED-MODE-1` / Lane B4).
//!
//! All types and reduction functions in this module are pure, deterministic,
//! and IO-free (no floats, no ambient clocks, no hash maps), making the core
//! `FieldPatch` / `LinkPatchOp` reducer directly suitable for formal extraction.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::rule::JsonObject;
use crate::{RuntimeObject, RuntimeValue};

/// Mapping extension key declaring per-dataset feed mode and version ordering.
pub const FEED_EXTENSION_KEY: &str = "norn.feed/1";

/// Whether a source feed delivers complete records or partial field patches.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedMode {
    /// Each delivered record is a full snapshot of the dataset's declared
    /// fields: an absent pointer becomes `null` and missing required fields
    /// refuse.
    #[default]
    FullSnapshot,
    /// Each delivered record is a partial patch: omitted keys preserve the
    /// prior canonical value (`FieldPatch::Absent`), explicit JSON `null`
    /// clears nullable fields (`FieldPatch::ExplicitNull`), and concrete values
    /// overwrite (`FieldPatch::Value`).
    Patch,
}

impl FeedMode {
    /// Whether this mode is the default ([`Self::FullSnapshot`]); used for
    /// backward-compatible `skip_serializing_if` on wire envelopes.
    #[must_use]
    pub const fn is_default(self) -> bool {
        matches!(self, Self::FullSnapshot)
    }

    /// Stable wire string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullSnapshot => "full_snapshot",
            Self::Patch => "patch",
        }
    }

    /// Parse a wire string representation, failing closed on unknown values.
    #[must_use]
    pub fn parse_wire(text: &str) -> Option<Self> {
        match text {
            "full_snapshot" => Some(Self::FullSnapshot),
            "patch" => Some(Self::Patch),
            _ => None,
        }
    }
}

/// How a feed's `source_version` values are parsed and compared for watermark
/// ordering and monotonicity checks.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VersionKind {
    /// Bytewise UTF-8 lexicographical order (default for backward compatibility).
    #[default]
    Lexicographical,
    /// Strict RFC 3339 nanosecond UTC-instant profile
    /// (`YYYY-MM-DDTHH:MM:SS[.f{1..9}](Z|±HH:MM)`), ordered by UTC instant.
    /// Seconds are `00..=59` (leap second `:60` is rejected) and fractional
    /// precision is bounded to 1..=9 digits (nanoseconds) so ordering is lossless.
    Rfc3339,
    /// Strict unsigned 64-bit decimal integer (`"100" > "99"`).
    Uint64,
    /// Strict signed 64-bit decimal integer (`"-10" < "-2" < "0" < "10"`).
    Int64,
    /// Opaque non-orderable version token (equality only; refuses ordering).
    Opaque,
}

impl VersionKind {
    /// Whether this version kind is the default ([`Self::Lexicographical`]).
    #[must_use]
    pub const fn is_default(self) -> bool {
        matches!(self, Self::Lexicographical)
    }

    /// Whether versions of this kind admit a total ordering suitable for
    /// watermark progression.
    #[must_use]
    pub const fn is_orderable(self) -> bool {
        match self {
            Self::Lexicographical | Self::Rfc3339 | Self::Uint64 | Self::Int64 => true,
            Self::Opaque => false,
        }
    }

    /// Stable wire name of this version kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexicographical => "lexicographical",
            Self::Rfc3339 => "rfc3339",
            Self::Uint64 => "uint64",
            Self::Int64 => "int64",
            Self::Opaque => "opaque",
        }
    }

    /// Parse a wire name, failing closed on unknown values.
    #[must_use]
    pub fn parse_wire(text: &str) -> Option<Self> {
        match text {
            "lexicographical" => Some(Self::Lexicographical),
            "rfc3339" => Some(Self::Rfc3339),
            "uint64" => Some(Self::Uint64),
            "int64" => Some(Self::Int64),
            "opaque" => Some(Self::Opaque),
            _ => None,
        }
    }
}

/// Declared feed contract for one source dataset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeedContract {
    /// Snapshot vs partial-patch delivery semantics.
    #[serde(default, skip_serializing_if = "is_default_feed_mode")]
    pub mode: FeedMode,
    /// Typed source-version parser and comparator.
    #[serde(default, skip_serializing_if = "is_default_version_kind")]
    pub version_kind: VersionKind,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires &T"
)]
const fn is_default_feed_mode(mode: &FeedMode) -> bool {
    mode.is_default()
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires &T"
)]
const fn is_default_version_kind(kind: &VersionKind) -> bool {
    kind.is_default()
}

/// Mapping-level declaration (`extensions["norn.feed/1"]`) binding source
/// datasets to their [`FeedContract`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeedDeclaration {
    /// Dataset id to its declared feed contract.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub feeds: BTreeMap<String, FeedContract>,
}

impl FeedDeclaration {
    /// Decode `extensions["norn.feed/1"]` if present.
    ///
    /// # Errors
    /// A present extension value that does not match [`FeedDeclaration`].
    pub fn from_extensions(extensions: Option<&JsonObject>) -> Result<Option<Self>, String> {
        let Some(value) = extensions.and_then(|ext| ext.get(FEED_EXTENSION_KEY)) else {
            return Ok(None);
        };
        let json = serde_json::to_value(value)
            .map_err(|error| format!("invalid {FEED_EXTENSION_KEY} extension: {error}"))?;
        serde_json::from_value(json)
            .map(Some)
            .map_err(|error| format!("invalid {FEED_EXTENSION_KEY} extension: {error}"))
    }

    /// The declared [`FeedMode`] for `dataset`, defaulting to
    /// [`FeedMode::FullSnapshot`].
    #[must_use]
    pub fn mode_for(&self, dataset: &str) -> FeedMode {
        self.feeds
            .get(dataset)
            .map_or(FeedMode::FullSnapshot, |contract| contract.mode)
    }
}

/// Why parsing or comparing a [`SourceVersion`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceVersionError {
    /// A source version was empty or blank.
    Empty,
    /// The raw string did not conform to its declared [`VersionKind`].
    InvalidFormat {
        /// Declared version kind.
        kind: VersionKind,
        /// Raw version text.
        raw: String,
        /// Human-readable parse refusal.
        reason: &'static str,
    },
    /// Attempted to compare two versions with different [`VersionKind`]s.
    IncompatibleKinds {
        /// Left operand's kind.
        left: VersionKind,
        /// Right operand's kind.
        right: VersionKind,
    },
    /// Attempted to order [`VersionKind::Opaque`] tokens.
    UnorderableOpaque,
}

impl fmt::Display for SourceVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("source version must not be empty"),
            Self::InvalidFormat { kind, raw, reason } => {
                write!(
                    f,
                    "invalid {} source version {raw:?}: {reason}",
                    kind.as_str()
                )
            }
            Self::IncompatibleKinds { left, right } => write!(
                f,
                "cannot compare {} and {} source versions",
                left.as_str(),
                right.as_str()
            ),
            Self::UnorderableOpaque => {
                f.write_str("opaque source versions cannot be ordered for watermark advancement")
            }
        }
    }
}

impl std::error::Error for SourceVersionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedVersion {
    Lexicographical(String),
    Rfc3339 { epoch_seconds: i64, nanos: u32 },
    Uint64(u64),
    Int64(i64),
    Opaque(String),
}

/// A source version parsed and validated under its declared [`VersionKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceVersion {
    kind: VersionKind,
    raw: String,
    parsed: ParsedVersion,
}

impl SourceVersion {
    /// Parse `raw` strictly according to `kind`.
    ///
    /// # Errors
    /// [`SourceVersionError::Empty`] or [`SourceVersionError::InvalidFormat`].
    pub fn parse(kind: VersionKind, raw: &str) -> Result<Self, SourceVersionError> {
        if raw.is_empty() {
            return Err(SourceVersionError::Empty);
        }
        let parsed = match kind {
            VersionKind::Lexicographical => ParsedVersion::Lexicographical(raw.to_owned()),
            VersionKind::Opaque => ParsedVersion::Opaque(raw.to_owned()),
            VersionKind::Uint64 => ParsedVersion::Uint64(parse_strict_u64(raw)?),
            VersionKind::Int64 => ParsedVersion::Int64(parse_strict_i64(raw)?),
            VersionKind::Rfc3339 => {
                let (epoch_seconds, nanos) = parse_rfc3339_instant(raw)?;
                ParsedVersion::Rfc3339 {
                    epoch_seconds,
                    nanos,
                }
            }
        };
        Ok(Self {
            kind,
            raw: raw.to_owned(),
            parsed,
        })
    }

    /// The declared [`VersionKind`].
    #[must_use]
    pub const fn kind(&self) -> VersionKind {
        self.kind
    }

    /// The exact raw version text as delivered by the source.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Compare two parsed versions of the same orderable [`VersionKind`].
    ///
    /// # Errors
    /// [`SourceVersionError::IncompatibleKinds`] if kinds differ, or
    /// [`SourceVersionError::UnorderableOpaque`] for [`VersionKind::Opaque`].
    pub fn try_cmp(&self, other: &Self) -> Result<Ordering, SourceVersionError> {
        if self.kind != other.kind {
            return Err(SourceVersionError::IncompatibleKinds {
                left: self.kind,
                right: other.kind,
            });
        }
        match (&self.parsed, &other.parsed) {
            (ParsedVersion::Lexicographical(a), ParsedVersion::Lexicographical(b)) => Ok(a.cmp(b)),
            (
                ParsedVersion::Rfc3339 {
                    epoch_seconds: sa,
                    nanos: na,
                },
                ParsedVersion::Rfc3339 {
                    epoch_seconds: sb,
                    nanos: nb,
                },
            ) => Ok((sa, na).cmp(&(sb, nb))),
            (ParsedVersion::Uint64(a), ParsedVersion::Uint64(b)) => Ok(a.cmp(b)),
            (ParsedVersion::Int64(a), ParsedVersion::Int64(b)) => Ok(a.cmp(b)),
            (ParsedVersion::Opaque(_), ParsedVersion::Opaque(_)) => {
                Err(SourceVersionError::UnorderableOpaque)
            }
            _ => Err(SourceVersionError::IncompatibleKinds {
                left: self.kind,
                right: other.kind,
            }),
        }
    }
}

fn invalid_version(kind: VersionKind, raw: &str, reason: &'static str) -> SourceVersionError {
    SourceVersionError::InvalidFormat {
        kind,
        raw: raw.to_owned(),
        reason,
    }
}

fn parse_strict_u64(raw: &str) -> Result<u64, SourceVersionError> {
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_version(
            VersionKind::Uint64,
            raw,
            "expected ASCII decimal digits",
        ));
    }
    raw.parse::<u64>()
        .map_err(|_overflow| invalid_version(VersionKind::Uint64, raw, "out of u64 range"))
}

fn parse_strict_i64(raw: &str) -> Result<i64, SourceVersionError> {
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_version(
            VersionKind::Int64,
            raw,
            "expected optional '-' followed by ASCII decimal digits",
        ));
    }
    raw.parse::<i64>()
        .map_err(|_overflow| invalid_version(VersionKind::Int64, raw, "out of i64 range"))
}

/// Parse strict RFC 3339 (`YYYY-MM-DD[T|t| ]HH:MM:SS[.frac](Z|z|±HH:MM)`) into
/// UTC `(epoch_seconds, nanos)` using pure checked integer arithmetic.
fn parse_rfc3339_instant(raw: &str) -> Result<(i64, u32), SourceVersionError> {
    let bad = |reason: &'static str| invalid_version(VersionKind::Rfc3339, raw, reason);
    let bytes = raw.as_bytes();
    if bytes.len() < 20 {
        return Err(bad("too short for RFC 3339 timestamp"));
    }
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(bad("expected YYYY-MM-DDTHH:MM:SS prefix"));
    }
    let year = parse_digits_i64(raw.get(0..4), bad)?;
    let month = parse_digits_u32(raw.get(5..7), bad)?;
    let day = parse_digits_u32(raw.get(8..10), bad)?;
    let hour = parse_digits_u32(raw.get(11..13), bad)?;
    let minute = parse_digits_u32(raw.get(14..16), bad)?;
    let second = parse_digits_u32(raw.get(17..19), bad)?;
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(bad("date or time component out of range"));
    }
    let rest = raw
        .get(19..)
        .ok_or_else(|| bad("missing timezone offset"))?;
    let (nanos, tz) = parse_fraction_and_tz(rest, bad)?;
    let offset_seconds = parse_tz_offset(tz, bad)?;
    let days = days_from_civil(year, month, day)?;
    let day_seconds = days
        .checked_mul(86_400)
        .ok_or_else(|| bad("timestamp out of i64 range"))?;
    let time_seconds = i64::from(hour)
        .checked_mul(3_600)
        .and_then(|h| h.checked_add(i64::from(minute).checked_mul(60)?))
        .and_then(|hm| hm.checked_add(i64::from(second)))
        .ok_or_else(|| bad("time out of i64 range"))?;
    let epoch_seconds = day_seconds
        .checked_add(time_seconds)
        .and_then(|local| local.checked_sub(offset_seconds))
        .ok_or_else(|| bad("UTC instant out of i64 range"))?;
    Ok((epoch_seconds, nanos))
}

fn parse_digits_u32(
    slice: Option<&str>,
    bad: impl Fn(&'static str) -> SourceVersionError,
) -> Result<u32, SourceVersionError> {
    let text = slice.ok_or_else(|| bad("missing numeric field"))?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("expected ASCII digits"));
    }
    text.parse::<u32>()
        .map_err(|_err| bad("numeric component overflow"))
}

fn parse_digits_i64(
    slice: Option<&str>,
    bad: impl Fn(&'static str) -> SourceVersionError,
) -> Result<i64, SourceVersionError> {
    parse_digits_u32(slice, bad).map(i64::from)
}

fn parse_fraction_and_tz(
    rest: &str,
    bad: impl Fn(&'static str) -> SourceVersionError,
) -> Result<(u32, &str), SourceVersionError> {
    let Some(after_dot) = rest.strip_prefix('.') else {
        return Ok((0, rest));
    };
    let frac_len = after_dot.bytes().take_while(u8::is_ascii_digit).count();
    if frac_len == 0 || frac_len > 9 {
        return Err(bad("fractional seconds must have 1 to 9 digits"));
    }
    let (frac_str, tz) = after_dot
        .split_at_checked(frac_len)
        .ok_or_else(|| bad("invalid fractional seconds"))?;
    let mut nanos = frac_str
        .parse::<u32>()
        .map_err(|_err| bad("fractional seconds overflow"))?;
    for _ in frac_len..9 {
        nanos = nanos
            .checked_mul(10)
            .ok_or_else(|| bad("nanosecond scaling overflow"))?;
    }
    Ok((nanos, tz))
}

fn parse_tz_offset(
    tz: &str,
    bad: impl Fn(&'static str) -> SourceVersionError,
) -> Result<i64, SourceVersionError> {
    if tz == "Z" || tz == "z" {
        return Ok(0);
    }
    let (sign, rest) = if let Some(rest) = tz.strip_prefix('+') {
        (1_i64, rest)
    } else if let Some(rest) = tz.strip_prefix('-') {
        (-1_i64, rest)
    } else {
        return Err(bad("expected 'Z' or '±HH:MM' offset"));
    };
    if rest.len() != 5 || rest.as_bytes().get(2) != Some(&b':') {
        return Err(bad("expected '±HH:MM' offset"));
    }
    let hours = parse_digits_u32(rest.get(0..2), &bad)?;
    let minutes = parse_digits_u32(rest.get(3..5), &bad)?;
    if hours > 23 || minutes > 59 {
        return Err(bad("timezone offset out of range"));
    }
    let magnitude = i64::from(hours)
        .checked_mul(3_600)
        .and_then(|h| h.checked_add(i64::from(minutes).checked_mul(60)?))
        .ok_or_else(|| bad("offset overflow"))?;
    sign.checked_mul(magnitude)
        .ok_or_else(|| bad("offset sign overflow"))
}

fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's civil-to-days algorithm (days since 1970-01-01) using
/// `div_euclid`/`rem_euclid` and checked arithmetic (zero `/` or `%`).
fn days_from_civil(year: i64, month: u32, day: u32) -> Result<i64, SourceVersionError> {
    let y = if month <= 2 {
        year.checked_sub(1).ok_or(SourceVersionError::Empty)?
    } else {
        year
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let m_shifted = if month > 2 {
        m.checked_sub(3)
    } else {
        m.checked_add(9)
    }
    .ok_or(SourceVersionError::Empty)?;
    let doy = m_shifted
        .checked_mul(153)
        .and_then(|v| v.checked_add(2))
        .map(|v| v.div_euclid(5))
        .and_then(|v| v.checked_add(i64::from(day)))
        .and_then(|v| v.checked_sub(1))
        .ok_or(SourceVersionError::Empty)?;
    let doe = yoe
        .checked_mul(365)
        .and_then(|v| v.checked_add(yoe.div_euclid(4)))
        .and_then(|v| v.checked_sub(yoe.div_euclid(100)))
        .and_then(|v| v.checked_add(doy))
        .ok_or(SourceVersionError::Empty)?;
    era.checked_mul(146_097)
        .and_then(|v| v.checked_add(doe))
        .and_then(|v| v.checked_sub(719_468))
        .ok_or(SourceVersionError::Empty)
}

/// Tri-state field update emitted by mapping evaluation and consumed by the
/// pure observation reducer (`FEED-MODE-1`).
///
/// Forms an associative monoid under [`compose_field_patch`] with identity
/// [`FieldPatch::Absent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum FieldPatch<V> {
    /// The source field(s) were omitted from a partial patch; preserve prior
    /// canonical state unchanged.
    Absent,
    /// The source explicitly reported `null`; clear a nullable target property
    /// to `null`.
    ExplicitNull,
    /// The source reported a concrete non-null value; overwrite the target
    /// property.
    Value(V),
}

/// Tri-state link update for a single-valued observed link rule (`FEED-MODE-1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", content = "target", rename_all = "snake_case")]
pub enum LinkPatchOp {
    /// Source key field(s) were omitted in a partial patch; preserve existing
    /// link claim(s).
    Absent,
    /// Source explicitly reported `null`; withdraw/clear the link claim.
    Clear,
    /// Source reported an explicit target key.
    Target(String),
}

/// Pure field-level observation reducer (generic, Aeneas/Lean-extractable):
/// applies a single [`FieldPatch`] to an optional prior field value.
#[must_use]
pub fn reduce_field<V: Clone>(
    prior: Option<&V>,
    patch: &FieldPatch<V>,
    null_value: V,
) -> Option<V> {
    match patch {
        FieldPatch::Absent => prior.cloned(),
        FieldPatch::ExplicitNull => Some(null_value),
        FieldPatch::Value(value) => Some(value.clone()),
    }
}

/// Pure [`RuntimeValue`] specialization of [`reduce_field`].
#[must_use]
pub fn reduce_runtime_field(
    prior: Option<&RuntimeValue>,
    patch: &FieldPatch<RuntimeValue>,
) -> Option<RuntimeValue> {
    reduce_field(prior, patch, RuntimeValue::Null)
}

/// Associative composition of two [`FieldPatch`] updates in chronological order
/// (`earlier` followed by `later`), with identity [`FieldPatch::Absent`].
///
/// Law: `reduce_field(reduce_field(s, p1, null).as_ref(), p2, null) ==
///       reduce_field(s, &compose_field_patch(p1, p2), null)`.
#[must_use]
pub fn compose_field_patch<V: Clone>(
    earlier: &FieldPatch<V>,
    later: &FieldPatch<V>,
) -> FieldPatch<V> {
    match later {
        FieldPatch::Absent => earlier.clone(),
        FieldPatch::ExplicitNull => FieldPatch::ExplicitNull,
        FieldPatch::Value(value) => FieldPatch::Value(value.clone()),
    }
}

/// Pure object-level observation reducer: applies a map of tri-state
/// [`FieldPatch`] updates onto an optional prior [`RuntimeObject`].
#[must_use]
pub fn reduce_object(
    prior: Option<&RuntimeObject>,
    patches: &BTreeMap<String, FieldPatch<RuntimeValue>>,
) -> RuntimeObject {
    let mut state = prior.cloned().unwrap_or_default();
    for (field, patch) in patches {
        if let Some(next) = reduce_runtime_field(state.get(field), patch) {
            state.insert(field.clone(), next);
        }
    }
    state
}

/// Associative composition of two object-level [`FieldPatch`] maps in
/// chronological order (`earlier` followed by `later`).
#[must_use]
pub fn compose_object_patches(
    earlier: &BTreeMap<String, FieldPatch<RuntimeValue>>,
    later: &BTreeMap<String, FieldPatch<RuntimeValue>>,
) -> BTreeMap<String, FieldPatch<RuntimeValue>> {
    let mut combined = earlier.clone();
    for (field, patch) in later {
        let merged = match combined.get(field) {
            Some(prev) => compose_field_patch(prev, patch),
            None => patch.clone(),
        };
        combined.insert(field.clone(), merged);
    }
    combined
}

/// Pure link-level observation reducer: applies a [`LinkPatchOp`] to an
/// optional prior target key.
#[must_use]
pub fn reduce_link(prior: Option<&str>, op: &LinkPatchOp) -> Option<String> {
    match op {
        LinkPatchOp::Absent => prior.map(str::to_owned),
        LinkPatchOp::Clear => None,
        LinkPatchOp::Target(target) => Some(target.clone()),
    }
}

/// Associative composition of two [`LinkPatchOp`] updates (`earlier` followed
/// by `later`), with identity [`LinkPatchOp::Absent`].
#[must_use]
pub fn compose_link_patch(earlier: &LinkPatchOp, later: &LinkPatchOp) -> LinkPatchOp {
    match later {
        LinkPatchOp::Absent => earlier.clone(),
        LinkPatchOp::Clear => LinkPatchOp::Clear,
        LinkPatchOp::Target(target) => LinkPatchOp::Target(target.clone()),
    }
}
