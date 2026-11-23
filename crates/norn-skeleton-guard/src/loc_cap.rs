//! Per-file non-test LOC ceiling with a shrink-only allowlist (`loc-baseline.json`).
//!
//! Every non-test `.rs` source file under `crates/` must stay at or below
//! [`MAX_NON_TEST_LOC`] (1,000) non-test lines, unless it is grandfathered in
//! `crates/norn-skeleton-guard/loc-baseline.json` at its frozen ceiling.
//!
//! - New files exceeding [`MAX_NON_TEST_LOC`] fail closed (`loc-cap-exceeded`).
//! - Grandfathered files growing by even +1 non-test line above their frozen
//!   ceiling fail closed (`loc-cap-exceeded`).
//! - Grandfathered files shrinking at or below their frozen ceiling are
//!   accepted so normal cleanup PRs do not require a gated edit to
//!   `crates/norn-skeleton-guard/loc-baseline.json`.
//! - `scripts/baseline-shrink-only.sh` ensures `loc-baseline.json` can never
//!   gain new files, raise `max_non_test_loc`, or raise any recorded ceiling.
//!
//! # Non-test LOC definition
//!
//! Files under any `tests/` path component and files reached exclusively
//! through `#[cfg(test)] mod ...;` declarations (discovered with the same
//! module-graph walk as [`crate::ratchet`]) are test files and are excluded.
//! Inside every remaining `.rs` file, physical lines belonging to `#[cfg(test)]`
//! (or `#[cfg(all(test, ...))]`) items — whether an inline `mod tests { ... }`
//! block at the end of the file, a `#[cfg(test)] mod sub_tests;` declaration,
//! or an inline `#[cfg(test)]` helper item — are excluded; every other line
//! counts toward the file's non-test LOC.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ratchet::{cfg_is_test, find_test_files};
use crate::{BaselineError, ScanError, Violation, violation};

/// Maximum permitted non-test lines for any `.rs` file not grandfathered in
/// `loc-baseline.json`.
pub const MAX_NON_TEST_LOC: usize = 1_000;

/// Parsed `loc-baseline.json`: the pinned cap and per-file frozen ceilings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocBaseline {
    /// The pinned non-test LOC cap (must equal [`MAX_NON_TEST_LOC`]).
    pub max_non_test_loc: usize,
    /// Per-file frozen non-test LOC ceilings (every entry strictly above `max_non_test_loc`).
    pub files: BTreeMap<String, usize>,
}

/// Active `#[cfg(test)]` item state while scanning a Rust source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestRegion {
    None,
    Pending {
        start_line: usize,
        base_brace_depth: usize,
    },
    Braced {
        start_line: usize,
        base_brace_depth: usize,
    },
}

struct LocScanner<'a> {
    source: &'a str,
    bytes: &'a [u8],
    test_lines: Vec<bool>,
    i: usize,
    line: usize,
    brace_depth: usize,
    paren_depth: usize,
    bracket_depth: usize,
    region: TestRegion,
}

