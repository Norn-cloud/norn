//! Pinned offline IANA tzdb bundle verification (`#1440`).
//!
//! Verifies that the committed binary tzdb bundles under
//! `crates/norn-expr/data/tzdb/`, their matching `registry/standards/*.source.json`
//! provenance files, and the AST-verified `const` / `include_bytes!` / loader
//! bindings in `crates/norn-expr/src/tzdb.rs` agree on exact bytes, release
//! headers, sorted unique zone tables, `Factory` exclusion, and `TZif` headers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprLit, ExprMacro, ImplItem, Item, Lit, LitStr};

use crate::{Violation, violation};

/// Fixed binary header magic (`NTZDB01\0`).
pub const TZDB_BUNDLE_MAGIC: &[u8; 8] = b"NTZDB01\0";
const HEADER_LEN: usize = 24;
const ENTRY_LEN: usize = 16;
const MAX_BUNDLE_BYTES: usize = 4_194_304;
const MAX_ZONES: usize = 4_096;
const FORBIDDEN_FACTORY_ZONE: &str = "Factory";

/// One pinned offline IANA tzdb bundle entry enforced by the guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TzdbBundlePin {
    /// Embedded IANA release identifier (e.g. `"2025b"`).
    pub release: &'static str,
    /// Repository-relative path to the binary bundle file.
    pub path: &'static str,
    /// Expected lowercase hex SHA-256 digest of the bundle bytes.
    pub sha256: &'static str,
    /// Expected admitted zone count (excluding `Factory`).
    pub zone_count: usize,
    /// Repository-relative path to the corresponding standards `.source.json`.
    pub source_json: &'static str,
    /// Upstream `jiff-tzdb` crate version from which this bundle was generated.
    pub crate_version: &'static str,
    /// Expected SHA-256 of the upstream `.crate` tarball.
    pub crate_sha256: &'static str,
    /// Name of the release `const` in `crates/norn-expr/src/tzdb.rs`.
    pub release_const: &'static str,
    /// Name of the SHA-256 `const` in `crates/norn-expr/src/tzdb.rs`.
    pub sha256_const: &'static str,
    /// Name of the `include_bytes!` `const` in `crates/norn-expr/src/tzdb.rs`.
    pub bytes_const: &'static str,
    /// Expected relative path passed to `include_bytes!` in `crates/norn-expr/src/tzdb.rs`.
    pub include_rel_path: &'static str,
    /// Name of the `TzdbBundle` loader method binding `bytes_const` and `sha256_const`.
    pub loader_fn: &'static str,
}

/// The exact compiled-in tzdb bundles shipped by `norn-expr`.
pub const PINNED_TZDB_BUNDLES: &[TzdbBundlePin] = &[
    TzdbBundlePin {
        release: "2025b",
        path: "crates/norn-expr/data/tzdb/2025b.tzdb",
        sha256: "6b6514f7fed817ad3cf616de4532c6ec7da85929336b7a2956b8f9f40f5fbf59",
        zone_count: 597,
        source_json: "registry/standards/iana-tzdb-2025b.source.json",
        crate_version: "0.1.4",
        crate_sha256: "c1283705eb0a21404d2bfd6eef2a7593d240bc42a0bdb39db0ad6fa2ec026524",
        release_const: "TZDB_RELEASE_2025B",
        sha256_const: "TZDB_SHA256_2025B",
        bytes_const: "BUILTIN_2025B_BYTES",
        include_rel_path: "../data/tzdb/2025b.tzdb",
        loader_fn: "v2025b",
    },
    TzdbBundlePin {
        release: "2026c",
        path: "crates/norn-expr/data/tzdb/2026c.tzdb",
        sha256: "e0cda9ad9e465ee416b473b0e2e8686a468c8c66a6f279b9c7379c5650b38018",
        zone_count: 597,
        source_json: "registry/standards/iana-tzdb-2026c.source.json",
        crate_version: "0.1.8",
        crate_sha256: "142bd39932ad231f10513df9ab62661fead8719872150b7ad02a2df79f4e141e",
        release_const: "TZDB_RELEASE_2026C",
        sha256_const: "TZDB_SHA256_2026C",
        bytes_const: "BUILTIN_2026C_BYTES",
        include_rel_path: "../data/tzdb/2026c.tzdb",
        loader_fn: "v2026c",
    },
];

