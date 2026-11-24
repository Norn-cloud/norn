//! Versioned offline IANA timezone database bundles (`#1440`).
//!
//! Each bundle is a deterministic, self-contained binary image (`NTZDB01\0`)
//! carrying an embedded IANA release identifier, a strictly sorted case-sensitive
//! zone index sharing deduplicated TZif byte ranges across aliases, and the raw
//! TZif payload section. The same checked decoder admits both compiled-in
//! bundles (`2025b` and `2026c`) and runtime/cell-installed bundle bytes,
//! parsing every distinct TZif blob once at admission so evaluation performs
//! zero TZif decoding.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};
use norn_ir::canonical::HashProfile;

use crate::temporal::TemporalError;

/// Versioned magic prefix of a Norn offline tzdb bundle (`NTZDB01\0`).
pub const TZDB_BUNDLE_MAGIC: &[u8; 8] = b"NTZDB01\0";

/// Maximum admitted byte length of a single tzdb bundle (4 MiB).
pub const MAX_TZDB_BUNDLE_BYTES: usize = 4_194_304;

/// Maximum admitted byte length of any single distinct TZif zone blob (64 KiB).
pub const MAX_TZDB_ZONE_BLOB_BYTES: usize = 65_536;

/// Maximum admitted zone entry count in a single tzdb bundle.
pub const MAX_TZDB_ZONES: usize = 4_096;

/// Maximum admitted release identifier length in bytes.
pub const MAX_TZDB_RELEASE_LEN: usize = 64;

/// Maximum admitted zone name length in bytes.
pub const MAX_TZDB_ZONE_NAME_LEN: usize = 255;

/// Fixed binary header size in bytes (`8 + 2 + 2 + 4 + 4 + 4`).
const HEADER_LEN: usize = 24;

/// Fixed index entry size in bytes (`4 * u32`).
const ENTRY_LEN: usize = 16;

/// Fixed RFC 8536 TZif header size in bytes.
const TZIF_HEADER_LEN: usize = 44;

/// Zone identifier from upstream `tzdb` `factory` that Norn never admits.
const FORBIDDEN_FACTORY_ZONE: &str = "Factory";

/// Built-in IANA tzdb `2025b` release identifier.
pub const TZDB_RELEASE_2025B: &str = "2025b";

/// Built-in IANA tzdb `2026c` release identifier.
pub const TZDB_RELEASE_2026C: &str = "2026c";

/// Exact SHA-256 digest of the compiled-in `2025b` bundle bytes.
pub const TZDB_SHA256_2025B: &str =
    "6b6514f7fed817ad3cf616de4532c6ec7da85929336b7a2956b8f9f40f5fbf59";

/// Exact SHA-256 digest of the compiled-in `2026c` bundle bytes.
pub const TZDB_SHA256_2026C: &str =
    "e0cda9ad9e465ee416b473b0e2e8686a468c8c66a6f279b9c7379c5650b38018";

const BUILTIN_2025B_BYTES: &[u8] = include_bytes!("../data/tzdb/2025b.tzdb");
const BUILTIN_2026C_BYTES: &[u8] = include_bytes!("../data/tzdb/2026c.tzdb");

/// Offset-free civil local wall-clock fields resolved against a [`TzdbBundle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalCivilWallTime {
    /// Four-digit proleptic-Gregorian year.
    pub year: i16,
    /// Month `1..=12`.
    pub month: i8,
    /// Day of month `1..=31`.
    pub day: i8,
    /// Hour `0..=23`.
    pub hour: i8,
    /// Minute `0..=59`.
    pub minute: i8,
    /// Second `0..=59`.
    pub second: i8,
    /// Subsecond nanoseconds `0..=999_999_999`.
    pub nanosecond: i32,
}

