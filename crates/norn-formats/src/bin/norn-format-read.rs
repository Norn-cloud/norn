//! `norn-format-read <manifest.json> --originals <dir> [--check]`
//!
//! Reads a pack's hash-pinned source originals with the format reader the
//! manifest names, refuses any error diagnostic, and writes (or, with
//! `--check`, verifies) the pack's generated `Norn.sourceSchema` module.
//! Standards and systems are packs: adding one needs a manifest, not Rust.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use norn_formats::SchemaImporter;
use norn_formats::pack::{ModuleSpec, render_source_module};
use norn_formats::xsd::{XsdConfig, XsdFile, XsdReader};
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
    /// The module to generate.
    module: ModuleSpec,
}

fn run(manifest_path: &Path, originals: &Path, check: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
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
    let schema = match manifest.format.as_str() {
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
                .ok_or("the reader produced no source schema")?
        }
        other => return Err(format!("no reader for format {other:?}")),
    };
    let module = render_source_module(&schema, &manifest.module)?;
    let target = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&manifest.module.path);
    if check {
        let committed =
            std::fs::read_to_string(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        if committed != module {
            return Err(format!(
                "{} is stale; rerun without --check",
                target.display()
            ));
        }
    } else {
        std::fs::write(&target, module).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(())
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
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--originals" => originals = args.next().map(PathBuf::from),
            "--check" => check = true,
            _ if manifest.is_none() => manifest = Some(PathBuf::from(arg)),
            _ => {
                return report(&format!("unexpected argument {arg}"), 2);
            }
        }
    }
    let (Some(manifest), Some(originals)) = (manifest, originals) else {
        return report(
            "usage: norn-format-read <manifest.json> --originals <dir> [--check]",
            2,
        );
    };
    match run(&manifest, &originals, check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(&error, 1),
    }
}
