//! ANALYTICS-FUNCTION-1: pin the native compiler and Cargo configuration.
use std::{env, error::Error, process::Command};
#[allow(
    clippy::disallowed_methods,
    clippy::print_stdout,
    reason = "Cargo build metadata"
)]
fn main() -> Result<(), Box<dyn Error>> {
    let compiler = Command::new(env::var("RUSTC")?).args(["-vV"]).output()?;
    if !compiler.status.success() {
        return Err("runtime compiler identity unavailable".into());
    }
    let compiler = String::from_utf8(compiler.stdout)?;
    let mut config: Vec<_> = env::vars()
        .filter(|(key, _)| {
            key.starts_with("CARGO_PROFILE_")
                || key.starts_with("CARGO_CFG_")
                || key.starts_with("CARGO_FEATURE_")
                || key == "CARGO_ENCODED_RUSTFLAGS"
                || matches!(key.as_str(), "TARGET" | "PROFILE" | "OPT_LEVEL" | "DEBUG")
        })
        .collect();
    config.sort();
    println!(
        "cargo:rustc-env=NORN_NATIVE_RUNTIME_BUILD={:?}",
        (compiler, config)
    );
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