/// Resolution of a [`LocalCivilWallTime`] against a pre-parsed IANA zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalOffsetResolution {
    /// Single unambiguous UTC offset in seconds.
    Unambiguous {
        /// Signed UTC offset in seconds.
        offset_seconds: i32,
    },
    /// Nonexistent wall time inside a DST spring-forward gap.
    Gap,
    /// Ambiguous wall time inside a DST fall-back fold.
    Fold {
        /// Earlier (pre-transition) UTC offset in seconds.
        before_seconds: i32,
        /// Later (post-transition) UTC offset in seconds.
        after_seconds: i32,
    },
}

/// Validation error returned when decoding, admitting, or cataloging a [`TzdbBundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TzdbBundleError {
    /// Bundle exceeds [`MAX_TZDB_BUNDLE_BYTES`].
    TooLarge {
        /// Actual byte length.
        len: usize,
        /// Configured upper bound.
        max: usize,
    },
    /// Bundle is shorter than the fixed header.
    TruncatedHeader,
    /// Header magic does not equal [`TZDB_BUNDLE_MAGIC`].
    InvalidMagic,
    /// Reserved header field is non-zero.
    NonZeroReserved,
    /// Embedded release string is empty, over-long, or contains non-printable/whitespace bytes.
    InvalidRelease,
    /// Zone count is zero or exceeds [`MAX_TZDB_ZONES`].
    InvalidZoneCount {
        /// Declared zone count.
        count: u32,
    },
    /// Declared section sizes overflow or do not equal the exact bundle byte length.
    LengthMismatch {
        /// Expected byte length computed from header fields.
        expected: usize,
        /// Actual byte length supplied.
        actual: usize,
    },
    /// Zone name bytes are empty, over-long, invalid UTF-8, or contain invalid characters.
    InvalidZoneName {
        /// Zero-based index entry position.
        index: usize,
    },
    /// Index entries are not strictly ascending by case-sensitive zone name.
    UnsortedOrDuplicateZone {
        /// Zero-based index entry position.
        index: usize,
    },
    /// Bundle includes a forbidden non-IANA zone identifier (such as `Factory`).
    ForbiddenZone {
        /// Offending zone name.
        name: String,
    },
    /// A name or TZif payload slice is out of bounds or empty.
    OutOfBoundsEntry {
        /// Zero-based index entry position.
        index: usize,
    },
    /// A single zone's TZif payload exceeds [`MAX_TZDB_ZONE_BLOB_BYTES`].
    ZoneBlobTooLarge {
        /// Offending zone name.
        zone: String,
        /// Actual blob byte length.
        len: usize,
        /// Configured per-blob upper bound.
        max: usize,
    },
    /// Distinct payload slices overlap partially or leave unreferenced payload bytes.
    OverlappingOrUnreferencedPayload,
    /// A zone's TZif payload fails exact RFC 8536 framing or `jiff::tz::TimeZone::tzif` parsing.
    InvalidTzif {
        /// Zone name whose payload failed validation.
        zone: String,
    },
    /// Computed SHA-256 digest does not match the caller's expected digest.
    DigestMismatch {
        /// Expected lowercase hex SHA-256 digest.
        expected: String,
        /// Actual lowercase hex SHA-256 digest of the bytes.
        actual: String,
    },
    /// Catalog already contains `release` with a different bundle digest.
    DuplicateRelease {
        /// Conflicting IANA release identifier.
        release: String,
    },
}

