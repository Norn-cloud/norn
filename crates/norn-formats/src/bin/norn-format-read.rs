//! `norn-format-read <manifest.json> --originals <dir> [--emit <file>]`
//! `norn-format-read <owl manifest.json> --originals <dir> --emit-dir <dir> [--align <table>]`
//!
//! An `owl` manifest (`registry/standards/imports/*.read.json`) names a
//! normalised graph: `--originals` is the directory `owl_sources.py fetch`
//! wrote (`raw/` and `graph.nt`). The bin checks every pinned original under
//! `raw/`, reads `graph.nt` with the OWL reader, refuses any error diagnostic,
//! and writes the layers (`artifacts.json`), the source schema
//! (`source_schema.json`), the loss report (`loss.jsonl`) and its counts
//! (`summary.json`) to `--emit-dir`, never into the repository.
//!
//! Reads a pack's hash-pinned source originals with the format reader the
//! manifest names, refuses any error diagnostic, and checks that the pack's
//! committed, hand-written source contract claims nothing the originals do
//! not: every contract field exists upstream with the same native and
//! candidate type, and is required only where upstream requires it.
//!
//! `--emit` writes the whole projection for review to a file outside the
//! repository; upstream-derived schema payload is never written into the tree.
//! Standards and systems are packs: adding one needs a manifest, not Rust.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use norn_formats::owl::{OwlConfig, OwlGraph, OwlNamespace, OwlNormaliser, OwlReader};
use norn_formats::owl_align::{OwlAlignment, align as align_vocabulary};
use norn_formats::pack::{ProjectionSpec, check_contract, project, render_review_module};
use norn_formats::xsd::{
    MAX_BUNDLE_BYTES, MAX_FILE_BYTES, XsdConfig, XsdFile, XsdReader, repeating_fields,
};
use norn_formats::{Disposition, SchemaImporter};
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{DiagnosticSeverity, OntologySource};

/// A pack's source-reading manifest.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    /// The format family; `xsd` today.
    format: String,
    /// The URL prefix every pinned file path starts with; the rest of the
    /// path is the file's location under `--originals`.
    originals_base: String,
    /// The caller-supplied observation time (no clock is read).
    observed_at: String,
    /// Licence, release and per-file SHA-256 pins.
    admission: OntologySource,
    /// What the pack projects, and its committed contract.
    projection: ProjectionSpec,
}

/// An OWL import manifest: pins of the originals and of their normalised graph.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwlManifest {
    /// `owl`.
    format: String,
    /// The standards-atlas entry the import serves.
    #[serde(rename = "atlasId")]
    _atlas_id: String,
    /// The caller-supplied observation time (no clock is read).
    observed_at: String,
    /// How `owl_sources.py pin` found the closure; not read here.
    #[serde(rename = "selection")]
    _selection: serde_json::Value,
    /// Licence, release and per-file SHA-256 pins of the originals.
    admission: OntologySource,
    /// The normaliser that wrote `graph.nt`.
    normaliser: OwlNormaliser,
    /// The owned namespaces and their prefixes.
    namespaces: Vec<OwlNamespace>,
    /// The normalised graph's pin.
    graph: GraphPin,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GraphPin {
    sha256: String,
    #[serde(rename = "triples")]
    _triples: u64,
}

/// Read at most `MAX_FILE_BYTES` (plus one, to detect excess) from `path`.
fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    read_limited(path, MAX_FILE_BYTES)
}

/// Read at most `max` bytes (plus one, to detect excess) from `path`.
fn read_limited(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let limit = u64::try_from(max).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > max {
        return Err(format!("{}: exceeds {max} bytes", path.display()));
    }
    Ok(bytes)
}

