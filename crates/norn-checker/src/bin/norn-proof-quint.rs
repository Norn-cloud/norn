//! Native argument adapter for the checker-owned generated-proof binder.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    norn_checker::obligations::quint::driver::run(&std::env::args().skip(1).collect::<Vec<_>>())
}