impl fmt::Display for TzdbBundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { len, max } => {
                write!(f, "tzdb bundle length {len} exceeds maximum {max} bytes")
            }
            Self::TruncatedHeader => f.write_str("tzdb bundle is shorter than the 24-byte header"),
            Self::InvalidMagic => f.write_str("tzdb bundle magic does not match NTZDB01\\0"),
            Self::NonZeroReserved => f.write_str("tzdb bundle reserved header field must be zero"),
            Self::InvalidRelease => f.write_str("tzdb bundle release identifier is invalid"),
            Self::InvalidZoneCount { count } => {
                write!(f, "tzdb bundle zone count {count} is out of bounds")
            }
            Self::LengthMismatch { expected, actual } => {
                write!(
                    f,
                    "tzdb bundle length mismatch: header declares {expected} bytes, got {actual}"
                )
            }
            Self::InvalidZoneName { index } => {
                write!(f, "tzdb bundle zone name at index {index} is invalid")
            }
            Self::UnsortedOrDuplicateZone { index } => {
                write!(
                    f,
                    "tzdb bundle zone index at {index} is unsorted or duplicate"
                )
            }
            Self::ForbiddenZone { name } => {
                write!(f, "tzdb bundle contains forbidden zone `{name}`")
            }
            Self::OutOfBoundsEntry { index } => {
                write!(f, "tzdb bundle entry {index} has out-of-bounds offset/len")
            }
            Self::ZoneBlobTooLarge { zone, len, max } => {
                write!(
                    f,
                    "tzdb bundle zone `{zone}` blob length {len} exceeds maximum {max} bytes"
                )
            }
            Self::OverlappingOrUnreferencedPayload => f.write_str(
                "tzdb bundle payload slices overlap partially or leave unreferenced bytes",
            ),
            Self::InvalidTzif { zone } => {
                write!(f, "tzdb bundle zone `{zone}` has invalid TZif data")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(
                    f,
                    "tzdb bundle SHA-256 mismatch: expected {expected}, got {actual}"
                )
            }
            Self::DuplicateRelease { release } => {
                write!(
                    f,
                    "tzdb catalog already contains release `{release}` with a different digest"
                )
            }
        }
    }
}

impl std::error::Error for TzdbBundleError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ZoneEntry {
    name: String,
    tzif_offset: usize,
    tzif_len: usize,
    blob_index: usize,
}

#[derive(Debug)]
struct TzdbBundleInner {
    release: String,
    sha256: String,
    zones: Vec<ZoneEntry>,
    parsed: Arc<[TimeZone]>,
    payload: Vec<u8>,
}

impl PartialEq for TzdbBundleInner {
    fn eq(&self, other: &Self) -> bool {
        self.release == other.release
            && self.sha256 == other.sha256
            && self.zones == other.zones
            && self.payload == other.payload
    }
}

impl Eq for TzdbBundleInner {}

/// An immutable, validated offline IANA timezone database bundle.
///
/// Cloning a `TzdbBundle` is an inexpensive reference-counted handle clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TzdbBundle {
    inner: Arc<TzdbBundleInner>,
}

struct ParsedSections<'a> {
    release: &'a str,
    zone_count: usize,
    index_slice: &'a [u8],
    names_slice: &'a [u8],
    payload_slice: &'a [u8],
}