fn read(manifest: &Manifest, originals: &Path) -> Result<SourceSchemaIr, String> {
    let pins = manifest.admission.files.clone().unwrap_or_default();
    if pins.is_empty() {
        return Err("admission pins no file".into());
    }
    if !manifest.originals_base.ends_with('/') {
        return Err("originalsBase must end with `/`".into());
    }
    // A running total stops reading at the bundle limit, so N pins never hold
    // N times the file limit in memory.
    let mut total = 0_usize;
    let mut seen = std::collections::BTreeSet::new();
    let bytes = pins
        .iter()
        .map(|pin| {
            // A repeated pin is refused before its file is read again.
            if !seen.insert(pin.path.as_str()) {
                return Err(format!("duplicate pinned path {}", pin.path));
            }
            let relative = pin
                .path
                .strip_prefix(&manifest.originals_base)
                .filter(|rest| {
                    !rest.is_empty()
                        && !rest.starts_with('/')
                        && !rest.split('/').any(|step| step.is_empty() || step == "..")
                })
                .ok_or_else(|| format!("{} is not a file under originalsBase", pin.path))?;
            let bytes = read_bounded(&originals.join(relative))?;
            total = total.saturating_add(bytes.len());
            if total > MAX_BUNDLE_BYTES {
                return Err("schema bundle exceeds 64 MiB".to_owned());
            }
            Ok(bytes)
        })
        .collect::<Result<Vec<_>, _>>()?;
    match manifest.format.as_str() {
        "xsd" => {
            let files: Vec<XsdFile<'_>> = pins
                .iter()
                .zip(&bytes)
                .map(|(pin, bytes)| XsdFile {
                    path: &pin.path,
                    bytes,
                })
                .collect();
            let config = XsdConfig {
                admission: manifest.admission.clone(),
                observed_at: manifest.observed_at.clone(),
            };
            let import = XsdReader.import(&files, &config);
            if let Some(error) = import
                .diagnostics
                .iter()
                .find(|d| d.severity == DiagnosticSeverity::Error)
            {
                return Err(format!("{}: {}", error.code, error.message));
            }
            import
                .source_schema
                .ok_or_else(|| "the reader produced no source schema".to_owned())
        }
        other => Err(format!("no reader for format {other:?}")),
    }
}

/// The repository containing `path`: the outermost ancestor holding `.git`
/// or a workspace `Cargo.lock` (a source export has no `.git`).
fn repository_of(path: &Path) -> Option<PathBuf> {
    let absolute = std::fs::canonicalize(path).ok()?;
    absolute
        .ancestors()
        .filter(|dir| dir.join(".git").exists() || dir.join("Cargo.lock").exists())
        .last()
        .map(Path::to_path_buf)
}

/// Refuse an output location inside a repository (or a symbolic link).
fn outside_repository(target: &Path, manifest_path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{}: refuses a symbolic link", target.display()));
    }
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    let resolved =
        std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if repository_of(&resolved).is_some()
        || repository_of(manifest_path).is_some_and(|repo| resolved.starts_with(&repo))
    {
        return Err(format!(
            "{}: never writes upstream-derived definitions into the repository",
            target.display()
        ));
    }
    Ok(())
}

