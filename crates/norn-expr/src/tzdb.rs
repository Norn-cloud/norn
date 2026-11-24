//! Versioned offline IANA timezone database bundles (`#1440`).
//!
//! Each bundle is a deterministic, self-contained binary image (`NTZDB01\0`)
//! carrying an embedded IANA release identifier, a strictly sorted case-sensitive
//! zone index sharing deduplicated TZif byte ranges across aliases, and the raw
//! TZif payload section. The same checked decoder admits both compiled-in
//! bundles (`2025b` and `2026c`) and runtime/cell-installed bundle bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use jiff::tz::TimeZone;
use norn_ir::canonical::HashProfile;

use crate::temporal::TemporalError;

/// Versioned magic prefix of a Norn offline tzdb bundle (`NTZDB01\0`).
pub const TZDB_BUNDLE_MAGIC: &[u8; 8] = b"NTZDB01\0";

/// Maximum admitted byte length of a single tzdb bundle (4 MiB).
pub const MAX_TZDB_BUNDLE_BYTES: usize = 4_194_304;

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

/// Validation error returned when decoding or admitting a [`TzdbBundle`].
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
    /// A zone's TZif payload fails `jiff::tz::TimeZone::tzif` parsing.
    InvalidTzif {
        /// Zone name whose payload failed TZif parsing.
        zone: String,
    },
    /// Computed SHA-256 digest does not match the caller's expected digest.
    DigestMismatch {
        /// Expected lowercase hex SHA-256 digest.
        expected: String,
        /// Actual lowercase hex SHA-256 digest of the bytes.
        actual: String,
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
            Self::InvalidTzif { zone } => {
                write!(f, "tzdb bundle zone `{zone}` has invalid TZif data")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(
                    f,
                    "tzdb bundle SHA-256 mismatch: expected {expected}, got {actual}"
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
}

#[derive(Debug, PartialEq, Eq)]
struct TzdbBundleInner {
    release: String,
    sha256: String,
    zones: Vec<ZoneEntry>,
    payload: Vec<u8>,
}

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

fn parse_zone_entries(sections: &ParsedSections<'_>) -> Result<Vec<ZoneEntry>, TzdbBundleError> {
    let mut zones = Vec::with_capacity(sections.zone_count);
    let mut verified_blobs: BTreeSet<(usize, usize)> = BTreeSet::new();
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

        if name_len == 0 || name_len > MAX_TZDB_ZONE_NAME_LEN {
            return Err(TzdbBundleError::InvalidZoneName { index });
        }
        let name_end = name_offset
            .checked_add(name_len)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        let name_bytes = sections
            .names_slice
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
        if let Some(prev) = &previous_name
            && name <= prev.as_str()
        {
            return Err(TzdbBundleError::UnsortedOrDuplicateZone { index });
        }
        previous_name = Some(name.to_owned());

        if tzif_len == 0 {
            return Err(TzdbBundleError::OutOfBoundsEntry { index });
        }
        let tzif_end = tzif_offset
            .checked_add(tzif_len)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        let tzif_bytes = sections
            .payload_slice
            .get(tzif_offset..tzif_end)
            .ok_or(TzdbBundleError::OutOfBoundsEntry { index })?;
        if verified_blobs.insert((tzif_offset, tzif_len)) {
            TimeZone::tzif(name, tzif_bytes).map_err(|_err| TzdbBundleError::InvalidTzif {
                zone: name.to_owned(),
            })?;
        }

        zones.push(ZoneEntry {
            name: name.to_owned(),
            tzif_offset,
            tzif_len,
        });
    }

    Ok(zones)
}

impl TzdbBundle {
    /// Decodes and validates an offline tzdb bundle from raw bytes.
    ///
    /// Performs checked arithmetic, enforces size and entry bounds, checks
    /// strictly sorted unique case-sensitive zone names, refuses `Factory`,
    /// verifies every distinct `TZif` payload with `jiff::tz::TimeZone::tzif`,
    /// and records the exact SHA-256 digest of `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`TzdbBundleError`] if any header, index, bound, or `TZif`
    /// invariant fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TzdbBundleError> {
        let sections = parse_sections(bytes)?;
        let zones = parse_zone_entries(&sections)?;
        let sha256 = HashProfile::Legacy0.hash_canonical_bytes(bytes);
        Ok(Self {
            inner: Arc::new(TzdbBundleInner {
                release: sections.release.to_owned(),
                sha256,
                zones,
                payload: sections.payload_slice.to_vec(),
            }),
        })
    }

    /// Decodes and validates a bundle, requiring its SHA-256 digest to match
    /// `expected_sha256` (lowercase hexadecimal) before decoding.
    ///
    /// # Errors
    ///
    /// Returns [`TzdbBundleError::DigestMismatch`] when the digest differs, or
    /// any decoding error from [`Self::from_bytes`].
    pub fn from_bytes_verified(
        bytes: &[u8],
        expected_sha256: &str,
    ) -> Result<Self, TzdbBundleError> {
        let actual = HashProfile::Legacy0.hash_canonical_bytes(bytes);
        if actual != expected_sha256 {
            return Err(TzdbBundleError::DigestMismatch {
                expected: expected_sha256.to_owned(),
                actual,
            });
        }
        Self::from_bytes(bytes)
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

    /// Looks up the raw TZif bytes for an exact case-sensitive IANA zone name.
    #[must_use]
    pub fn tzif(&self, name: &str) -> Option<&[u8]> {
        if name == FORBIDDEN_FACTORY_ZONE {
            return None;
        }
        let idx = self
            .inner
            .zones
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .ok()?;
        let entry = self.inner.zones.get(idx)?;
        let end = entry.tzif_offset.checked_add(entry.tzif_len)?;
        self.inner.payload.get(entry.tzif_offset..end)
    }

    /// Returns whether `name` is an exact case-sensitive zone in this bundle.
    #[must_use]
    pub fn contains_zone(&self, name: &str) -> bool {
        self.tzif(name).is_some()
    }

    pub(crate) fn timezone(&self, name: &str) -> Result<TimeZone, TemporalError> {
        let data = self.tzif(name).ok_or(TemporalError::InvalidTimezone)?;
        TimeZone::tzif(name, data).map_err(|_err| TemporalError::InvalidTimezone)
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
    #[must_use]
    pub fn with_bundle(mut self, bundle: TzdbBundle) -> Self {
        self.bundles.insert(bundle.release().to_owned(), bundle);
        self
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

fn is_valid_ascii_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::{
        BUILTIN_2025B_BYTES, BUILTIN_2026C_BYTES, MAX_TZDB_BUNDLE_BYTES, TZDB_BUNDLE_MAGIC,
        TZDB_RELEASE_2025B, TZDB_RELEASE_2026C, TZDB_SHA256_2025B, TZDB_SHA256_2026C, TzdbBundle,
        TzdbBundleError, TzdbCatalog,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn encode_synthetic(
        release: &str,
        entries: &[(&str, &[u8])],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let release_bytes = release.as_bytes();
        let mut names = Vec::new();
        let mut payload = Vec::new();
        let mut index = Vec::new();
        for (name, tzif) in entries {
            let name_off = u32::try_from(names.len())?;
            let name_len = u32::try_from(name.len())?;
            names.extend_from_slice(name.as_bytes());
            let tzif_off = u32::try_from(payload.len())?;
            let tzif_len = u32::try_from(tzif.len())?;
            payload.extend_from_slice(tzif);
            index.extend_from_slice(&name_off.to_le_bytes());
            index.extend_from_slice(&name_len.to_le_bytes());
            index.extend_from_slice(&tzif_off.to_le_bytes());
            index.extend_from_slice(&tzif_len.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(TZDB_BUNDLE_MAGIC);
        out.extend_from_slice(&u16::try_from(release_bytes.len())?.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&u32::try_from(entries.len())?.to_le_bytes());
        out.extend_from_slice(&u32::try_from(names.len())?.to_le_bytes());
        out.extend_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
        out.extend_from_slice(release_bytes);
        out.extend_from_slice(&index);
        out.extend_from_slice(&names);
        out.extend_from_slice(&payload);
        Ok(out)
    }

    #[test]
    fn builtin_bundles_decode_with_exact_releases_digests_and_597_zones() -> TestResult {
        let b25 = TzdbBundle::v2025b();
        let b26 = TzdbBundle::v2026c();
        assert_eq!(b25.release(), TZDB_RELEASE_2025B);
        assert_eq!(b26.release(), TZDB_RELEASE_2026C);
        assert_eq!(b25.sha256(), TZDB_SHA256_2025B);
        assert_eq!(b26.sha256(), TZDB_SHA256_2026C);
        assert_eq!(b25.zone_count(), 597);
        assert_eq!(b26.zone_count(), 597);
        assert!(!b25.contains_zone("Factory"));
        assert!(!b26.contains_zone("Factory"));
        assert!(b25.contains_zone("America/Edmonton"));
        assert!(b26.contains_zone("America/Edmonton"));

        let decoded_25 = TzdbBundle::from_bytes_verified(BUILTIN_2025B_BYTES, TZDB_SHA256_2025B)?;
        let decoded_26 = TzdbBundle::from_bytes_verified(BUILTIN_2026C_BYTES, TZDB_SHA256_2026C)?;
        assert_eq!(&decoded_25, b25);
        assert_eq!(&decoded_26, b26);

        let catalog = TzdbCatalog::builtin();
        assert_eq!(
            catalog.releases().collect::<Vec<_>>(),
            vec!["2025b", "2026c"]
        );
        assert_eq!(catalog.get("2025b"), Some(b25));
        assert_eq!(catalog.get_verified("2026c", TZDB_SHA256_2026C), Some(b26));
        assert_eq!(catalog.get_verified("2026c", TZDB_SHA256_2025B), None);
        assert_eq!(catalog.get_by_sha256(TZDB_SHA256_2025B), Some(b25));
        assert_eq!(catalog.default_bundle(), b25);
        Ok(())
    }

    #[test]
    fn decoder_rejects_malformed_bundles() -> TestResult {
        let utc_tzif = TzdbBundle::v2025b().tzif("UTC").ok_or("missing UTC tzif")?;

        // Valid synthetic bundle passes.
        let valid = encode_synthetic("2099z", &[("Etc/UTC", utc_tzif), ("UTC", utc_tzif)])?;
        let parsed = TzdbBundle::from_bytes(&valid)?;
        assert_eq!(parsed.release(), "2099z");
        assert_eq!(parsed.zone_count(), 2);

        // Digest mismatch.
        assert!(matches!(
            TzdbBundle::from_bytes_verified(&valid, &"0".repeat(64)),
            Err(TzdbBundleError::DigestMismatch { .. })
        ));

        // Bad magic.
        let mut bad_magic = valid.clone();
        *bad_magic.get_mut(0).ok_or("missing byte 0")? = b'X';
        assert_eq!(
            TzdbBundle::from_bytes(&bad_magic),
            Err(TzdbBundleError::InvalidMagic)
        );

        // Truncated header.
        assert_eq!(
            TzdbBundle::from_bytes(b"NTZDB01"),
            Err(TzdbBundleError::TruncatedHeader)
        );

        // Non-zero reserved field.
        let mut bad_reserved = valid.clone();
        *bad_reserved.get_mut(10).ok_or("missing byte 10")? = 1;
        assert_eq!(
            TzdbBundle::from_bytes(&bad_reserved),
            Err(TzdbBundleError::NonZeroReserved)
        );

        // Trailing garbage byte (length mismatch).
        let mut trailing = valid;
        trailing.push(0);
        assert!(matches!(
            TzdbBundle::from_bytes(&trailing),
            Err(TzdbBundleError::LengthMismatch { .. })
        ));

        // Unsorted zone names.
        let unsorted = encode_synthetic("2099z", &[("UTC", utc_tzif), ("Etc/UTC", utc_tzif)])?;
        assert_eq!(
            TzdbBundle::from_bytes(&unsorted),
            Err(TzdbBundleError::UnsortedOrDuplicateZone { index: 1 })
        );

        // Duplicate zone names.
        let duplicate = encode_synthetic("2099z", &[("UTC", utc_tzif), ("UTC", utc_tzif)])?;
        assert_eq!(
            TzdbBundle::from_bytes(&duplicate),
            Err(TzdbBundleError::UnsortedOrDuplicateZone { index: 1 })
        );

        // Forbidden Factory entry.
        let factory = encode_synthetic("2099z", &[("Factory", utc_tzif)])?;
        assert_eq!(
            TzdbBundle::from_bytes(&factory),
            Err(TzdbBundleError::ForbiddenZone {
                name: "Factory".to_owned()
            })
        );

        // Invalid TZif payload.
        let bad_tzif = encode_synthetic("2099z", &[("UTC", b"not-a-valid-tzif-payload")])?;
        assert_eq!(
            TzdbBundle::from_bytes(&bad_tzif),
            Err(TzdbBundleError::InvalidTzif {
                zone: "UTC".to_owned()
            })
        );

        // Out-of-bounds TZif offset/len.
        let mut oob = encode_synthetic("2099z", &[("UTC", utc_tzif)])?;
        // Header (24) + release (5) + index entry offset 12 (tzif_len) -> bytes 41..45.
        let bad_len = u32::try_from(utc_tzif.len())?
            .checked_add(1)
            .ok_or("u32 overflow")?
            .to_le_bytes();
        oob.get_mut(41..45)
            .ok_or("missing tzif_len slot")?
            .copy_from_slice(&bad_len);
        assert_eq!(
            TzdbBundle::from_bytes(&oob),
            Err(TzdbBundleError::OutOfBoundsEntry { index: 0 })
        );

        // Exceeds MAX_TZDB_BUNDLE_BYTES.
        let oversized = vec![0_u8; MAX_TZDB_BUNDLE_BYTES.checked_add(1).ok_or("size + 1")?];
        assert_eq!(
            TzdbBundle::from_bytes(&oversized),
            Err(TzdbBundleError::TooLarge {
                len: oversized.len(),
                max: MAX_TZDB_BUNDLE_BYTES,
            })
        );
        Ok(())
    }
}