fn parse_sections(bytes: &[u8]) -> Result<ParsedSections<'_>, TzdbBundleError> {
    if bytes.len() > MAX_TZDB_BUNDLE_BYTES {
        return Err(TzdbBundleError::TooLarge {
            len: bytes.len(),
            max: MAX_TZDB_BUNDLE_BYTES,
        });
    }
    let header = bytes
        .get(..HEADER_LEN)
        .ok_or(TzdbBundleError::TruncatedHeader)?;
    if header.get(..8) != Some(TZDB_BUNDLE_MAGIC.as_slice()) {
        return Err(TzdbBundleError::InvalidMagic);
    }
    let release_len = usize::from(read_u16_le(header, 8)?);
    if read_u16_le(header, 10)? != 0 {
        return Err(TzdbBundleError::NonZeroReserved);
    }
    let zone_count_u32 = read_u32_le(header, 12)?;
    let overflow_err = || TzdbBundleError::LengthMismatch {
        expected: usize::MAX,
        actual: bytes.len(),
    };
    let names_len = usize::try_from(read_u32_le(header, 16)?).map_err(|_err| overflow_err())?;
    let payload_len = usize::try_from(read_u32_le(header, 20)?).map_err(|_err| overflow_err())?;

    if release_len == 0 || release_len > MAX_TZDB_RELEASE_LEN {
        return Err(TzdbBundleError::InvalidRelease);
    }
    let zone_count =
        usize::try_from(zone_count_u32).map_err(|_err| TzdbBundleError::InvalidZoneCount {
            count: zone_count_u32,
        })?;
    if zone_count == 0 || zone_count > MAX_TZDB_ZONES {
        return Err(TzdbBundleError::InvalidZoneCount {
            count: zone_count_u32,
        });
    }

    let release_end = HEADER_LEN
        .checked_add(release_len)
        .ok_or_else(overflow_err)?;
    let index_bytes = zone_count.checked_mul(ENTRY_LEN).ok_or_else(overflow_err)?;
    let index_end = release_end
        .checked_add(index_bytes)
        .ok_or_else(overflow_err)?;
    let names_end = index_end.checked_add(names_len).ok_or_else(overflow_err)?;
    let payload_end = names_end
        .checked_add(payload_len)
        .ok_or_else(overflow_err)?;
    if payload_end != bytes.len() {
        return Err(TzdbBundleError::LengthMismatch {
            expected: payload_end,
            actual: bytes.len(),
        });
    }

    let release_slice = bytes
        .get(HEADER_LEN..release_end)
        .ok_or(TzdbBundleError::InvalidRelease)?;
    let release =
        std::str::from_utf8(release_slice).map_err(|_err| TzdbBundleError::InvalidRelease)?;
    if !is_valid_ascii_token(release) {
        return Err(TzdbBundleError::InvalidRelease);
    }

    Ok(ParsedSections {
        release,
        zone_count,
        index_slice: bytes
            .get(release_end..index_end)
            .ok_or(TzdbBundleError::TruncatedHeader)?,
        names_slice: bytes
            .get(index_end..names_end)
            .ok_or(TzdbBundleError::TruncatedHeader)?,
        payload_slice: bytes
            .get(names_end..payload_end)
            .ok_or(TzdbBundleError::TruncatedHeader)?,
    })
}

fn parse_zone_entries(
    sections: &ParsedSections<'_>,
) -> Result<(Vec<ZoneEntry>, Arc<[TimeZone]>), TzdbBundleError> {
    let mut zones = Vec::with_capacity(sections.zone_count);
    let mut blob_indices: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut parsed_zones: Vec<TimeZone> = Vec::new();
    let mut previous_name: Option<String> = None;

    for index in 0..sections.zone_count {
        let entry_start = index
            .checked_mul(ENTRY_LEN)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        let entry_end = entry_start
            .checked_add(ENTRY_LEN)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        let entry = sections
            .index_slice
            .get(entry_start..entry_end)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;

        let name_offset = usize::try_from(read_u32_le(entry, 0)?)
            .map_err(|_err| TzdbBundleError::OutOfBoundsEntry { index })?;
        let name_len = usize::try_from(read_u32_le(entry, 4)?)
            .map_err(|_err| TzdbBundleError::OutOfBoundsEntry { index })?;
        let tzif_offset = usize::try_from(read_u32_le(entry, 8)?)
            .map_err(|_err| TzdbBundleError::OutOfBoundsEntry { index })?;
        let tzif_len = usize::try_from(read_u32_le(entry, 12)?)
            .map_err(|_err| TzdbBundleError::OutOfBoundsEntry { index })?;

        let name = decode_zone_name(sections.names_slice, index, name_offset, name_len)?;
        if let Some(prev) = &previous_name
            && name <= prev.as_str()
        {
            return Err(TzdbBundleError::UnsortedOrDuplicateZone { index });
        }
        previous_name = Some(name.to_owned());

        if tzif_len == 0 {
            return Err(TzdbBundleError::OutOfBoundsEntry { index });
        }
        if tzif_len > MAX_TZDB_ZONE_BLOB_BYTES {
            return Err(TzdbBundleError::ZoneBlobTooLarge {
                zone: name.to_owned(),
                len: tzif_len,
                max: MAX_TZDB_ZONE_BLOB_BYTES,
            });
        }
        let tzif_end = tzif_offset
            .checked_add(tzif_len)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        let tzif_bytes = sections
            .payload_slice
            .get(tzif_offset..tzif_end)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;

        let blob_index = match blob_indices.get(&(tzif_offset, tzif_len)) {
            Some(&existing) => existing,
            None => {
                if !validate_tzif_exact_framing(tzif_bytes) {
                    return Err(TzdbBundleError::InvalidTzif {
                        zone: name.to_owned(),
                    });
                }
                let tz = TimeZone::tzif(name, tzif_bytes).map_err(|_err| {
                    TzdbBundleError::InvalidTzif {
                        zone: name.to_owned(),
                    }
                })?;
                let next_idx = parsed_zones.len();
                parsed_zones.push(tz);
                blob_indices.insert((tzif_offset, tzif_len), next_idx);
                next_idx
            }
        };

        zones.push(ZoneEntry {
            name: name.to_owned(),
            tzif_offset,
            tzif_len,
            blob_index,
        });
    }

    verify_exact_payload_partition(&blob_indices, sections.payload_slice.len())?;
    Ok((zones, Arc::from(parsed_zones)))
}