impl<'a> LocScanner<'a> {
    fn new(source: &'a str, total_lines: usize) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            test_lines: vec![false; total_lines],
            i: 0,
            line: 0,
            brace_depth: 0,
            paren_depth: 0,
            bracket_depth: 0,
            region: TestRegion::None,
        }
    }

    fn scan(mut self) -> usize {
        while let Some(&byte) = self.bytes.get(self.i) {
            if !self.step_token(byte) {
                self.step_delimiter(byte);
                self.i = self.i.saturating_add(1);
            }
        }
        self.test_lines.iter().filter(|is_test| !**is_test).count()
    }

    fn step_token(&mut self, byte: u8) -> bool {
        if byte == b'\n' {
            self.line = self.line.saturating_add(1);
            self.i = self.i.saturating_add(1);
            return true;
        }
        if byte == b'/' && self.bytes.get(self.i.saturating_add(1)) == Some(&b'/') {
            self.i = skip_line_comment(self.bytes, self.i);
            return true;
        }
        if byte == b'/' && self.bytes.get(self.i.saturating_add(1)) == Some(&b'*') {
            let (next_i, added_lines) = skip_block_comment(self.bytes, self.i);
            self.i = next_i;
            self.line = self.line.saturating_add(added_lines);
            return true;
        }
        if let Some((next_i, added_lines)) = try_skip_raw_string(self.bytes, self.i) {
            self.i = next_i;
            self.line = self.line.saturating_add(added_lines);
            return true;
        }
        if byte == b'"' {
            let (next_i, added_lines) = skip_quoted_string(self.bytes, self.i);
            self.i = next_i;
            self.line = self.line.saturating_add(added_lines);
            return true;
        }
        if byte == b'\'' {
            self.i = skip_char_or_lifetime(self.source, self.bytes, self.i);
            return true;
        }
        if byte == b'#' && self.bytes.get(self.i.saturating_add(1)) == Some(&b'[') {
            let attr_start_line = self.line;
            let inner_start = self.i.saturating_add(2);
            let (after_attr, inner_end, added_lines) =
                scan_bracketed(self.source, self.bytes, self.i);
            if matches!(self.region, TestRegion::None)
                && let Some(inner) = self.source.get(inner_start..inner_end)
                && is_cfg_test_attr(inner)
            {
                self.region = TestRegion::Pending {
                    start_line: attr_start_line,
                    base_brace_depth: self.brace_depth,
                };
            }
            self.i = after_attr;
            self.line = self.line.saturating_add(added_lines);
            return true;
        }
        false
    }

    fn step_delimiter(&mut self, byte: u8) {
        match byte {
            b'(' => self.paren_depth = self.paren_depth.saturating_add(1),
            b')' => self.paren_depth = self.paren_depth.saturating_sub(1),
            b'[' => self.bracket_depth = self.bracket_depth.saturating_add(1),
            b']' => self.bracket_depth = self.bracket_depth.saturating_sub(1),
            b'{' => {
                if let TestRegion::Pending {
                    start_line,
                    base_brace_depth,
                } = self.region
                    && self.brace_depth == base_brace_depth
                    && self.paren_depth == 0
                    && self.bracket_depth == 0
                {
                    self.region = TestRegion::Braced {
                        start_line,
                        base_brace_depth,
                    };
                }
                self.brace_depth = self.brace_depth.saturating_add(1);
            }
            b'}' => {
                self.brace_depth = self.brace_depth.saturating_sub(1);
                if let TestRegion::Braced {
                    start_line,
                    base_brace_depth,
                } = self.region
                    && self.brace_depth == base_brace_depth
                {
                    mark_lines(&mut self.test_lines, start_line, self.line);
                    self.region = TestRegion::None;
                }
            }
            b';' => {
                if let TestRegion::Pending {
                    start_line,
                    base_brace_depth,
                } = self.region
                    && self.brace_depth == base_brace_depth
                    && self.paren_depth == 0
                    && self.bracket_depth == 0
                {
                    mark_lines(&mut self.test_lines, start_line, self.line);
                    self.region = TestRegion::None;
                }
            }
            _ => {}
        }
    }
}

/// Scans a single Rust source file and returns the number of physical lines
/// outside `#[cfg(test)]` items.
#[must_use]
pub fn non_test_loc(source: &str) -> usize {
    let total_lines = source.lines().count();
    if total_lines == 0 {
        return 0;
    }
    LocScanner::new(source, total_lines).scan()
}

fn mark_lines(test_lines: &mut [bool], start_line: usize, end_line: usize) {
    let clamped_end = end_line.min(test_lines.len().saturating_sub(1));
    if let Some(slice) = test_lines.get_mut(start_line..=clamped_end) {
        for slot in slice {
            *slot = true;
        }
    }
}

fn is_cfg_test_attr(inner: &str) -> bool {
    let Ok(syn::Meta::List(list)) = syn::parse_str::<syn::Meta>(inner) else {
        return false;
    };
    list.path.is_ident("cfg")
        && list
            .parse_args::<syn::Meta>()
            .is_ok_and(|meta| cfg_is_test(&meta))
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start.saturating_add(2);
    while let Some(&byte) = bytes.get(i) {
        if byte == b'\n' {
            break;
        }
        i = i.saturating_add(1);
    }
    i
}

