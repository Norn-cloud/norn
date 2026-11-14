# Core boundaries and provenance

This test and CI crate checks the retained Cargo boundaries, deterministic core,
canonical-byte ownership, exact source enrollment and shipped-file provenance.
It is never a runtime dependency.

The crate is its own Cargo workspace (D95): its `Cargo.toml` and `Cargo.lock` are
in this gated directory, outside the root workspace, so the root manifest and
lockfile never change what the trusted lane builds.

Run `cargo xtask` from the repository root: the alias names the guard's manifest by a
relative path.

`cargo test --locked --manifest-path crates/norn-skeleton-guard/Cargo.toml` runs the boundary and negative
provenance regressions. `cargo xtask ci provenance` checks the candidate;
`cargo xtask ci provenance --root PATH --distribution --base-root BASE` checks a
fully expanded distribution against trusted source facts. The provenance engine
is unchanged from the private cut's reviewed implementation.

The hosted CI runs `scripts/ci/core.sh`. Fork code runs only on GitHub-hosted
runners without repository secrets. The trusted lane builds the base's checker,
reads candidate content as data, preserves core obligations and validates exact
source enrollment using the base's Python validator. Workflow and gate contract
changes require a reviewed base-policy change before updating those contracts.