fn decode_zone_name(
    names_slice: &[u8],
    index: usize,
    name_offset: usize,
    name_len: usize,
) -> Result<&str, TzdbBundleError> {
    if name_len == 0 || name_len > MAX_TZDB_ZONE_NAME_LEN {
        return Err(TzdbBundleError::InvalidZoneName { index });
    }
    let name_end = name_offset
        .checked_add(name_len)
        .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
    let name_bytes = names_slice
        .get(name_offset..name_end)
        .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
    let name = std::str::from_utf8(name_bytes)
        .map_err(|_err| TzdbBundleError::InvalidZoneName { index })?;
    if !is_valid_ascii_token(name) {
        return Err(TzdbBundleError::InvalidZoneName { index });
    }
    if name == FORBIDDEN_FACTORY_ZONE {
        return Err(TzdbBundleError::ForbiddenZone {
            name: name.to_owned(),
        });
    }
    Ok(name)
}

fn verify_exact_payload_partition(
    blob_indices: &BTreeMap<(usize, usize), usize>,
    payload_len: usize,
) -> Result<(), TzdbBundleError> {
    let mut cursor = 0_usize;
    for &(offset, len) in blob_indices.keys() {
        if offset != cursor {
            return Err(TzdbBundleError::OverlappingOrUnreferencedPayload);
        }
        cursor = cursor
            .checked_add(len)
            .ok_or(TzdbBundleError::OverlappingOrUnreferencedPayload)?;
    }
    if cursor != payload_len {
        return Err(TzdbBundleError::OverlappingOrUnreferencedPayload);
    }
    Ok(())
}

/// Validates RFC 8536 TZif framing so zero trailing bytes may hide after the
/// TZif V1 data block or V2/V3/V4 POSIX TZ footer newline.
fn validate_tzif_exact_framing(data: &[u8]) -> bool {
    let Some((ver, v1_end)) = parse_tzif_block(data, 0, false) else {
        return false;
    };
    if ver == 0 {
        return v1_end == data.len();
    }
    let Some((ver2, v2_end)) = parse_tzif_block(data, v1_end, true) else {
        return false;
    };
    if ver2 != ver {
        return false;
    }
    let Some(footer) = data.get(v2_end..) else {
        return false;
    };
    if footer.len() < 2 || footer.first() != Some(&b'\n') || footer.last() != Some(&b'\n') {
        return false;
    }
    let Some(interior_end) = footer.len().checked_sub(1) else {
        return false;
    };
    let Some(interior) = footer.get(1..interior_end) else {
        return false;
    };
    !interior.contains(&b'\n') && !interior.contains(&0)
}

