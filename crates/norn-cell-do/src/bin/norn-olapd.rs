//! `norn-olapd`: the tenant analytics engine next to the cell host (D102).
//! See [`norn_cell_do::olapd`]. `norn-olapd --health` asks the running
//! daemon's `/healthz` and exits 0 when it answers. `norn-olapd --new-key
//! <kid>` prints a fresh signing key for the cell host
//! (`NORN_OLAP_SIGNING_KEY`) and the keyring line that verifies it.

use std::io::Write as _;

fn main() -> std::process::ExitCode {
    const USAGE: &str = "usage: norn-olapd [--health | --new-key <kid>]";
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--health") => {
            if let Some(extra) = args.next() {
                let _unwritable = writeln!(
                    std::io::stderr(),
                    "norn-olapd: unexpected argument `{extra}`\n{USAGE}"
                );
                return std::process::ExitCode::FAILURE;
            }
            return if norn_cell_do::olapd::healthy() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            };
        }
        Some("--new-key") => {
            let (Some(kid), None) = (args.next(), args.next()) else {
                let _unwritable = writeln!(std::io::stderr(), "{USAGE}");
                return std::process::ExitCode::FAILURE;
            };
            return match norn_cell_do::olapd::new_key(&kid) {
                Ok((secret, public)) => {
                    let mut out = std::io::stdout();
                    let _unwritable = writeln!(out, "NORN_OLAP_SIGNING_KEY={secret}");
                    let _unwritable = writeln!(out, "keyring line: {public}");
                    std::process::ExitCode::SUCCESS
                }
                Err(error) => {
                    let _unwritable = writeln!(std::io::stderr(), "norn-olapd: {error}");
                    std::process::ExitCode::FAILURE
                }
            };
        }
        None => {}
        Some(other) => {
            let _unwritable = writeln!(
                std::io::stderr(),
                "norn-olapd: unknown argument `{other}`\n{USAGE}"
            );
            return std::process::ExitCode::FAILURE;
        }
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