/// Verifies all [`PINNED_TZDB_BUNDLES`] in `workspace_root`.
#[must_use]
pub fn check_tzdb_bundles(workspace_root: &Path) -> Vec<Violation> {
    check_tzdb_bundles_against(workspace_root, PINNED_TZDB_BUNDLES)
}

/// Verifies an explicit slice of [`TzdbBundlePin`] entries against `workspace_root`.
#[must_use]
pub fn check_tzdb_bundles_against(workspace_root: &Path, pins: &[TzdbBundlePin]) -> Vec<Violation> {
    let mut out = Vec::new();
    for pin in pins {
        check_one_bundle(workspace_root, pin, &mut out);
        check_source_json(workspace_root, pin, &mut out);
    }
    check_rust_digest_pins(workspace_root, pins, &mut out);
    out.sort();
    out
}

fn check_one_bundle(workspace_root: &Path, pin: &TzdbBundlePin, out: &mut Vec<Violation>) {
    let file_path = workspace_root.join(pin.path);
    let bytes = match std::fs::read(&file_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            out.push(violation(
                "tzdb-bundle-missing",
                format!("{}: {err}", pin.path),
            ));
            return;
        }
    };

    let actual_sha = hex::encode(Sha256::digest(&bytes));
    if actual_sha != pin.sha256 {
        out.push(violation(
            "tzdb-bundle-digest",
            format!(
                "{}: expected SHA-256 {}, found {actual_sha}",
                pin.path, pin.sha256
            ),
        ));
    }

    if let Err(reason) = validate_bundle_structure(&bytes, pin.release, pin.zone_count) {
        out.push(violation(
            "tzdb-bundle-format",
            format!("{}: {reason}", pin.path),
        ));
    }
}

fn check_source_json(workspace_root: &Path, pin: &TzdbBundlePin, out: &mut Vec<Violation>) {
    let source_path = workspace_root.join(pin.source_json);
    let text = match std::fs::read_to_string(&source_path) {
        Ok(text) => text,
        Err(err) => {
            out.push(violation(
                "tzdb-source-pin",
                format!("{}: {err}", pin.source_json),
            ));
            return;
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(err) => {
            out.push(violation(
                "tzdb-source-pin",
                format!("{}: invalid JSON: {err}", pin.source_json),
            ));
            return;
        }
    };
    if doc.get("releaseId").and_then(serde_json::Value::as_str) != Some(pin.release) {
        out.push(violation(
            "tzdb-source-pin",
            format!(
                "{}: releaseId must equal `{}`",
                pin.source_json, pin.release
            ),
        ));
    }
    if doc.get("crateVersion").and_then(serde_json::Value::as_str) != Some(pin.crate_version) {
        out.push(violation(
            "tzdb-source-pin",
            format!(
                "{}: crateVersion must equal `{}`",
                pin.source_json, pin.crate_version
            ),
        ));
    }
    if doc.get("crateSha256").and_then(serde_json::Value::as_str) != Some(pin.crate_sha256) {
        out.push(violation(
            "tzdb-source-pin",
            format!(
                "{}: crateSha256 must equal `{}`",
                pin.source_json, pin.crate_sha256
            ),
        ));
    }
    let has_bundle_entry = doc
        .get("files")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|files| {
            files.iter().any(|entry| {
                entry.get("path").and_then(serde_json::Value::as_str) == Some(pin.path)
                    && entry.get("sha256").and_then(serde_json::Value::as_str) == Some(pin.sha256)
            })
        });
    if !has_bundle_entry {
        out.push(violation(
            "tzdb-source-pin",
            format!(
                "{}: files must pin `{}` at SHA-256 {}",
                pin.source_json, pin.path, pin.sha256
            ),
        ));
    }
}