fn parse_tzif_block(data: &[u8], start: usize, v2: bool) -> Option<(u8, usize)> {
    let header_end = start.checked_add(TZIF_HEADER_LEN)?;
    let header = data.get(start..header_end)?;
    if header.get(..4) != Some(b"TZif".as_slice()) {
        return None;
    }
    let version = *header.get(4)?;
    if !matches!(version, 0 | b'2' | b'3' | b'4') {
        return None;
    }
    let isut = usize::try_from(read_u32_be(header, 20)?).ok()?;
    let isstd = usize::try_from(read_u32_be(header, 24)?).ok()?;
    let leap = usize::try_from(read_u32_be(header, 28)?).ok()?;
    let time_cnt = usize::try_from(read_u32_be(header, 32)?).ok()?;
    let type_cnt = usize::try_from(read_u32_be(header, 36)?).ok()?;
    let char_cnt = usize::try_from(read_u32_be(header, 40)?).ok()?;
    if type_cnt == 0 {
        return None;
    }
    let time_bytes = time_cnt.checked_mul(if v2 { 8 } else { 4 })?;
    let leap_bytes = leap.checked_mul(if v2 { 12 } else { 8 })?;
    let type_bytes = type_cnt.checked_mul(6)?;
    let block_end = header_end
        .checked_add(time_bytes)?
        .checked_add(time_cnt)?
        .checked_add(type_bytes)?
        .checked_add(char_cnt)?
        .checked_add(leap_bytes)?
        .checked_add(isstd)?
        .checked_add(isut)?;
    if block_end > data.len() {
        return None;
    }
    Some((version, block_end))
}

