//! HOST-TREE-1: the source-tree digest a host Function's
//! `implementation.digest` pins (FUNCTION-HOST-1, Core 07).
//!
//! The tree is a set of regular files, each named by a relative `/`-separated
//! UTF-8 path. Every file contributes its path, its byte length and the
//! SHA-256 of its exact bytes (no line-ending or encoding normalization). The
//! entries are sorted by path bytes and the digest is the Norn1 canonical-JSON
//! hash of that list under [`SOURCE_TREE_DOMAIN`], so the same files always
//! give the same digest and any added, removed, renamed or edited file changes
//! it. This module is pure; walking a directory belongs to the caller
//! (`norn source-digest`).

use serde::Serialize;

use crate::function::Digest;

/// Domain separator of the source-tree digest.
pub const SOURCE_TREE_DOMAIN: &[u8] = b"norn.function/source-tree/v1\0";

/// Why a set of files has no source-tree digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTreeError {
    /// A tree has at least one file.
    Empty,
    /// A path that is absolute, empty, has an empty, `.` or `..` component,
    /// or contains a backslash or control character.
    InvalidPath(String),
    /// The same path appears twice.
    DuplicatePath(String),
    /// Canonical encoding failed.
    Canonical,
}

impl SourceTreeError {
    /// Stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Empty => "function.source_tree.empty",
            Self::InvalidPath(_) => "function.source_tree.invalid_path",
            Self::DuplicatePath(_) => "function.source_tree.duplicate_path",
            Self::Canonical => "function.source_tree.canonical",
        }
    }
}

impl std::fmt::Display for SourceTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) | Self::DuplicatePath(path) => {
                write!(f, "{}: {path:?}", self.code())
            }
            Self::Empty | Self::Canonical => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for SourceTreeError {}

#[derive(Serialize)]
struct Entry<'a> {
    path: &'a str,
    size: u64,
    sha256: String,
}

/// Whether `path` is a valid tree path (see [`SourceTreeError::InvalidPath`]).
#[must_use]
pub fn is_valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.chars().any(|c| c == '\\' || c.is_control())
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The HOST-TREE-1 digest of `files` (path, exact bytes), in any order.
///
/// # Errors
/// An empty tree, an invalid or duplicate path, or a canonicalization failure.
pub fn source_tree_digest<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Digest, SourceTreeError> {
    let mut entries = Vec::new();
    for (path, bytes) in files {
        if !is_valid_path(path) {
            return Err(SourceTreeError::InvalidPath(path.to_owned()));
        }
        entries.push(Entry {
            path,
            size: u64::try_from(bytes.len()).map_err(|_overflow| SourceTreeError::Canonical)?,
            sha256: norn_canonical::HashProfile::Legacy0.hash_canonical_bytes(bytes),
        });
    }
    if entries.is_empty() {
        return Err(SourceTreeError::Empty);
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    if let Some(path) = entries.windows(2).find_map(|pair| match pair {
        [left, right] if left.path == right.path => Some(left.path),
        _ => None,
    }) {
        return Err(SourceTreeError::DuplicatePath(path.to_owned()));
    }
    norn_canonical::domain_hash_hex(SOURCE_TREE_DOMAIN, &entries)
        .map(|hex| Digest::from_hex(&hex))
        .map_err(|_canonical| SourceTreeError::Canonical)
}
