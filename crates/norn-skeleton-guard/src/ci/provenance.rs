//! Public-cut admission (#3632). Candidate files are data, never generators.
//!
//! Invariant: every shipped byte has one exact record and a closed, acyclic,
//! admissible ancestry. Acquisition evidence is factual, not a redistribution
//! grant. Analysis references do not imply derivation; extraction databases do.
//!
//! Contract: version 2 control data lives under `.github/ci/provenance/`.
//! `manifest.json` holds the version and `software_sources`; each shipped file
//! `<path>` has exactly one record, `artifacts/<path>.json`, so changes to
//! different files never touch the same record. The identity is the exact
//! record file path (never a glob or a field), with lowercase SHA256,
//! origin/category, SPDX rights, source bindings,
//! parent paths, hashed rights evidence and shipped notices. Sources use literal
//! MANIFEST `id` or standards-lock `sourceId`; they bind revision and evidence
//! path/hash, with no vendor-name aliases. Unknown fields and enum values fail.
//! Until the #121 migration lands, the version-1 `shipped-artifacts.json`
//! (records with a `path` field) is read instead; never both.
//! The control directory is the sole exception to the self-hash problem; every
//! file in it must be the manifest or a record.
//!
//! A query/law/verdict uses `analyzed_sources` for pinned subjects of analysis;
//! copied or extracted content uses `sources`/`parents`. Independently authored
//! synthetic inputs use `synthetic-fixture`; generated goldens must close over
//! synthetic inputs. MIT software uses separate `software.*` source records,
//! with provenance evidence plus a shipped copyright/permission notice. Such
//! rights cannot admit schema/data categories. No acquisition terms are edited.
//!
//! This checks structural declarations and exact bytes, not authorship truth
//! or runtime mapping correctness. Human source review and the referenced
//! install-validation tests remain mandatory. Real databases are generated
//! outside the shipped tree, against pinned checkouts at CI runtime; neither
//! a query's analysis reference nor a verdict admits that database. Archives
//! must be expanded and their entire distribution checked independently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The only directory exempt from its own content hash. Its entire typed
/// content is validated, including all records; it cannot admit itself.
pub const CONTROL: &str = ".github/ci/provenance/";
/// Version and software sources.
pub const MANIFEST: &str = ".github/ci/provenance/manifest.json";
/// Prefix of each per-file record, `RECORDS` + path + `.json`.
pub const RECORDS: &str = ".github/ci/provenance/artifacts/";

/// Transitional (#121): the single version-1 manifest, read only until the
/// per-file migration lands. A tree with both layouts is refused.
pub const LEGACY_MANIFEST: &str = ".github/ci/shipped-artifacts.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    software_sources: Vec<SoftwareSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyManifest {
    version: u32,
    software_sources: Vec<SoftwareSource>,
    artifacts: Vec<serde_json::Value>,
}