impl TzdbBundle {
    /// Decodes and validates an offline tzdb bundle from raw bytes.
    ///
    /// Performs checked arithmetic, enforces bundle and per-blob size bounds,
    /// checks strictly sorted unique case-sensitive zone names, refuses
    /// `Factory`, verifies exact non-overlapping payload partitioning, checks
    /// exact RFC 8536 TZif framing, pre-parses every distinct `TZif` blob once
    /// with `jiff::tz::TimeZone::tzif`, and records the exact SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns [`TzdbBundleError`] if any header, index, bound, partition, or
    /// `TZif` invariant fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TzdbBundleError> {
        Self::decode_inner(bytes, None)
    }

    /// Decodes and validates a bundle, enforcing [`MAX_TZDB_BUNDLE_BYTES`] and
    /// requiring its SHA-256 digest to match `expected_sha256` (lowercase
    /// hexadecimal) before decoding, without hashing the payload twice.
    ///
    /// # Errors
    ///
    /// Returns [`TzdbBundleError::TooLarge`], [`TzdbBundleError::DigestMismatch`],
    /// or any decoding error from [`Self::from_bytes`].
    pub fn from_bytes_verified(
        bytes: &[u8],
        expected_sha256: &str,
    ) -> Result<Self, TzdbBundleError> {
        if bytes.len() > MAX_TZDB_BUNDLE_BYTES {
            return Err(TzdbBundleError::TooLarge {
                len: bytes.len(),
                max: MAX_TZDB_BUNDLE_BYTES,
            });
        }
        let actual = HashProfile::Legacy0.hash_canonical_bytes(bytes);
        if actual != expected_sha256 {
            return Err(TzdbBundleError::DigestMismatch {
                expected: expected_sha256.to_owned(),
                actual,
            });
        }
        Self::decode_inner(bytes, Some(actual))
    }

    fn decode_inner(
        bytes: &[u8],
        precomputed_sha256: Option<String>,
    ) -> Result<Self, TzdbBundleError> {
        let sections = parse_sections(bytes)?;
        let (zones, parsed) = parse_zone_entries(&sections)?;
        let sha256 =
            precomputed_sha256.unwrap_or_else(|| HashProfile::Legacy0.hash_canonical_bytes(bytes));
        Ok(Self {
            inner: Arc::new(TzdbBundleInner {
                release: sections.release.to_owned(),
                sha256,
                zones,
                parsed,
                payload: sections.payload_slice.to_vec(),
            }),
        })
    }

    /// Returns the compiled-in IANA tzdb `2025b` bundle (operational default).
    #[must_use]
    pub fn v2025b() -> &'static Self {
        static BUNDLE: OnceLock<TzdbBundle> = OnceLock::new();
        BUNDLE.get_or_init(|| {
            Self::from_bytes_verified(BUILTIN_2025B_BYTES, TZDB_SHA256_2025B)
                .unwrap_or_else(|_err| Self::fail_closed_empty())
        })
    }

    /// Returns the compiled-in IANA tzdb `2026c` bundle.
    #[must_use]
    pub fn v2026c() -> &'static Self {
        static BUNDLE: OnceLock<TzdbBundle> = OnceLock::new();
        BUNDLE.get_or_init(|| {
            Self::from_bytes_verified(BUILTIN_2026C_BYTES, TZDB_SHA256_2026C)
                .unwrap_or_else(|_err| Self::fail_closed_empty())
        })
    }

    /// Returns the operational default tzdb bundle (`2025b`).
    #[must_use]
    pub fn default_bundle() -> &'static Self {
        Self::v2025b()
    }

    fn fail_closed_empty() -> Self {
        Self {
            inner: Arc::new(TzdbBundleInner {
                release: String::new(),
                sha256: String::new(),
                zones: Vec::new(),
                parsed: Arc::from([]),
                payload: Vec::new(),
            }),
        }
    }

    /// Returns the embedded IANA release identifier (for example `"2025b"`).
    #[must_use]
    pub fn release(&self) -> &str {
        &self.inner.release
    }

    /// Returns the lowercase hexadecimal SHA-256 digest of the encoded bundle bytes.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.inner.sha256
    }

    /// Returns the number of admitted IANA zone identifiers in this bundle.
    #[must_use]
    pub fn zone_count(&self) -> usize {
        self.inner.zones.len()
    }

    /// Iterates over all admitted zone identifiers in sorted order.
    #[must_use]
    pub fn zone_names(&self) -> impl Iterator<Item = &str> {
        self.inner.zones.iter().map(|entry| entry.name.as_str())
    }

    fn zone_entry(&self, name: &str) -> Option<&ZoneEntry> {
        if name == FORBIDDEN_FACTORY_ZONE {
            return None;
        }
        let idx = self
            .inner
            .zones
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .ok()?;
        self.inner.zones.get(idx)
    }

    /// Looks up the raw TZif bytes for an exact case-sensitive IANA zone name.
    #[must_use]
    pub fn tzif(&self, name: &str) -> Option<&[u8]> {
        let entry = self.zone_entry(name)?;
        let end = entry.tzif_offset.checked_add(entry.tzif_len)?;
        self.inner.payload.get(entry.tzif_offset..end)
    }

    /// Returns whether `name` is an exact case-sensitive zone in this bundle.
    #[must_use]
    pub fn contains_zone(&self, name: &str) -> bool {
        self.zone_entry(name).is_some()
    }

    /// Resolves an offset-free civil local wall time in exact case-sensitive
    /// zone `name` against the timezone pre-parsed at bundle admission (zero
    /// runtime TZif decoding).
    #[must_use]
    pub fn resolve_local_offset(
        &self,
        name: &str,
        wall: LocalCivilWallTime,
    ) -> Option<LocalOffsetResolution> {
        let tz = self.parsed_timezone(name)?;
        let dt = DateTime::new(
            wall.year,
            wall.month,
            wall.day,
            wall.hour,
            wall.minute,
            wall.second,
            wall.nanosecond,
        )
        .ok()?;
        Some(match tz.to_ambiguous_timestamp(dt).offset() {
            AmbiguousOffset::Unambiguous { offset } => LocalOffsetResolution::Unambiguous {
                offset_seconds: offset.seconds(),
            },
            AmbiguousOffset::Gap { .. } => LocalOffsetResolution::Gap,
            AmbiguousOffset::Fold { before, after } => LocalOffsetResolution::Fold {
                before_seconds: before.seconds(),
                after_seconds: after.seconds(),
            },
        })
    }

    pub(crate) fn parsed_timezone(&self, name: &str) -> Option<&TimeZone> {
        let entry = self.zone_entry(name)?;
        self.inner.parsed.get(entry.blob_index)
    }

    pub(crate) fn timezone(&self, name: &str) -> Result<&TimeZone, TemporalError> {
        self.parsed_timezone(name)
            .ok_or(TemporalError::InvalidTimezone)
    }
}

