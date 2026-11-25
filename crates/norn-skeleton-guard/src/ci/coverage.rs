//! Base-retained coverage obligations: every core obligation a subsystem enrolls on the trusted
//! base stays enrolled on the candidate.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Obligations = BTreeMap<String, BTreeSet<String>>;

/// The split coverage: subsystem metadata and independently addressed obligation entries.
const COVERAGE_MANIFEST: &str = "verification/coverage-v1/manifest";
const COVERAGE_SUBSYSTEMS: &str = "verification/coverage-v1/manifest/subsystems";

pub(super) fn retained_obligations(root: &Path, base: &Path) -> Result<(), String> {
    let after = enrolled_obligations(root)?;
    for (id, required) in enrolled_obligations(base)? {
        if !after
            .get(&id)
            .is_some_and(|actual| required.is_subset(actual))
        {
            return Err(format!("{id}: retained core obligations were removed"));
        }
    }
    Ok(())
}

fn enrolled_obligations(root: &Path) -> Result<Obligations, String> {
    let manifest = root.join(COVERAGE_MANIFEST);
    if !manifest.join("index.json").is_file() {
        return Err(format!("{COVERAGE_MANIFEST}/index.json is missing"));
    }
    let mut enrolled = Obligations::new();
    let entries = std::fs::read_dir(root.join(COVERAGE_SUBSYSTEMS))
        .map_err(|error| format!("missing source enrollment: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let raw = name.to_str().ok_or("invalid subsystem name")?;
        let id = raw;
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(format!("{COVERAGE_SUBSYSTEMS}: unknown entry {raw}"));
        }
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            return Err(format!("{COVERAGE_SUBSYSTEMS}: unknown entry {raw}"));
        }
        let required = entry_obligations(&entry.path())?;
        if enrolled.insert(id.to_owned(), required).is_some() {
            return Err(format!("duplicate subsystem {id}"));
        }
    }
    Ok(enrolled)
}

fn entry_obligations(directory: &Path) -> Result<BTreeSet<String>, String> {
    use sha2::{Digest, Sha256};
    let read = |path: &Path| -> Result<serde_json::Value, String> {
        if !path
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err(format!("{} is not a regular entry", path.display()));
        }
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    let metadata = read(&directory.join("index.json"))?;
    if metadata.get("obligations") != Some(&serde_json::json!({"$entries": "obligations"})) {
        return Err("missing subsystem obligations entry field".to_owned());
    }
    let folder = directory.join("obligations");
    match folder.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("obligations is not a regular directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.to_string()),
    }
    let mut result = BTreeSet::new();
    for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let value = read(&entry.path())?;
        let id = value
            .get("value")
            .and_then(serde_json::Value::as_str)
            .ok_or("invalid obligation entry value")?;
        if value.as_object().is_none_or(|object| object.len() != 2)
            || value
                .get("order")
                .and_then(serde_json::Value::as_u64)
                .is_none()
        {
            return Err("invalid obligation entry envelope".to_owned());
        }
        let mut hash = Sha256::new();
        hash.update(b"entry:string\0");
        hash.update(id.as_bytes());
        let expected = format!("{:x}.json", hash.finalize());
        if entry.file_name().to_str() != Some(expected.as_str()) || !result.insert(id.to_owned()) {
            return Err("duplicate or mismatched obligation entry identity".to_owned());
        }
    }
    Ok(result)
}