struct VerifiedCallCollector {
    calls: Vec<(String, String)>,
}

impl<'ast> Visit<'ast> for VerifiedCallCollector {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(callee) = node.func.as_ref()
            && callee
                .path
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "from_bytes_verified")
            && node.args.len() == 2
            && let (Some(Expr::Path(arg0)), Some(Expr::Path(arg1))) =
                (node.args.first(), node.args.iter().nth(1))
            && let (Some(id0), Some(id1)) = (arg0.path.get_ident(), arg1.path.get_ident())
        {
            self.calls.push((id0.to_string(), id1.to_string()));
        }
        visit::visit_expr_call(self, node);
    }
}

struct RustTzdbPins {
    const_strs: BTreeMap<String, String>,
    const_includes: BTreeMap<String, String>,
    loader_calls: BTreeMap<String, Vec<(String, String)>>,
}

fn collect_rust_tzdb_pins(parsed: &syn::File) -> RustTzdbPins {
    let mut pins = RustTzdbPins {
        const_strs: BTreeMap::new(),
        const_includes: BTreeMap::new(),
        loader_calls: BTreeMap::new(),
    };
    for item in &parsed.items {
        if let Item::Const(item_const) = item {
            let name = item_const.ident.to_string();
            if let Expr::Lit(ExprLit {
                lit: Lit::Str(lit), ..
            }) = item_const.expr.as_ref()
            {
                pins.const_strs.insert(name, lit.value());
            } else if let Expr::Macro(ExprMacro { mac, .. }) = item_const.expr.as_ref()
                && mac.path.is_ident("include_bytes")
                && let Ok(lit) = mac.parse_body::<LitStr>()
            {
                pins.const_includes.insert(name, lit.value());
            }
        } else if let Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let ImplItem::Fn(method) = impl_item {
                    let mut collector = VerifiedCallCollector { calls: Vec::new() };
                    collector.visit_block(&method.block);
                    if !collector.calls.is_empty() {
                        pins.loader_calls
                            .entry(method.sig.ident.to_string())
                            .or_default()
                            .extend(collector.calls);
                    }
                }
            }
        }
    }
    pins
}

fn check_rust_digest_pins(workspace_root: &Path, pins: &[TzdbBundlePin], out: &mut Vec<Violation>) {
    let rust_path = workspace_root.join("crates/norn-expr/src/tzdb.rs");
    let source = match std::fs::read_to_string(&rust_path) {
        Ok(source) => source,
        Err(err) => {
            out.push(violation(
                "tzdb-rust-pin",
                format!("crates/norn-expr/src/tzdb.rs: {err}"),
            ));
            return;
        }
    };
    let parsed = match syn::parse_file(&source) {
        Ok(parsed) => parsed,
        Err(err) => {
            out.push(violation(
                "tzdb-rust-pin",
                format!("crates/norn-expr/src/tzdb.rs: failed to parse Rust AST: {err}"),
            ));
            return;
        }
    };
    let ast_pins = collect_rust_tzdb_pins(&parsed);

    for pin in pins {
        if ast_pins
            .const_strs
            .get(pin.release_const)
            .map(String::as_str)
            != Some(pin.release)
        {
            out.push(violation(
                "tzdb-rust-pin",
                format!(
                    "crates/norn-expr/src/tzdb.rs: const `{}` must equal \"{}\"",
                    pin.release_const, pin.release
                ),
            ));
        }
        if ast_pins
            .const_strs
            .get(pin.sha256_const)
            .map(String::as_str)
            != Some(pin.sha256)
        {
            out.push(violation(
                "tzdb-rust-pin",
                format!(
                    "crates/norn-expr/src/tzdb.rs: const `{}` must equal \"{}\" for release `{}`",
                    pin.sha256_const, pin.sha256, pin.release
                ),
            ));
        }
        if ast_pins
            .const_includes
            .get(pin.bytes_const)
            .map(String::as_str)
            != Some(pin.include_rel_path)
        {
            out.push(violation(
                "tzdb-rust-pin",
                format!(
                    "crates/norn-expr/src/tzdb.rs: const `{}` must call include_bytes!(\"{}\")",
                    pin.bytes_const, pin.include_rel_path
                ),
            ));
        }
        let binds_loader = ast_pins
            .loader_calls
            .get(pin.loader_fn)
            .is_some_and(|calls| {
                calls.iter().any(|(bytes_id, sha_id)| {
                    bytes_id == pin.bytes_const && sha_id == pin.sha256_const
                })
            });
        if !binds_loader {
            out.push(violation(
                "tzdb-rust-pin",
                format!(
                    "crates/norn-expr/src/tzdb.rs: `{}` must call from_bytes_verified({}, {})",
                    pin.loader_fn, pin.bytes_const, pin.sha256_const
                ),
            ));
        }
    }
}

