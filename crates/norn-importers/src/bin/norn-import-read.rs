//! `norn-import-read ofbiz <manifest.json> --originals <dir> [--emit <file>]`
//!
//! Reads the files a manifest pins from `--originals`, refuses any whose
//! SHA-256 differs from its pin, imports them, and prints counts: datasets,
//! fields, constraints, code sets, lifecycle candidates and the loss report's
//! dispositions. It exits nonzero on any error diagnostic or unexplained
//! element. `--emit` writes the whole import, canonical JSON, to a file outside
//! the repository; upstream-derived output is never written into the tree.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use norn_formats::SchemaImporter as _;
use norn_formats::contract::Disposition;
use norn_importers::ofbiz::{OfbizConfig, OfbizFile, OfbizReader, Role, refused};
use norn_ir::JsonValue;
use serde_json::json;

/// Largest file read, and largest total.
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    system: String,
    repository: String,
    revision: String,
    observed_at: String,
    licence: String,
    files: Vec<Pin>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    path: String,
    role: Role,
    sha256: String,
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let limit = u64::try_from(MAX_FILE_BYTES).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!("{}: exceeds 16 MiB", path.display()));
    }
    Ok(bytes)
}

/// The repository containing `path`: the outermost ancestor holding `.git`
/// or a workspace `Cargo.lock`.
fn repository_of(path: &Path) -> Option<PathBuf> {
    let absolute = std::fs::canonicalize(path).ok()?;
    absolute
        .ancestors()
        .filter(|dir| dir.join(".git").exists() || dir.join("Cargo.lock").exists())
        .last()
        .map(Path::to_path_buf)
}

fn emit_target(target: &Path, manifest: &Path) -> Result<(), String> {
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
    if repository_of(&resolved).is_some()
        || repository_of(manifest).is_some_and(|repo| resolved.starts_with(&repo))
    {
        return Err(format!(
            "{}: --emit never writes upstream-derived output into the repository",
            target.display()
        ));
    }
    Ok(())
}

/// Counts of what the import produced, for the run's report.
fn summary(import: &norn_formats::SchemaImport) -> serde_json::Value {
    let mut dispositions: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &import.loss_report.entries {
        let name = match &entry.disposition {
            Disposition::Mapped { .. } => "mapped".to_owned(),
            Disposition::Extension { .. } => "extension".to_owned(),
            Disposition::Deferred { code, .. } => format!("deferred:{code}"),
            Disposition::Rejected { code, .. } => format!("rejected:{code}"),
            Disposition::Dropped { reason, .. } => format!("dropped:{reason:?}"),
            Disposition::Unexplained => "unexplained".to_owned(),
        };
        let slot = dispositions.entry(name).or_default();
        *slot = slot.saturating_add(1);
    }
    let mut diagnostics: BTreeMap<String, usize> = BTreeMap::new();
    for diagnostic in &import.diagnostics {
        let slot = diagnostics.entry(diagnostic.code.clone()).or_default();
        *slot = slot.saturating_add(1);
    }
    let schema_counts = import.source_schema.as_ref().map(|schema| {
        let count = |kind: &str| schema.datasets.iter().filter(|d| format!("{:?}", d.kind).eq_ignore_ascii_case(kind)).count();
        let constraints: BTreeMap<String, usize> = schema.constraints.iter().fold(BTreeMap::new(), |mut map, c| {
            let slot = map.entry(format!("{:?}", c.kind)).or_default();
            *slot = slot.saturating_add(1);
            map
        });
        let extension = schema
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get(norn_importers::ofbiz::EXTENSION));
        let length = |member: &str| match extension {
            Some(JsonValue::Object(object)) => match object.get(member) {
                Some(JsonValue::Array(items)) => items.len(),
                Some(JsonValue::Object(map)) => map.len(),
                Some(
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_),
                )
                | None => 0,
            },
            Some(
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::String(_)
                | JsonValue::Array(_),
            )
            | None => 0,
        };
        json!({
            "tables": count("table"), "views": count("view"), "fields": schema.fields.len(),
            "constraints": constraints, "codeSets": schema.code_sets.len(), "lineage": schema.lineage.len(),
            "lifecycleCandidates": length("lifecycleCandidates"), "codeSetGaps": length("codeSetGaps"),
            "unresolved": length("unresolved"), "entityGroups": length("entityGroups"),
        })
    });
    json!({
        "refused": refused(import),
        "schema": schema_counts,
        "lossReport": {"elements": import.loss_report.entries.len(), "dispositions": dispositions},
        "diagnostics": diagnostics,
        "firstErrors": import.diagnostics.iter().filter(|d| d.severity == norn_ir::DiagnosticSeverity::Error).map(|d| d.message.clone()).take(3).collect::<Vec<_>>(),
    })
}

fn run(manifest_path: &Path, originals: &Path, emit: Option<&Path>) -> Result<String, String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if let Some(target) = emit {
        emit_target(target, manifest_path)?;
    }
    let mut total = 0_usize;
    let mut bytes = Vec::with_capacity(manifest.files.len());
    for pin in &manifest.files {
        if pin.path.is_empty()
            || pin.path.starts_with('/')
            || pin
                .path
                .split('/')
                .any(|step| step.is_empty() || step == "..")
        {
            return Err(format!(
                "{} is not a relative path under --originals",
                pin.path
            ));
        }
        let file = read_bounded(&originals.join(&pin.path))?;
        total = total.saturating_add(file.len());
        if total > MAX_TOTAL_BYTES {
            return Err("pinned files exceed 256 MiB".into());
        }
        bytes.push(file);
    }
    let files: Vec<OfbizFile<'_>> = manifest
        .files
        .iter()
        .zip(&bytes)
        .map(|(pin, bytes)| OfbizFile {
            path: &pin.path,
            role: pin.role,
            bytes,
        })
        .collect();
    let config = OfbizConfig {
        system: manifest.system.clone(),
        repository: manifest.repository.clone(),
        revision: manifest.revision.clone(),
        observed_at: manifest.observed_at.clone(),
        licence: manifest.licence.clone(),
        pins: manifest
            .files
            .iter()
            .map(|pin| (pin.path.clone(), pin.sha256.clone()))
            .collect(),
    };
    let import = OfbizReader.import(&files, &config);
    if let Some(target) = emit {
        let text = import.canonical_json().map_err(|e| e.to_string())?;
        std::fs::write(target, text).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let report = summary(&import);
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    if refused(&import) {
        return Err(text);
    }
    Ok(text)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut format = None;
    let mut manifest = None;
    let mut originals = None;
    let mut emit = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--originals" => originals = args.next().map(PathBuf::from),
            "--emit" => emit = args.next().map(PathBuf::from),
            _ if format.is_none() => format = Some(arg),
            _ if manifest.is_none() => manifest = Some(PathBuf::from(arg)),
            _ => {
                writeln!(std::io::stderr().lock(), "unexpected argument {arg}").ok();
                return ExitCode::from(2);
            }
        }
    }
    let (Some("ofbiz"), Some(manifest), Some(originals)) = (format.as_deref(), manifest, originals)
    else {
        writeln!(
            std::io::stderr().lock(),
            "usage: norn-import-read ofbiz <manifest.json> --originals <dir> [--emit <file>]"
        )
        .ok();
        return ExitCode::from(2);
    };
    match run(&manifest, &originals, emit.as_deref()) {
        Ok(text) => {
            writeln!(std::io::stdout().lock(), "{text}").ok();
            ExitCode::SUCCESS
        }
        Err(text) => {
            writeln!(std::io::stderr().lock(), "{text}").ok();
            ExitCode::FAILURE
        }
    }
}
