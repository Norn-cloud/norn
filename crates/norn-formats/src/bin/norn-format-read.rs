//! `norn-format-read <manifest.json> --originals <dir> [--emit <file>]`
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

use norn_formats::SchemaImporter;
use norn_formats::pack::{ProjectionSpec, check_contract, project, render_review_module};
use norn_formats::xsd::{XsdConfig, XsdFile, XsdReader};
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

fn read(manifest: &Manifest, originals: &Path) -> Result<SourceSchemaIr, String> {
    let pins = manifest.admission.files.clone().unwrap_or_default();
    if pins.is_empty() {
        return Err("admission pins no file".into());
    }
    let bytes = pins
        .iter()
        .map(|pin| {
            let relative = pin
                .path
                .strip_prefix(&manifest.originals_base)
                .filter(|rest| !rest.is_empty() && !rest.split('/').any(|step| step == ".."))
                .ok_or_else(|| format!("{} is not under originalsBase", pin.path))?;
            std::fs::read(originals.join(relative)).map_err(|e| format!("{}: {e}", pin.path))
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

fn run(manifest_path: &Path, originals: &Path, emit: Option<&Path>) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let schema = read(&manifest, originals)?;
    let projection = project(&schema, &manifest.projection)?;
    if let Some(target) = emit {
        let parent = target.parent().filter(|p| !p.as_os_str().is_empty());
        let parent = parent.unwrap_or_else(|| Path::new("."));
        if let (Some(repo), Some(dir)) = (
            repository_of(manifest_path),
            std::fs::canonicalize(parent).ok(),
        ) && dir.starts_with(&repo)
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
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--originals" => originals = args.next().map(PathBuf::from),
            "--emit" => emit = args.next().map(PathBuf::from),
            "--check" => {}
            _ if manifest.is_none() => manifest = Some(PathBuf::from(arg)),
            _ => return report(&format!("unexpected argument {arg}"), 2),
        }
    }
    let (Some(manifest), Some(originals)) = (manifest, originals) else {
        return report(
            "usage: norn-format-read <manifest.json> --originals <dir> [--emit <file>]",
            2,
        );
    };
    match run(&manifest, &originals, emit.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(&error, 1),
    }
}
