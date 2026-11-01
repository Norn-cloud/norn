//! Compile the authoritative per-code registry without a shared generated file.

use serde::Deserialize;
use std::{fmt::Write as _, fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    code: String,
    tier: String,
    severity: String,
    message: String,
    since: String,
}

/// Validate every entry and render the sorted static registry.
///
/// # Errors
/// Refuses unknown files, malformed entries, duplicate identities and invalid metadata.
pub(crate) fn generate(directory: &Path) -> Result<String, String> {
    if !directory
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("diagnostic registry is not a regular directory".to_owned());
    }
    let mut files = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    files.sort_by_key(std::fs::DirEntry::file_name);
    if files.is_empty() {
        return Err("diagnostic registry is empty".to_owned());
    }
    let mut result = String::from(
        "/// Every registered diagnostic, sorted by code.\npub static REGISTRY: &[CodeEntry] = &[\n",
    );
    for file in files {
        if !file.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("diagnostic entry is not a regular file".to_owned());
        }
        let entry: Entry =
            serde_json::from_slice(&fs::read(file.path()).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let segments: Vec<_> = entry.code.split('.').collect();
        if segments.len() != 3
            || segments.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            })
            || file.file_name().to_str() != Some(format!("{}.json", entry.code).as_str())
        {
            return Err("diagnostic filename and stable dotted identity must match".to_owned());
        }
        if !matches!(
            entry.tier.as_str(),
            "T0" | "T1" | "T2" | "T3" | "T4" | "T5" | "T6" | "T7"
        ) || entry.message.trim().is_empty()
            || entry.since.trim().is_empty()
        {
            return Err("invalid tier, message or since version".to_owned());
        }
        let severity = match entry.severity.as_str() {
            "error" => "Error",
            "warning" => "Warning",
            "info" => "Info",
            _ => return Err("invalid diagnostic severity".to_owned()),
        };
        writeln!(result, "CodeEntry {{ code: {:?}, tier: Tier::{}, severity: DiagnosticSeverity::{severity}, message: {:?}, since: {:?} }},", entry.code, entry.tier, entry.message, entry.since).map_err(|e| e.to_string())?;
    }
    result.push_str("];\n");
    Ok(result)
}

#[cfg(not(test))]
fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=conformance/diagnostic-codes");
    let generated = generate(Path::new("conformance/diagnostic-codes"))?;
    let output = std::env::var("OUT_DIR").map_err(|e| e.to_string())?;
    fs::write(Path::new(&output).join("diagnostic_registry.rs"), generated)
        .map_err(|e| e.to_string())
}