struct GuardSections<'a> {
    zone_count: usize,
    index_slice: &'a [u8],
    names_slice: &'a [u8],
    payload_slice: &'a [u8],
}

fn parse_guard_sections<'a>(
    bytes: &'a [u8],
    expected_release: &str,
    expected_zones: usize,
) -> Result<GuardSections<'a>, String> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(format!(
            "bundle length {} exceeds {MAX_BUNDLE_BYTES} bytes",
            bytes.len()
        ));
    }
    let header = bytes
        .get(..HEADER_LEN)
        .ok_or_else(|| "truncated 24-byte header".to_owned())?;
    if header.get(..8) != Some(TZDB_BUNDLE_MAGIC.as_slice()) {
        return Err("invalid magic header (expected NTZDB01\\0)".to_owned());
    }
    let release_len = usize::from(read_u16_le(header, 8)?);
    let reserved = read_u16_le(header, 10)?;
    if reserved != 0 {
        return Err(format!("reserved header field is non-zero ({reserved})"));
    }
    let zone_count = usize::try_from(read_u32_le(header, 12)?)
        .map_err(|_err| "zone count overflows usize".to_owned())?;
    let names_len = usize::try_from(read_u32_le(header, 16)?)
        .map_err(|_err| "names_len overflows usize".to_owned())?;
    let payload_len = usize::try_from(read_u32_le(header, 20)?)
        .map_err(|_err| "payload_len overflows usize".to_owned())?;

    if zone_count == 0 || zone_count > MAX_ZONES {
        return Err(format!("zone count {zone_count} out of bounds"));
    }
    if zone_count != expected_zones {
        return Err(format!(
            "expected {expected_zones} zones, header declares {zone_count}"
        ));
    }

    let release_end = HEADER_LEN
        .checked_add(release_len)
        .ok_or_else(|| "release_end overflow".to_owned())?;
    let index_bytes = zone_count
        .checked_mul(ENTRY_LEN)
        .ok_or_else(|| "index_bytes overflow".to_owned())?;
    let index_end = release_end
        .checked_add(index_bytes)
        .ok_or_else(|| "index_end overflow".to_owned())?;
    let names_end = index_end
        .checked_add(names_len)
        .ok_or_else(|| "names_end overflow".to_owned())?;
    let payload_end = names_end
        .checked_add(payload_len)
        .ok_or_else(|| "payload_end overflow".to_owned())?;
    if payload_end != bytes.len() {
        return Err(format!(
            "declared length {payload_end} != actual length {}",
            bytes.len()
        ));
    }

    let release = std::str::from_utf8(
        bytes
            .get(HEADER_LEN..release_end)
            .ok_or_else(|| "release slice out of bounds".to_owned())?,
    )
    .map_err(|_err| "release header is not UTF-8".to_owned())?;
    if release != expected_release {
        return Err(format!(
            "embedded release `{release}` != expected `{expected_release}`"
        ));
    }

    Ok(GuardSections {
        zone_count,
        index_slice: bytes
            .get(release_end..index_end)
            .ok_or_else(|| "index slice out of bounds".to_owned())?,
        names_slice: bytes
            .get(index_end..names_end)
            .ok_or_else(|| "names slice out of bounds".to_owned())?,
        payload_slice: bytes
            .get(names_end..payload_end)
            .ok_or_else(|| "payload slice out of bounds".to_owned())?,
    })
}

