//! `norn-olapd`: the tenant analytics engine next to the cell host (D102).
//! See [`norn_cell_do::olapd`]. `norn-olapd --health` asks the running
//! daemon's `/healthz` and exits 0 when it answers.

use std::io::Write as _;

fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--health") {
        return if norn_cell_do::olapd::healthy() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    let outcome = norn_cell_do::olapd::Config::from_env()
        .and_then(|config| norn_cell_do::olapd::run(config).map_err(|error| error.to_string()));
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _unwritable = writeln!(std::io::stderr(), "norn-olapd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