/// Read an OWL manifest's graph and write its import to `emit_dir`.
#[allow(
    clippy::too_many_lines,
    reason = "one linear pass: read, check, emit, align"
)]
fn run_owl(
    manifest_path: &Path,
    text: &str,
    originals: &Path,
    emit_dir: &Path,
    align: Option<&Path>,
) -> Result<(), String> {
    let manifest: OwlManifest =
        serde_json::from_str(text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if manifest.format != "owl" {
        return Err("not an owl manifest".into());
    }
    for pin in manifest.admission.files.iter().flatten() {
        let relative = pin
            .path
            .strip_prefix("https://")
            .filter(|rest| !rest.split('/').any(|step| step.is_empty() || step == ".."))
            .ok_or_else(|| format!("{} is not a plain HTTPS file URL", pin.path))?;
        let bytes = read_bounded(&originals.join("raw").join(relative))?;
        if norn_formats::contract::sha256_hex(&bytes) != pin.sha256 {
            return Err(format!("{}: digest mismatch", pin.path));
        }
    }
    let graph = read_limited(
        &originals.join("graph.nt"),
        norn_formats::owl::MAX_GRAPH_BYTES,
    )?;
    let config = OwlConfig {
        admission: manifest.admission,
        observed_at: manifest.observed_at,
        graph_sha256: manifest.graph.sha256,
        normaliser: manifest.normaliser,
        namespaces: manifest.namespaces,
    };
    let import = OwlReader.import(
        OwlGraph {
            path: "graph.nt",
            bytes: &graph,
        },
        &config,
    );
    if let Some(error) = import
        .diagnostics
        .iter()
        .find(|d| d.severity == DiagnosticSeverity::Error)
    {
        return Err(format!("{}: {}", error.code, error.message));
    }
    outside_repository(&emit_dir.join("summary.json"), manifest_path)?;
    std::fs::create_dir_all(emit_dir).map_err(|e| format!("{}: {e}", emit_dir.display()))?;
    let write = |name: &str, text: String| {
        std::fs::write(emit_dir.join(name), text).map_err(|e| format!("{name}: {e}"))
    };
    let artifacts: Vec<_> = import
        .layers
        .iter()
        .flat_map(|layer| &layer.artifacts)
        .collect();
    write(
        "artifacts.json",
        serde_json::to_string(&artifacts).map_err(|e| e.to_string())?,
    )?;
    let schema = import
        .source_schema_envelope(&config.admission.source_id.replace("source.", "owl."))?
        .ok_or("the reader produced no source schema")?;
    write(
        "source_schema.json",
        serde_json::to_string(&schema).map_err(|e| e.to_string())?,
    )?;
    let mut loss = String::new();
    let mut deferred = std::collections::BTreeMap::<&str, u64>::new();
    let mut degraded = std::collections::BTreeMap::<&str, u64>::new();
    for entry in &import.loss_report.entries {
        let value = norn_formats::contract::sorted_keys(
            serde_json::to_value(entry).map_err(|e| e.to_string())?,
        );
        loss.push_str(&serde_json::to_string(&value).map_err(|e| e.to_string())?);
        loss.push('\n');
        match &entry.disposition {
            Disposition::Deferred { code, .. } => {
                let count = deferred.entry(code).or_default();
                *count = count.saturating_add(1);
            }
            Disposition::Mapped {
                degraded: Some(degradation),
                ..
            } => {
                let count = degraded.entry(&degradation.code).or_default();
                *count = count.saturating_add(1);
            }
            Disposition::Mapped { .. }
            | Disposition::Extension { .. }
            | Disposition::Rejected { .. }
            | Disposition::Dropped { .. }
            | Disposition::Unexplained => {}
        }
    }
    write("loss.jsonl", loss)?;
    let summary = import.loss_report.summary();
    let members = |name: &str| -> usize {
        artifacts
            .iter()
            .filter_map(|artifact| serde_json::to_value(artifact).ok())
            .map(|value| {
                value
                    .pointer(&format!("/payload/interface/{name}"))
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len)
            })
            .sum()
    };
    let mut report = serde_json::json!({
        "graphSha256": config.graph_sha256,
        "interfaces": artifacts.len(),
        "links": members("links"),
        "properties": members("properties"),
        "extends": members("extends"),
        "entries": summary.total,
        "mapped": summary.mapped,
        "degraded": summary.degraded,
        "deferred": summary.deferred,
        "dropped": summary.dropped,
        "unexplained": summary.unexplained,
        "deferredByCode": deferred,
        "degradedByCode": degraded,
    });
    if let Some(table) = align {
        let table: OwlAlignment = serde_json::from_str(
            &std::fs::read_to_string(table).map_err(|e| format!("{}: {e}", table.display()))?,
        )
        .map_err(|e| format!("{}: {e}", table.display()))?;
        let vocabulary: Vec<_> = import
            .layers
            .iter()
            .filter(|layer| layer.name == norn_formats::owl::VOCABULARY_LAYER)
            .flat_map(|layer| layer.artifacts.iter().cloned())
            .collect();
        let aligned = align_vocabulary(&vocabulary, &config.admission, &table)?;
        write(
            "packs.json",
            serde_json::to_string(&aligned.packs).map_err(|e| e.to_string())?,
        )?;
        let alignment = serde_json::json!({
            "summary": aligned.summary(),
            "findings": aligned.findings,
        });
        write(
            "alignment.json",
            serde_json::to_string_pretty(&alignment).map_err(|e| e.to_string())?,
        )?;
        if let Some(fields) = report.as_object_mut() {
            fields.insert("alignment".into(), aligned.summary());
        }
    }
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    writeln!(std::io::stdout().lock(), "{text}").map_err(|e| e.to_string())?;
    write("summary.json", text)
}