fn skip_block_comment(bytes: &[u8], start: usize) -> (usize, usize) {
    let mut i = start.saturating_add(2);
    let mut depth = 1_usize;
    let mut lines = 0_usize;
    while depth > 0 {
        let Some(&byte) = bytes.get(i) else {
            break;
        };
        if byte == b'\n' {
            lines = lines.saturating_add(1);
            i = i.saturating_add(1);
        } else if byte == b'/' && bytes.get(i.saturating_add(1)) == Some(&b'*') {
            depth = depth.saturating_add(1);
            i = i.saturating_add(2);
        } else if byte == b'*' && bytes.get(i.saturating_add(1)) == Some(&b'/') {
            depth = depth.saturating_sub(1);
            i = i.saturating_add(2);
        } else {
            i = i.saturating_add(1);
        }
    }
    (i, lines)
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn try_skip_raw_string(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let prev_is_ident = start
        .checked_sub(1)
        .and_then(|idx| bytes.get(idx))
        .is_some_and(|byte| is_ident_continue(*byte));
    if prev_is_ident {
        return None;
    }
    let after_prefix = match bytes.get(start)? {
        b'r' => start.saturating_add(1),
        b'b' | b'c' if bytes.get(start.saturating_add(1)) == Some(&b'r') => start.saturating_add(2),
        _ => return None,
    };
    let mut i = after_prefix;
    let mut hashes = 0_usize;
    while bytes.get(i) == Some(&b'#') {
        hashes = hashes.saturating_add(1);
        i = i.saturating_add(1);
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i = i.saturating_add(1);
    let mut lines = 0_usize;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'\n' {
            lines = lines.saturating_add(1);
            i = i.saturating_add(1);
            continue;
        }
        if byte == b'"' {
            let mut matched = 0_usize;
            while matched < hashes
                && bytes.get(i.saturating_add(1).saturating_add(matched)) == Some(&b'#')
            {
                matched = matched.saturating_add(1);
            }
            if matched == hashes {
                return Some((i.saturating_add(1).saturating_add(hashes), lines));
            }
        }
        i = i.saturating_add(1);
    }
    Some((i, lines))
}

fn skip_quoted_string(bytes: &[u8], start: usize) -> (usize, usize) {
    let mut i = start.saturating_add(1);
    let mut lines = 0_usize;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'\\' {
            if bytes.get(i.saturating_add(1)) == Some(&b'\n') {
                lines = lines.saturating_add(1);
            }
            i = i.saturating_add(2).min(bytes.len());
            continue;
        }
        if byte == b'\n' {
            lines = lines.saturating_add(1);
        }
        i = i.saturating_add(1);
        if byte == b'"' {
            break;
        }
    }
    (i, lines)
}

fn skip_char_or_lifetime(source: &str, bytes: &[u8], start: usize) -> usize {
    let next = start.saturating_add(1);
    if bytes.get(next) == Some(&b'\\') {
        let mut i = next.saturating_add(2).min(bytes.len());
        while let Some(&byte) = bytes.get(i) {
            if byte == b'\'' {
                return i.saturating_add(1);
            }
            if byte == b'\n' {
                return i;
            }
            i = i.saturating_add(1);
        }
        return i;
    }
    if let Some(tail) = source.get(next..)
        && let Some(ch) = tail.chars().next()
        && ch != '\n'
    {
        let after_ch = next.saturating_add(ch.len_utf8());
        if bytes.get(after_ch) == Some(&b'\'') {
            return after_ch.saturating_add(1);
        }
    }
    next
}

/// Scans an outer attribute starting at `#[` and returns
/// `(index_after_closing_bracket, index_of_closing_bracket, newlines_crossed)`.
fn scan_bracketed(source: &str, bytes: &[u8], start: usize) -> (usize, usize, usize) {
    let mut i = start.saturating_add(2);
    let mut depth = 1_usize;
    let mut lines = 0_usize;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'\n' {
            lines = lines.saturating_add(1);
            i = i.saturating_add(1);
        } else if byte == b'/' && bytes.get(i.saturating_add(1)) == Some(&b'/') {
            i = skip_line_comment(bytes, i);
        } else if byte == b'/' && bytes.get(i.saturating_add(1)) == Some(&b'*') {
            let (next_i, added) = skip_block_comment(bytes, i);
            i = next_i;
            lines = lines.saturating_add(added);
        } else if let Some((next_i, added)) = try_skip_raw_string(bytes, i) {
            i = next_i;
            lines = lines.saturating_add(added);
        } else if byte == b'"' {
            let (next_i, added) = skip_quoted_string(bytes, i);
            i = next_i;
            lines = lines.saturating_add(added);
        } else if byte == b'\'' {
            i = skip_char_or_lifetime(source, bytes, i);
        } else if byte == b'[' {
            depth = depth.saturating_add(1);
            i = i.saturating_add(1);
        } else if byte == b']' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return (i.saturating_add(1), i, lines);
            }
            i = i.saturating_add(1);
        } else {
            i = i.saturating_add(1);
        }
    }
    (i, i, lines)
}

/// Counts non-test lines for every non-test `.rs` file under
/// `<workspace_root>/crates` (`target/` directories, `tests/` directories, and
/// `#[cfg(test)]`-only module files are excluded). Paths are `/`-separated
/// relative to `workspace_root`.
///
/// # Errors
///
/// Returns [`ScanError`] if `crates/` cannot be walked or a file cannot be
/// read or parsed.
pub fn loc_counts(workspace_root: &Path) -> Result<BTreeMap<String, usize>, ScanError> {
    let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut parsed: BTreeMap<PathBuf, syn::File> = BTreeMap::new();
    let walker = walkdir::WalkDir::new(workspace_root.join("crates"))
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target");
    for entry in walker {
        let entry = entry.map_err(ScanError::Walk)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_path_buf();
        let source =
            std::fs::read_to_string(path).map_err(|err| ScanError::Io(path.to_path_buf(), err))?;
        let file =
            syn::parse_file(&source).map_err(|err| ScanError::Parse(path.to_path_buf(), err))?;
        sources.insert(rel.clone(), source);
        parsed.insert(rel, file);
    }
    let test_files: BTreeSet<PathBuf> = find_test_files(&parsed);
    let mut out = BTreeMap::new();
    for (rel, source) in &sources {
        if test_files.contains(rel) {
            continue;
        }
        let key = rel.to_string_lossy().replace('\\', "/");
        out.insert(key, non_test_loc(source));
    }
    Ok(out)
}