/// Validates binary layout, embedded release header, sorted unique zone names,
/// `Factory` exclusion, in-bounds offsets/lengths, and `TZif` headers.
///
/// # Errors
///
/// Returns a human-readable reason if any structural invariant fails.
pub fn validate_bundle_structure(
    bytes: &[u8],
    expected_release: &str,
    expected_zones: usize,
) -> Result<(), String> {
    let sections = parse_guard_sections(bytes, expected_release, expected_zones)?;
    let mut previous: Option<&str> = None;
    let mut checked_blobs = BTreeSet::new();
    for idx in 0..sections.zone_count {
        let start = idx
            .checked_mul(ENTRY_LEN)
            .ok_or_else(|| format!("entry {idx} offset overflow"))?;
        let end = start
            .checked_add(ENTRY_LEN)
            .ok_or_else(|| format!("entry {idx} end overflow"))?;
        let entry = sections
            .index_slice
            .get(start..end)
            .ok_or_else(|| format!("entry {idx} out of bounds"))?;
        let name_off = usize::try_from(read_u32_le(entry, 0)?)
            .map_err(|_err| format!("entry {idx} name_off overflow"))?;
        let name_len = usize::try_from(read_u32_le(entry, 4)?)
            .map_err(|_err| format!("entry {idx} name_len overflow"))?;
        let tzif_off = usize::try_from(read_u32_le(entry, 8)?)
            .map_err(|_err| format!("entry {idx} tzif_off overflow"))?;
        let tzif_len = usize::try_from(read_u32_le(entry, 12)?)
            .map_err(|_err| format!("entry {idx} tzif_len overflow"))?;

        if name_len == 0 {
            return Err(format!("entry {idx} has empty zone name"));
        }
        let name_end = name_off
            .checked_add(name_len)
            .ok_or_else(|| format!("entry {idx} name range overflow"))?;
        let name = std::str::from_utf8(
            sections
                .names_slice
                .get(name_off..name_end)
                .ok_or_else(|| format!("entry {idx} name slice out of bounds"))?,
        )
        .map_err(|_err| format!("entry {idx} name is not UTF-8"))?;

        if name == FORBIDDEN_FACTORY_ZONE {
            return Err("bundle contains forbidden zone `Factory`".to_owned());
        }
        if let Some(prev) = previous
            && name <= prev
        {
            return Err(format!(
                "zone index not strictly sorted at {idx}: `{prev}` then `{name}`"
            ));
        }
        previous = Some(name);

        if tzif_len < 44 {
            return Err(format!("entry {idx} (`{name}`) TZif payload too short"));
        }
        let tzif_end = tzif_off
            .checked_add(tzif_len)
            .ok_or_else(|| format!("entry {idx} (`{name}`) TZif range overflow"))?;
        let tzif = sections
            .payload_slice
            .get(tzif_off..tzif_end)
            .ok_or_else(|| format!("entry {idx} (`{name}`) TZif slice out of bounds"))?;
        if checked_blobs.insert((tzif_off, tzif_len)) && tzif.get(..4) != Some(b"TZif".as_slice()) {
            return Err(format!(
                "entry {idx} (`{name}`) TZif payload missing TZif magic"
            ));
        }
    }
    Ok(())
}

fn read_u16_le(slice: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "u16 offset overflow".to_owned())?;
    let bytes: [u8; 2] = slice
        .get(offset..end)
        .and_then(|raw| raw.try_into().ok())
        .ok_or_else(|| "truncated u16".to_owned())?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le(slice: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "u32 offset overflow".to_owned())?;
    let bytes: [u8; 4] = slice
        .get(offset..end)
        .and_then(|raw| raw.try_into().ok())
        .ok_or_else(|| "truncated u32".to_owned())?;
    Ok(u32::from_le_bytes(bytes))
}