/// Artifact-specific software provenance; never an override of acquisition
/// terms for schemas/data. IDs must use the separate software namespace.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SoftwareSource {
    id: String,
    revision: String,
    license: String,
    evidence: FileRef,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRef {
    path: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRef {
    id: String,
    revision: String,
    evidence: FileRef,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Origin {
    Authored,
    Upstream,
    Derived,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Category {
    Code,
    GenericProof,
    AuthoredQuery,
    AuthoredLaw,
    Verdict,
    SyntheticFixture,
    SyntheticGolden,
    ProvenanceEvidence,
    Software,
    StandardsData,
    Mapping,
    Connector,
    Generated,
    VendorSchema,
    VendorData,
    ExtractionDatabase,
    AccountingDomain,
    InventoryDomain,
    CertifiedDomainLaw,
    Soliman,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connector {
    /// Exact supported revisions, each also present in `analyzed_sources`.
    versions: Vec<SupportedVersions>,
    api_terms: Vec<FileRef>,
    /// Sparse identifiers only; no schema or expressive metadata in this list.
    mapped_fields: Vec<String>,
    install_validation: FileRef,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportedVersions {
    source_id: String,
    revisions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    /// Derived from the record file path; never a claim inside the record.
    #[serde(skip)]
    path: String,
    sha256: String,
    origin: Origin,
    category: Category,
    license: String,
    sources: Vec<SourceRef>,
    analyzed_sources: Vec<SourceRef>,
    parents: Vec<String>,
    rights_evidence: Vec<FileRef>,
    notices: Vec<FileRef>,
    connector: Option<Connector>,
}

struct Source {
    evidence: FileRef,
    revision: String,
    license: String,
    redistribution: String,
    files: Vec<FileRef>,
    vendor_payload: bool,
}

/// Complete deterministic rejection inventory. Unit-test success is separate
/// from candidate admission. Source counts include factual rejected sources
/// even when no shipped payload uses them.
#[derive(Default, Serialize)]
pub struct Report {
    /// Number of inspected shipped file paths.
    pub checked_paths: usize,
    /// Exact rejected paths and all local/ancestry reasons.
    pub rejected_paths: BTreeMap<String, Vec<String>>,
    /// Factual source IDs whose payload rights do not clear admission.
    pub rejected_sources: BTreeMap<String, String>,
    /// Malformed control data, source catalogs or inventory errors.
    pub policy_errors: Vec<String>,
}

impl Report {
    /// Diagnostics used by both candidate and trusted policy lanes.
    #[must_use]
    pub fn errors(&self) -> Vec<String> {
        let mut errors = self.policy_errors.clone();
        errors.extend(
            self.rejected_paths
                .iter()
                .map(|(path, reasons)| format!("public-cut {path}: {}", reasons.join("; "))),
        );
        errors
    }

    fn reject(&mut self, path: &str, reason: impl Into<String>) {
        let reasons = self.rejected_paths.entry(path.to_owned()).or_default();
        let reason = reason.into();
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', '\n', '\r'])
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p == ".git")
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}

fn bytes(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    if !safe_path(path) {
        return Err(format!("unsafe/non-exact path {path:?}"));
    }
    let mut current = root.to_path_buf();
    for part in path.split('/') {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).map_err(|e| format!("{path}: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err(format!("symlink refused: {path}"));
        }
    }
    if std::fs::metadata(&current)
        .map_err(|e| e.to_string())?
        .len()
        > 32 * 1024 * 1024
    {
        return Err(format!(
            "file exceeds bounded policy inspection limit: {path}"
        ));
    }
    if !current.is_file() {
        return Err(format!("not a regular file: {path}"));
    }
    std::fs::read(current).map_err(|e| format!("{path}: {e}"))
}

fn digest(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn hash_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn exact_revision(revision: &str) -> bool {
    !revision.is_empty()
        && !revision.chars().any(char::is_whitespace)
        && !revision.contains(['*', '^', '~', '>', '<'])
        && !matches!(
            revision.to_ascii_lowercase().as_str(),
            "main" | "master" | "head" | "latest" | "current" | "stable" | "unverified" | "unknown"
        )
}

fn permissive(license: &str) -> bool {
    matches!(
        license,
        "Apache-2.0"
            | "MIT"
            | "BSD-2-Clause"
            | "BSD-3-Clause"
            | "ISC"
            | "CC0-1.0"
            | "Unicode-3.0"
            | "ODC-PDDL-1.0"
    )
}

fn source_error(source: &Source) -> Option<String> {
    if source.vendor_payload {
        Some(format!(
            "vendor schema/data payload excluded: {} / {}",
            source.license, source.redistribution
        ))
    } else if !permissive(&source.license)
        || !matches!(source.redistribution.as_str(), "permitted" | "vendored")
    {
        Some(format!("{} / {}", source.license, source.redistribution))
    } else if !exact_revision(&source.revision) {
        Some("missing exact source revision".to_owned())
    } else {
        None
    }
}

fn text(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("missing source {key}"))
}

fn file_refs(value: Option<&serde_json::Value>) -> Result<Vec<FileRef>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let list = value.as_array().ok_or("source files must be an array")?;
    list.iter()
        .map(|v| {
            Ok(FileRef {
                path: text(v, "path")?,
                sha256: text(v, "sha256")?,
            })
        })
        .collect()
}

/// Preserve literal identifiers from both catalogs. Duplicate IDs fail rather
/// than guessing an alias from a vendor name or a similar URL. `SourceRef` binds
/// the exact evidence file/hash as well as the source ID/revision.
fn catalog(root: &Path, paths: &[String]) -> Result<BTreeMap<String, Source>, String> {
    let mut result = BTreeMap::new();
    for path in paths.iter().filter(|p| {
        p.as_str() == "registry/standards/sources.lock.json"
            || (p.starts_with("registry/standards/") && p.ends_with(".source.json"))
            || (p.starts_with("registry/sources/") && p.ends_with("/MANIFEST"))
    }) {
        let data = bytes(root, path)?;
        let value: serde_json::Value =
            serde_json::from_slice(&data).map_err(|e| format!("{path}: {e}"))?;
        let derived = path.ends_with(".source.json");
        let lock = path.ends_with("sources.lock.json") || derived;
        let vendor_payload = !lock
            && value.get("purpose").and_then(serde_json::Value::as_str)
                != Some("standards-catalogue");
        let sources = if derived {
            Some(std::slice::from_ref(&value))
        } else if lock {
            value.as_array().map(Vec::as_slice)
        } else {
            value
                .get("sources")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
        }
        .ok_or_else(|| format!("{path}: missing source array"))?;
        for value in sources {
            let id = text(value, if lock { "sourceId" } else { "id" })?;
            let terms = if lock {
                value
            } else {
                value.get("terms").ok_or("missing terms")?
            };
            let source = Source {
                evidence: FileRef {
                    path: path.clone(),
                    sha256: digest(&data),
                },
                revision: if lock {
                    text(value, "releaseId").unwrap_or_default()
                } else {
                    value
                        .pointer("/pin/revision")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| text(value, "version").ok())
                        .unwrap_or_default()
                },
                license: text(terms, "license").unwrap_or_else(|_| "unverified".to_owned()),
                redistribution: text(terms, "redistribution")?,
                files: file_refs(if lock {
                    value.get("files")
                } else {
                    value.pointer("/pin/files")
                })?,
                vendor_payload,
            };
            if result.insert(id.clone(), source).is_some() {
                return Err(format!("ambiguous source ID {id}"));
            }
        }
    }
    Ok(result)
}

fn reference(reference: &FileRef, hashes: &BTreeMap<String, String>) -> Result<(), String> {
    if !safe_path(&reference.path)
        || !hash_valid(&reference.sha256)
        || hashes.get(&reference.path) != Some(&reference.sha256)
    {
        Err(format!(
            "missing/tampered evidence or notice {}",
            reference.path
        ))
    } else {
        Ok(())
    }
}

fn forbidden(category: Category) -> bool {
    matches!(
        category,
        Category::VendorSchema
            | Category::VendorData
            | Category::ExtractionDatabase
            | Category::AccountingDomain
            | Category::InventoryDomain
            | Category::CertifiedDomainLaw
            | Category::Soliman
    )
}

fn opaque(path: &str, data: &[u8]) -> bool {
    // Opaque containers cannot assert a list of members: require an expanded
    // distribution checked independently by --distribution. No candidate
    // unpacker, build script or generator runs in the trusted lane.
    [
        ".zip", ".gz", ".tgz", ".tar", ".xz", ".bz2", ".7z", ".db", ".sqlite", ".sqlite3",
        ".parquet",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
        || data.starts_with(b"SQLite format 3\0")
        || data.starts_with(b"PK\x03\x04")
        || data.starts_with(&[0x1f, 0x8b])
        || data.starts_with(b"PAR1")
        || data.get(257..262) == Some(b"ustar")
}

fn validate_sources(
    a: &Artifact,
    sources: &BTreeMap<String, Source>,
    hashes: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut ids = BTreeSet::new();
    for (refs, derived) in [(&a.sources, true), (&a.analyzed_sources, false)] {
        for r in refs {
            if let Err(e) = reference(&r.evidence, hashes) {
                errors.push(e);
            }
            if !ids.insert(&r.id) {
                errors.push(format!("ambiguous repeated source {}", r.id));
            }
            match sources.get(&r.id) {
                None => errors.push(format!("unknown source {}", r.id)),
                Some(s) => {
                    if !exact_revision(&r.revision)
                        || r.revision != s.revision
                        || r.evidence.path != s.evidence.path
                        || r.evidence.sha256 != s.evidence.sha256
                    {
                        errors.push(format!("unbound source evidence/revision {}", r.id));
                    }
                    if derived {
                        if let Some(e) = source_error(s) {
                            errors.push(format!("source {} rejected: {e}", r.id));
                        }
                        if a.origin == Origin::Upstream && a.license != s.license {
                            errors.push(format!("upstream licence not preserved: {}", r.id));
                        }
                        if a.origin == Origin::Upstream
                            && !s.files.is_empty()
                            && !s.files.iter().any(|f| f.sha256 == a.sha256)
                        {
                            errors.push(format!(
                                "upstream content differs from pinned source {}",
                                r.id
                            ));
                        }
                        if r.id.starts_with("software.") && a.category != Category::Software {
                            errors.push(
                                "software rights cannot clear schema/data payloads".to_owned(),
                            );
                        }
                        if s.license == "MIT" && a.notices.is_empty() {
                            errors.push("MIT source requires shipped notice".to_owned());
                        }
                    }
                }
            }
        }
    }
    errors
}

fn validate_connector(a: &Artifact, hashes: &BTreeMap<String, String>) -> Vec<String> {
    let mut errors = Vec::new();
    if matches!(a.category, Category::Mapping | Category::Connector) {
        match &a.connector {
            None => errors.push("missing connector/pack admission evidence".to_owned()),
            Some(c) => {
                if c.versions.is_empty() || c.api_terms.is_empty() || a.analyzed_sources.is_empty()
                {
                    errors.push("missing exact supported versions/API terms".to_owned());
                }
                let mut ids = BTreeSet::new();
                for version in &c.versions {
                    let id = &version.source_id;
                    let mut revisions = BTreeSet::new();
                    if !ids.insert(id)
                        || version.revisions.is_empty()
                        || version.revisions.iter().any(|v| {
                            !exact_revision(v)
                                || !revisions.insert(v)
                                || !a
                                    .analyzed_sources
                                    .iter()
                                    .any(|r| &r.id == id && &r.revision == v)
                        })
                    {
                        errors.push(format!("unsupported/unpinned vendor version {id}"));
                    }
                }
                if a.category == Category::Mapping
                    && (c.mapped_fields.is_empty()
                        || c.mapped_fields.iter().any(|f| {
                            f.is_empty()
                                || f.contains(['*', '{', '}', '[', ']', '\n'])
                                || f.split('.').count() < 2
                        }))
                {
                    errors.push("mapping must list sparse field identifiers".to_owned());
                }
                for e in c
                    .api_terms
                    .iter()
                    .chain(std::iter::once(&c.install_validation))
                {
                    if let Err(e) = reference(e, hashes) {
                        errors.push(e);
                    }
                }
            }
        }
    } else if a.connector.is_some() {
        errors.push("unexpected connector evidence".to_owned());
    }
    errors
}

fn validate_record(
    a: &Artifact,
    sources: &BTreeMap<String, Source>,
    hashes: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    if !hash_valid(&a.sha256) || hashes.get(&a.path) != Some(&a.sha256) {
        errors.push("missing artifact or content hash drift".to_owned());
    }
    if !permissive(&a.license) {
        errors.push(format!("nonpermissive/unknown rights {}", a.license));
    }
    if forbidden(a.category) {
        errors.push("forbidden payload category".to_owned());
    }
    if a.rights_evidence.is_empty() {
        errors.push("missing rights evidence".to_owned());
    }
    for evidence in a.rights_evidence.iter().chain(&a.notices) {
        if let Err(e) = reference(evidence, hashes) {
            errors.push(e);
        }
    }
    match a.origin {
        Origin::Authored
            if !a.sources.is_empty() || !a.parents.is_empty() || a.license != "Apache-2.0" =>
        {
            errors.push("authored origin has derivative ancestry or non-Apache grant".to_owned());
        }
        Origin::Upstream if a.sources.is_empty() => {
            errors.push("upstream origin lacks source".to_owned());
        }
        Origin::Derived if a.sources.is_empty() && a.parents.is_empty() => {
            errors.push("derived origin lacks parents/sources".to_owned());
        }
        Origin::Authored | Origin::Upstream | Origin::Derived => (),
    }
    if matches!(a.category, Category::Generated | Category::SyntheticGolden)
        && (a.origin != Origin::Derived || a.parents.is_empty())
    {
        errors.push("generated artifact must be derived".to_owned());
    }
    if matches!(
        a.category,
        Category::AuthoredQuery
            | Category::AuthoredLaw
            | Category::GenericProof
            | Category::ProvenanceEvidence
            | Category::SyntheticFixture
    ) && a.origin != Origin::Authored
    {
        errors.push("category requires independently authored origin".to_owned());
    }
    if !a.analyzed_sources.is_empty()
        && !matches!(
            a.category,
            Category::AuthoredQuery
                | Category::AuthoredLaw
                | Category::Verdict
                | Category::ProvenanceEvidence
                | Category::Mapping
                | Category::Connector
        )
    {
        errors.push("analysis references not allowed for payload category".to_owned());
    }
    errors.extend(validate_sources(a, sources, hashes));
    // Known source paths and byte-identical copies cannot claim authored
    // origin or sever their source link by changing a filename/category.
    for (id, s) in sources {
        if s.files
            .iter()
            .any(|f| f.path == a.path || f.sha256 == a.sha256)
            && !a.sources.iter().any(|r| &r.id == id)
        {
            errors.push(format!(
                "known source payload requires derivation from {id}"
            ));
        }
    }
    if a.category == Category::ProvenanceEvidence
        && !sources.values().any(|s| s.evidence.path == a.path)
    {
        errors.push("factual evidence category restricted to source catalogs".to_owned());
    }
    if a.category == Category::Software && (a.notices.is_empty() || a.sources.is_empty()) {
        errors.push("software requires source provenance and shipped notices".to_owned());
    }
    if a.license == "MIT" && a.notices.is_empty() {
        errors.push("MIT requires shipped copyright/permission notice".to_owned());
    }
    errors.extend(validate_connector(a, hashes));
    errors
}

fn inspect_bytes(root: &Path, paths: &[String], report: &mut Report) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    for path in paths {
        match bytes(root, path) {
            Ok(data) => {
                if opaque(path, &data) {
                    report.reject(path, "opaque container/extraction database refused; inspect expanded distribution");
                }
                hashes.insert(path.clone(), digest(&data));
            }
            Err(e) => report.reject(path, e),
        }
    }
    hashes
}

fn payload_category(root: &Path, a: &Artifact) -> Result<(), String> {
    let data = bytes(root, &a.path)?;
    if (data.starts_with(b"\0asm") || data.starts_with(b"\x7fELF") || data.starts_with(b"MZ"))
        && a.category != Category::Generated
    {
        return Err("compiled/embedded artifact requires generated lineage".to_owned());
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return Ok(());
    };
    embedded_lineage(root, a, &value)?;
    let format = value
        .get("format")
        .or_else(|| value.pointer("/payload/format"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let schema = value.get("kind").and_then(serde_json::Value::as_str) == Some("source_schema")
        || format == "norn.source-schema/1"
        || value.get("doctype").and_then(serde_json::Value::as_str) == Some("DocType");
    if schema
        && !matches!(
            a.category,
            Category::SyntheticFixture | Category::SyntheticGolden
        )
    {
        return Err(
            "schema/DocType payload requires independently authored synthetic provenance"
                .to_owned(),
        );
    }
    let database = format == "norn.behaviour-source/1"
        || format.starts_with("norn.codegraph/")
        || format.starts_with("norn.extraction/");
    if database && a.category != Category::SyntheticGolden {
        return Err(
            "extraction facts require synthetic derivation; real databases never ship".to_owned(),
        );
    }
    Ok(())
}

fn embedded_lineage(root: &Path, a: &Artifact, value: &serde_json::Value) -> Result<(), String> {
    if let Some(derived) = value.pointer("/provenance/derivedFrom") {
        for parent in derived
            .as_array()
            .ok_or("derivedFrom must be a path array")?
        {
            let parent = parent
                .as_str()
                .ok_or("derivedFrom must contain exact paths")?;
            if !a.parents.iter().any(|p| p == parent) {
                return Err(format!(
                    "embedded derivedFrom missing from lineage: {parent}"
                ));
            }
        }
    }
    if value
        .pointer("/provenance/source")
        .and_then(serde_json::Value::as_str)
        == Some("import")
    {
        let parent = value
            .pointer("/provenance/sourcePath")
            .and_then(serde_json::Value::as_str)
            .ok_or("import provenance missing sourcePath")?;
        if !a.parents.iter().any(|p| p == parent) {
            return Err(format!("embedded import missing from lineage: {parent}"));
        }
    }
    if value.get("format").and_then(serde_json::Value::as_str) == Some("norn.registry.package/1") {
        let dir = Path::new(&a.path)
            .parent()
            .ok_or("package has no parent directory")?;
        for member in value
            .get("artifacts")
            .and_then(serde_json::Value::as_array)
            .ok_or("package missing artifact members")?
        {
            let member: FileRef = serde_json::from_value(
                serde_json::json!({"path":member.get("path"),"sha256":member.get("sha256")}),
            )
            .map_err(|e| e.to_string())?;
            if !safe_path(&member.path) {
                return Err("non-exact package member path".to_owned());
            }
            let path = dir
                .join(&member.path)
                .to_str()
                .ok_or("non-UTF8 package member")?
                .to_owned();
            if !a.parents.contains(&path) {
                return Err(format!("distribution member missing from lineage: {path}"));
            }
            if digest(&bytes(root, &path)?) != member.sha256 {
                return Err(format!("distribution member hash drift: {path}"));
            }
        }
    }
    Ok(())
}

fn inspect_sources(
    root: &Path,
    paths: &[String],
    base: Option<&Path>,
    report: &mut Report,
) -> BTreeMap<String, Source> {
    let mut sources = match catalog(root, paths) {
        Ok(s) => s,
        Err(e) => {
            report.policy_errors.push(e);
            BTreeMap::new()
        }
    };
    if let Some(base) = base {
        match super::git::tracked(base).and_then(|p| catalog(base, &p)) {
            Ok(prior) => {
                for (id, old) in prior {
                    if let Some(new) = sources.get(&id) {
                        if old.license != new.license
                            || old.redistribution != new.redistribution
                            || old.revision != new.revision
                            || old.files != new.files
                            || old.vendor_payload != new.vendor_payload
                        {
                            report.policy_errors.push(format!(
                                "source evidence terms/pin changed against trusted base: {id}"
                            ));
                        }
                    } else {
                        sources.insert(id, old);
                    }
                }
            }
            Err(e) => report
                .policy_errors
                .push(format!("trusted source catalog: {e}")),
        }
    }
    for (id, source) in &sources {
        if let Some(e) = source_error(source) {
            report.rejected_sources.insert(id.clone(), e);
        }
    }
    sources
}

fn close_ancestry(records: &BTreeMap<String, &Artifact>, report: &mut Report) {
    // Iterative topological closure avoids recursion/stack exhaustion on an
    // adversarial candidate; missing, duplicate and cyclic parents fail closed.
    let mut pending: BTreeSet<String> = records.keys().cloned().collect();
    let mut admitted = BTreeSet::new();
    while !pending.is_empty() {
        let ready: Vec<_> = pending
            .iter()
            .filter(|path| {
                records
                    .get(*path)
                    .is_some_and(|a| a.parents.iter().all(|p| !pending.contains(p)))
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            for path in &pending {
                report.reject(path, "cyclic ancestry");
            }
            break;
        }
        for path in ready {
            pending.remove(&path);
            if let Some(a) = records.get(&path) {
                let mut parents = BTreeSet::new();
                for parent in &a.parents {
                    if !parents.insert(parent) || !admitted.contains(parent) {
                        report.reject(
                            &path,
                            format!("missing/ambiguous/rejected ancestor {parent}"),
                        );
                    }
                }
                if a.category == Category::SyntheticGolden
                    && !a.parents.iter().any(|p| {
                        records.get(p).is_some_and(|p| {
                            matches!(
                                p.category,
                                Category::SyntheticFixture | Category::SyntheticGolden
                            )
                        })
                    })
                {
                    report.reject(&path, "synthetic golden lacks synthetic input ancestor");
                }
                if a.category == Category::SyntheticGolden
                    && (!a.sources.is_empty()
                        || a.parents.iter().any(|p| {
                            records.get(p).is_some_and(|p| {
                                !matches!(
                                    p.category,
                                    Category::SyntheticFixture
                                        | Category::SyntheticGolden
                                        | Category::Code
                                )
                            })
                        }))
                {
                    report.reject(&path, "synthetic golden has nonsynthetic data ancestry");
                }
                if !report.rejected_paths.contains_key(&path) {
                    admitted.insert(path);
                }
            }
        }
    }
}

fn close_artifacts(records: &BTreeMap<String, &Artifact>, report: &mut Report) {
    loop {
        let prior = report.rejected_paths.len();
        close_ancestry(records, report);
        let tainted: BTreeSet<_> = records
            .values()
            .filter(|a| report.rejected_paths.contains_key(&a.path))
            .map(|a| &a.sha256)
            .collect();
        for a in records.values().filter(|a| tainted.contains(&a.sha256)) {
            report.reject(
                &a.path,
                "content matches a rejected artifact; renamed copies inherit rejection",
            );
        }
        if report.rejected_paths.len() == prior {
            break;
        }
    }
}

/// One record per file under [`RECORDS`]; the filesystem makes each path
/// unique. Any other control file, or an unreadable record, fails closed.
fn load_records(root: &Path, paths: &[String], report: &mut Report) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    for path in paths.iter().filter(|p| p.starts_with(CONTROL)) {
        let Some(target) = path
            .strip_prefix(RECORDS)
            .and_then(|p| p.strip_suffix(".json"))
        else {
            if path != MANIFEST {
                report.reject(path, "unknown provenance control file");
            }
            continue;
        };
        match bytes(root, path)
            .and_then(|b| serde_json::from_slice::<Artifact>(&b).map_err(|e| e.to_string()))
        {
            Ok(mut a) => {
                target.clone_into(&mut a.path);
                artifacts.push(a);
            }
            Err(e) => report.reject(path, format!("malformed provenance record: {e}")),
        }
    }
    artifacts
}

fn load_legacy(
    root: &Path,
    report: &mut Report,
) -> Result<(Vec<SoftwareSource>, Vec<Artifact>), String> {
    let manifest: LegacyManifest = bytes(root, LEGACY_MANIFEST)
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .map_err(|e| format!("{LEGACY_MANIFEST}: {e}"))?;
    if manifest.version != 1 {
        return Err(format!("{LEGACY_MANIFEST}: unknown provenance version"));
    }
    let mut seen = BTreeSet::new();
    let mut artifacts = Vec::new();
    for mut value in manifest.artifacts {
        let path = value
            .as_object_mut()
            .and_then(|o| o.remove("path"))
            .and_then(|p| p.as_str().map(str::to_owned))
            .ok_or_else(|| format!("{LEGACY_MANIFEST}: record without path"))?;
        let mut a: Artifact =
            serde_json::from_value(value).map_err(|e| format!("{LEGACY_MANIFEST} {path}: {e}"))?;
        if !seen.insert(path.clone()) {
            report.reject(&path, "ambiguous duplicate artifact record");
        }
        a.path = path;
        artifacts.push(a);
    }
    Ok((manifest.software_sources, artifacts))
}

fn inspect(root: &Path, paths: &[String], base: Option<&Path>) -> Report {
    let mut report = Report {
        checked_paths: paths.len(),
        ..Report::default()
    };
    let hashes = inspect_bytes(root, paths, &mut report);
    let mut sources = inspect_sources(root, paths, base, &mut report);
    let legacy = paths.iter().any(|p| p == LEGACY_MANIFEST);
    if legacy && paths.iter().any(|p| p.starts_with(CONTROL)) {
        report
            .policy_errors
            .push(format!("both {LEGACY_MANIFEST} and {CONTROL} present"));
        return report;
    }
    let loaded = if legacy {
        load_legacy(root, &mut report)
    } else {
        bytes(root, MANIFEST)
            .and_then(|b| serde_json::from_slice::<Manifest>(&b).map_err(|e| e.to_string()))
            .and_then(|m| match m.version {
                2 => Ok(m.software_sources),
                _ => Err("unknown provenance version".to_owned()),
            })
            .map_err(|e| format!("{MANIFEST}: {e}"))
            .map(|software| (software, load_records(root, paths, &mut report)))
    };
    let (software_sources, artifacts) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => {
            report.policy_errors.push(e);
            return report;
        }
    };
    for s in software_sources {
        if !s.id.starts_with("software.") || s.revision.is_empty() || s.license != "MIT" {
            report
                .policy_errors
                .push(format!("invalid MIT software provenance {}", s.id));
        }
        if let Err(e) = reference(&s.evidence, &hashes) {
            report.policy_errors.push(e);
        }
        let source = Source {
            evidence: s.evidence,
            revision: s.revision,
            license: s.license,
            redistribution: "permitted".to_owned(),
            files: Vec::new(),
            vendor_payload: false,
        };
        if sources.insert(s.id.clone(), source).is_some() {
            report
                .policy_errors
                .push(format!("ambiguous source ID {}", s.id));
        }
    }
    let mut records = BTreeMap::new();
    for a in &artifacts {
        if a.path.starts_with(CONTROL) || a.path == LEGACY_MANIFEST || !safe_path(&a.path) {
            report.reject(&a.path, "control file/self hash or non-exact artifact path");
        }
        records.insert(a.path.clone(), a);
        for e in validate_record(a, &sources, &hashes) {
            report.reject(&a.path, e);
        }
        if let Err(e) = payload_category(root, a) {
            report.reject(&a.path, e);
        }
        if a.license == "MIT"
            || a.sources
                .iter()
                .any(|r| sources.get(&r.id).is_some_and(|s| s.license == "MIT"))
        {
            let notice = a.notices.iter().any(|n| {
                bytes(root, &n.path).is_ok_and(|b| {
                    let t = String::from_utf8_lossy(&b);
                    t.contains("Permission is hereby granted, free of charge")
                        && t.to_ascii_lowercase().contains("copyright")
                })
            });
            if !notice {
                report.reject(&a.path, "MIT copyright/permission notice missing");
            }
        }
    }
    for path in paths
        .iter()
        .filter(|p| !p.starts_with(CONTROL) && p.as_str() != LEGACY_MANIFEST)
    {
        if !records.contains_key(path) {
            report.reject(path, "missing shipped-artifact admission record");
        }
    }
    close_artifacts(&records, &mut report);
    report
}

/// Check the entire tracked candidate tree. No include/exclude globs and no
/// directory-wide claims: the exporter must actually remove held-out payloads.
#[must_use]
pub fn check(root: &Path) -> Report {
    match super::git::tracked(root) {
        Ok(paths) => inspect(root, &paths, None),
        Err(e) => Report {
            policy_errors: vec![e],
            ..Report::default()
        },
    }
}

/// Trusted/base-aware admission: source evidence cannot be removed or
/// relabelled to sever known lineage. No executable content from root is run.
#[must_use]
pub fn check_against(root: &Path, base: &Path) -> Report {
    match super::git::tracked(root) {
        Ok(paths) => inspect(root, &paths, Some(base)),
        Err(e) => Report {
            policy_errors: vec![e],
            ..Report::default()
        },
    }
}

/// Check every file in an expanded distribution, including untracked/ignored
/// files. The same manifest and lineage contract applies to embedded/release
/// copies. Symlinks, opaque archives and files outside closure fail closed.
#[must_use]
pub fn check_distribution(root: &Path) -> Report {
    distribution(root, None)
}

/// Expanded-distribution admission retaining the trusted base's source facts.
#[must_use]
pub fn check_distribution_against(root: &Path, base: &Path) -> Report {
    distribution(root, Some(base))
}

fn distribution(root: &Path, base: Option<&Path>) -> Report {
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        match entry {
            Ok(e) if !e.file_type().is_dir() => {
                match e.path().strip_prefix(root).ok().and_then(Path::to_str) {
                    Some(path) => paths.push(path.to_owned()),
                    None => {
                        return Report {
                            policy_errors: vec!["non-UTF8 distribution path".to_owned()],
                            ..Report::default()
                        };
                    }
                }
            }
            Ok(_) => (),
            Err(e) => {
                return Report {
                    policy_errors: vec![e.to_string()],
                    ..Report::default()
                };
            }
        }
    }
    paths.sort();
    inspect(root, &paths, base)
}