/// Parses `loc-baseline.json` and validates its structure and cap pin.
///
/// # Errors
///
/// Returns [`BaselineError`] if the JSON is malformed, `max_non_test_loc` does
/// not match [`MAX_NON_TEST_LOC`], `files` is missing, or any entry is not an
/// integer strictly greater than [`MAX_NON_TEST_LOC`].
pub fn parse_loc_baseline(baseline_json: &str) -> Result<LocBaseline, BaselineError> {
    let root: serde_json::Value =
        serde_json::from_str(baseline_json).map_err(|err| BaselineError(err.to_string()))?;
    let max_non_test_loc = root
        .get("max_non_test_loc")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            BaselineError("loc-baseline.json: missing integer `max_non_test_loc`".to_owned())
        })?;
    if max_non_test_loc != MAX_NON_TEST_LOC {
        return Err(BaselineError(format!(
            "loc-baseline.json: `max_non_test_loc` is {max_non_test_loc}, expected {MAX_NON_TEST_LOC}"
        )));
    }
    let files_obj = root
        .get("files")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| BaselineError("loc-baseline.json: missing object `files`".to_owned()))?;
    let mut files = BTreeMap::new();
    for (path, ceiling) in files_obj {
        let ceiling = ceiling
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > MAX_NON_TEST_LOC)
            .ok_or_else(|| {
                BaselineError(format!(
                    "loc-baseline.json: `files.\"{path}\"` must be an integer > {MAX_NON_TEST_LOC}; delete entries at or below the cap"
                ))
            })?;
        files.insert(path.clone(), ceiling);
    }
    Ok(LocBaseline {
        max_non_test_loc,
        files,
    })
}

/// Compares measured non-test LOC counts against `loc-baseline.json`:
///
/// - Any file not in `loc-baseline.json` with `loc > MAX_NON_TEST_LOC` fails
///   with `loc-cap-exceeded`.
/// - Any file in `loc-baseline.json` with `loc > ceiling` fails with
///   `loc-cap-exceeded`.
/// - Files shrinking at or below their ceiling pass (lowering `loc-baseline.json`
///   is encouraged in cleanup PRs, and `baseline-shrink-only.sh` guarantees
///   ceilings never rise).
///
/// # Errors
///
/// Returns [`BaselineError`] if `baseline_json` is malformed.
pub fn check_loc_cap(
    found: &BTreeMap<String, usize>,
    baseline_json: &str,
) -> Result<Vec<Violation>, BaselineError> {
    let baseline = parse_loc_baseline(baseline_json)?;
    let mut out = Vec::new();
    for (path, &loc) in found {
        if let Some(&ceiling) = baseline.files.get(path) {
            if loc > ceiling {
                out.push(violation(
                    "loc-cap-exceeded",
                    format!(
                        "{path} has {loc} non-test LOC, exceeding its frozen ceiling of {ceiling} \
                         in crates/norn-skeleton-guard/loc-baseline.json (cap: {}). \
                         Split the file into smaller modules; ceilings are shrink-only.",
                        baseline.max_non_test_loc
                    ),
                ));
            }
        } else if loc > baseline.max_non_test_loc {
            out.push(violation(
                "loc-cap-exceeded",
                format!(
                    "{path} has {loc} non-test LOC, exceeding the {} non-test LOC cap. \
                     Split the file into smaller modules; new entries in loc-baseline.json are forbidden.",
                    baseline.max_non_test_loc
                ),
            ));
        }
    }
    out.sort();
    Ok(out)
}

/// Builds the canonical `loc-baseline.json` payload matching every file in
/// `found` that currently exceeds [`MAX_NON_TEST_LOC`].
#[must_use]
pub fn loc_baseline_json(found: &BTreeMap<String, usize>) -> serde_json::Value {
    let files: serde_json::Map<String, serde_json::Value> = found
        .iter()
        .filter(|(_, loc)| **loc > MAX_NON_TEST_LOC)
        .map(|(path, loc)| (path.clone(), serde_json::Value::from(*loc)))
        .collect();
    serde_json::json!({
        "_comment": "Frozen shrink-only non-test LOC ceilings for files exceeding 1,000 non-test LOC (src/loc_cap.rs). Ceilings may only shrink or be removed; no new entries allowed (CI: scripts/baseline-shrink-only.sh).",
        "max_non_test_loc": MAX_NON_TEST_LOC,
        "files": files,
    })
}
