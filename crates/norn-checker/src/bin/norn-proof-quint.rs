//! Native argument adapter for the checker-owned generated-proof binder.
use norn_checker::obligations::quint::driver::{DriverMode, DriverRequest, run};
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [mode, original, mutant, base, profile, work, _tools] = args.as_slice() else {
        return Err(
            "usage: norn-proof-quint prepare|bind ORIGINAL MUTANT BASE BOUNDS OUTPUT_DIR TOOL_ROOT"
                .into(),
        );
    };
    let mode = match mode.as_str() {
        "prepare" => DriverMode::Prepare,
        "bind" => DriverMode::Bind,
        _ => return Err("invalid driver mode".into()),
    };
    run(DriverRequest {
        mode,
        original: Path::new(original),
        mutant: Path::new(mutant),
        base: Path::new(base),
        profile: Path::new(profile),
        work: Path::new(work),
    })
}