/// An immutable catalog of validated [`TzdbBundle`] releases indexed by release id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TzdbCatalog {
    bundles: BTreeMap<String, TzdbBundle>,
}

impl Default for TzdbCatalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl TzdbCatalog {
    /// Returns the catalog populated with the compiled-in `2025b` and `2026c` bundles.
    #[must_use]
    pub fn builtin() -> Self {
        let mut bundles = BTreeMap::new();
        let b2025b = TzdbBundle::v2025b().clone();
        let b2026c = TzdbBundle::v2026c().clone();
        bundles.insert(b2025b.release().to_owned(), b2025b);
        bundles.insert(b2026c.release().to_owned(), b2026c);
        Self { bundles }
    }

    /// Returns a catalog containing `bundle` in addition to existing entries.
    ///
    /// Release identities are immutable: adding the same `(release, sha256)`
    /// pair is idempotent, while attempting to overwrite an existing release
    /// with a different digest is refused with [`TzdbBundleError::DuplicateRelease`].
    ///
    /// # Errors
    ///
    /// Returns [`TzdbBundleError::InvalidRelease`] if `bundle.release()` is
    /// empty, or [`TzdbBundleError::DuplicateRelease`] if `bundle.release()` is
    /// already present with a different SHA-256 digest.
    pub fn with_bundle(mut self, bundle: TzdbBundle) -> Result<Self, TzdbBundleError> {
        if bundle.release().is_empty() {
            return Err(TzdbBundleError::InvalidRelease);
        }
        if let Some(existing) = self.bundles.get(bundle.release()) {
            if existing.sha256() == bundle.sha256() {
                return Ok(self);
            }
            return Err(TzdbBundleError::DuplicateRelease {
                release: bundle.release().to_owned(),
            });
        }
        self.bundles.insert(bundle.release().to_owned(), bundle);
        Ok(self)
    }

    /// Looks up a bundle by exact release string (for example `"2025b"` or `"2026c"`).
    #[must_use]
    pub fn get(&self, release: &str) -> Option<&TzdbBundle> {
        self.bundles.get(release)
    }

    /// Looks up a bundle by exact release string and expected SHA-256 digest.
    #[must_use]
    pub fn get_verified(&self, release: &str, expected_sha256: &str) -> Option<&TzdbBundle> {
        self.bundles
            .get(release)
            .filter(|bundle| bundle.sha256() == expected_sha256)
    }

    /// Looks up a bundle by its exact lowercase hex SHA-256 digest.
    #[must_use]
    pub fn get_by_sha256(&self, sha256: &str) -> Option<&TzdbBundle> {
        self.bundles
            .values()
            .find(|bundle| bundle.sha256() == sha256)
    }

    /// Iterates over the catalog's release identifiers in sorted order.
    #[must_use]
    pub fn releases(&self) -> impl Iterator<Item = &str> {
        self.bundles.keys().map(String::as_str)
    }

    /// Returns the operational default bundle (`2025b`).
    #[must_use]
    pub fn default_bundle(&self) -> &TzdbBundle {
        self.bundles
            .get(TZDB_RELEASE_2025B)
            .unwrap_or_else(TzdbBundle::v2025b)
    }
}

fn read_u16_le(slice: &[u8], offset: usize) -> Result<u16, TzdbBundleError> {
    let end = offset
        .checked_add(2)
        .ok_or(TzdbBundleError::TruncatedHeader)?;
    let bytes: [u8; 2] = slice
        .get(offset..end)
        .and_then(|raw| raw.try_into().ok())
        .ok_or(TzdbBundleError::TruncatedHeader)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(slice: &[u8], offset: usize) -> Result<u32, TzdbBundleError> {
    let end = offset
        .checked_add(4)
        .ok_or(TzdbBundleError::TruncatedHeader)?;
    let bytes: [u8; 4] = slice
        .get(offset..end)
        .and_then(|raw| raw.try_into().ok())
        .ok_or(TzdbBundleError::TruncatedHeader)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u32_be(slice: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes: [u8; 4] = slice.get(offset..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn is_valid_ascii_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
}
