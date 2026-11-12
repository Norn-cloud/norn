//! `norn-dev-host`: the native host behind `norn dev` (#207). See
//! [`norn_cell_do::dev`].

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use clap::Parser as _;
    norn_cell_do::dev::execute(&norn_cell_do::dev::DevArgs::parse())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
