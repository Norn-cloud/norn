//! Bind fuel evidence to the complete checker/evaluator implementation.
#![allow(clippy::print_stdout, reason = "Cargo build protocol")]

use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

include!("build_support.rs");

fn sources(
    root: &Path,
    path: &Path,
    output: &mut String,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            sources(root, &entry.path(), output)?;
        }
    } else if path.extension().is_some_and(|extension| extension == "rs")
        || path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml" || name == "Cargo.lock")
    {
        let name = path
            .strip_prefix(root)?
            .iter()
            .map(|component| component.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let contents = normalize_source(&fs::read_to_string(path)?);
        writeln!(output, "({name:?}, {contents:?}),")?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = manifest.join("../..").canonicalize()?;
    let mut output = String::from("&[\n");
    // Whole source trees intentionally over-invalidate: new modules and caps,
    // exact-site lowering, value operations and proc macros cannot be omitted.
    for path in [
        "crates/norn-expr/src",
        "crates/norn-expr/build.rs",
        "crates/norn-expr/build_support.rs",
        "crates/norn-expr/Cargo.toml",
        "crates/norn-ir/src",
        "crates/norn-ir/Cargo.toml",
        "crates/norn-canonical/src",
        "crates/norn-canonical/Cargo.toml",
        "vendor/zen/core",
        "Cargo.toml",
        "Cargo.lock",
    ] {
        sources(&root, &root.join(path), &mut output)?;
    }
    output.push_str("]\n");
    fs::write(
        PathBuf::from(std::env::var("OUT_DIR")?).join("fuel_sources.rs"),
        output,
    )?;
    Ok(())
}