fn run(
    manifest_path: &Path,
    originals: &Path,
    emit: Option<&Path>,
    align: Option<&Path>,
) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let format = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("format")
                .and_then(|f| f.as_str())
                .map(str::to_owned)
        });
    if format.as_deref() == Some("owl") {
        let emit_dir = emit.ok_or("an owl manifest needs --emit-dir <dir>")?;
        return run_owl(manifest_path, &text, originals, emit_dir, align);
    }
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let schema = read(&manifest, originals)?;
    let projection = project(&schema, &manifest.projection, &repeating_fields(&schema))?;
    if let Some(target) = emit {
        // A symlinked target could land inside the tree; refuse it outright.
        if std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{}: --emit refuses a symbolic link",
                target.display()
            ));
        }
        let parent = target.parent().filter(|p| !p.as_os_str().is_empty());
        let parent = parent.unwrap_or_else(|| Path::new("."));
        let resolved =
            std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        // The target's own location decides too: a manifest outside any
        // repository must not disable the restriction.
        if repository_of(&resolved).is_some()
            || repository_of(manifest_path).is_some_and(|repo| resolved.starts_with(&repo))
        {
            return Err(format!(
                "{}: --emit never writes upstream-derived schema into the repository",
                target.display()
            ));
        }
        let module = render_review_module(&projection)?;
        return std::fs::write(target, module).map_err(|e| format!("{}: {e}", target.display()));
    }
    let contract_path = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&manifest.projection.contract);
    let contract: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&contract_path)
            .map_err(|e| format!("{}: {e}", contract_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", contract_path.display()))?;
    let payload = contract.get("payload").unwrap_or(&contract);
    let problems = check_contract(payload, &projection);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} claims more than the pinned originals:\n  {}",
            contract_path.display(),
            problems.join("\n  ")
        ))
    }
}

/// Write `message` to stderr and exit with `code`; a failed write still exits.
fn report(message: &str, code: u8) -> ExitCode {
    writeln!(std::io::stderr().lock(), "{message}").ok();
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut manifest = None;
    let mut originals = None;
    let mut emit = None;
    let mut align = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--originals" => originals = args.next().map(PathBuf::from),
            "--emit" | "--emit-dir" => emit = args.next().map(PathBuf::from),
            "--align" => align = args.next().map(PathBuf::from),
            "--check" => {}
            _ if manifest.is_none() => manifest = Some(PathBuf::from(arg)),
            _ => return report(&format!("unexpected argument {arg}"), 2),
        }
    }
    let (Some(manifest), Some(originals)) = (manifest, originals) else {
        return report(
            "usage: norn-format-read <manifest.json> --originals <dir> [--emit <file> | --emit-dir <dir> [--align <table.json>]]",
            2,
        );
    };
    match run(&manifest, &originals, emit.as_deref(), align.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(&error, 1),
    }
}
