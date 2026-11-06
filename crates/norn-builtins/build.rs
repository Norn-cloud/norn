//! BUILTIN-PIN-1: the build fails unless the registry pin is this crate's
//! HOST-TREE-1 digest, so a stale pin never compiles into a registry. The
//! digest reaches the crate as `NORN_BUILTINS_SOURCE_TREE`.
#![allow(clippy::print_stdout, reason = "Cargo build protocol")]

use std::path::PathBuf;

#[path = "pin_tree.rs"]
mod tree;

fn main() -> Result<(), String> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let pin = root.join("../../registry/builtins/norn-builtins.json");
    // The whole crate directory and the pin outside it.
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed={}", pin.display());
    let text = std::fs::read_to_string(&pin).map_err(|e| format!("{}: {e}", pin.display()))?;
    let digest = tree::check(&root, &text)?;
    println!("cargo:rustc-env=NORN_BUILTINS_SOURCE_TREE={digest}");
    Ok(())
}
