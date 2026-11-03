//! Compile the authoritative per-code registry without a shared generated file.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

/// Validate every entry and render the sorted static registry.
///
/// # Errors
/// Refuses unknown files, malformed entries, duplicate identities and invalid metadata.
pub(crate) fn generate(directory: &Path, guidance_path: &Path) -> Result<String, String> {
    if !directory
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("diagnostic registry is not a regular directory".to_owned());
    }
    let mut guidance = read_guidance(guidance_path)?;
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
    let strings = regex_lite::Regex::new(r#""(?:\\.|[^"\\])*""#).map_err(|e| e.to_string())?;
    for file in files {
        if !file.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("diagnostic entry is not a regular file".to_owned());
        }
        let text = fs::read_to_string(file.path()).map_err(|e| e.to_string())?;
        let entry: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let object = entry
            .as_object()
            .ok_or("diagnostic entry must be an object")?;
        // Five string keys and five string values are exactly ten JSON string tokens.
        // Extra tokens expose duplicate keys that Value would otherwise discard.
        if object.len() != 5 || strings.find_iter(&text).count() != 10 {
            return Err(
                "diagnostic entry must contain exactly five unique string fields".to_owned(),
            );
        }
        let field = |key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("missing or non-string diagnostic field: {key}"))
        };
        let code = field("code")?;
        let tier = field("tier")?;
        let severity = field("severity")?;
        let message = field("message")?;
        let since = field("since")?;
        let (fix, no_fix) = guidance.remove(code).ok_or_else(|| format!("{code} requires a fix or no_fix reason"))?;

        let segments: Vec<_> = code.split('.').collect();
        if segments.len() != 3
            || segments.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            })
            || file.file_name().to_str() != Some(format!("{code}.json").as_str())
        {
            return Err("diagnostic filename and stable dotted identity must match".to_owned());
        }
        if !matches!(tier, "T0" | "T1" | "T2" | "T3" | "T4" | "T5" | "T6" | "T7")
            || message.trim().is_empty()
            || since.trim().is_empty()
        {
            return Err("invalid tier, message or since version".to_owned());
        }
        let severity = match severity {
            "error" => "Error",
            "warning" => "Warning",
            "info" => "Info",
            _ => return Err("invalid diagnostic severity".to_owned()),
        };
        writeln!(result, "CodeEntry {{ code: {code:?}, tier: Tier::{tier}, severity: DiagnosticSeverity::{severity}, message: {message:?}, since: {since:?}, fix: {fix}, no_fix: {no_fix} }},").map_err(|e| e.to_string())?;
    }
    if !guidance.is_empty() { return Err("guidance names an unregistered diagnostic".to_owned()); }
    result.push_str("];\n");
    Ok(result)
}

fn read_guidance(path: &Path) -> Result<BTreeMap<String, (String, String)>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let entries: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let entries = entries.as_array().ok_or("guidance must be an array")?;
    let strings = regex_lite::Regex::new(r#""(?:\\.|[^"\\])*""#).map_err(|e| e.to_string())?;
    if strings.find_iter(&text).count() != entries.len().saturating_mul(4) { return Err("guidance fields must be unique strings".to_owned()); }
    let mut result = BTreeMap::new();
    for entry in entries {
        let fields = entry.as_object().ok_or("guidance must be an object")?;
        if fields.len() != 2 { return Err("guidance requires code and exactly one fix or no_fix".to_owned()); }
        let code = fields.get("code").and_then(serde_json::Value::as_str).ok_or("guidance requires code")?;
        let (key, value) = match (fields.get("fix"), fields.get("no_fix")) {
            (Some(value), None) => ("fix", value),
            (None, Some(value)) => ("no_fix", value),
            _ => return Err("guidance requires exactly one fix or no_fix".to_owned()),
        };
        let value = value.as_str().filter(|value| !value.trim().is_empty()).ok_or("guidance must not be blank")?;
        let literal = format!("Some({value:?})");
        let pair = if key == "fix" { (literal, "None".to_owned()) } else { ("None".to_owned(), literal) };
        if result.insert(code.to_owned(), pair).is_some() { return Err("duplicate guidance identity".to_owned()); }
    }
    Ok(result)
}

#[cfg(not(test))]
fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=conformance/diagnostic-codes");
    println!("cargo:rerun-if-changed=conformance/diagnostic-guidance.json");
    let generated = generate(Path::new("conformance/diagnostic-codes"), Path::new("conformance/diagnostic-guidance.json"))?;
    let output = std::env::var("OUT_DIR").map_err(|e| e.to_string())?;
    fs::write(Path::new(&output).join("diagnostic_registry.rs"), generated)
        .map_err(|e| e.to_string())
}
