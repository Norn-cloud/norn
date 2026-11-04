# Core boundaries and provenance

This test and CI crate checks the retained Cargo boundaries, deterministic core,
canonical-byte ownership, exact source enrollment and shipped-file provenance.
It is never a runtime dependency.

`cargo test --locked -p norn-skeleton-guard` runs the boundary and negative
provenance regressions. `cargo xtask ci provenance` checks the candidate;
`cargo xtask ci provenance --root PATH --distribution --base-root BASE` checks a
fully expanded distribution against trusted source facts. The provenance engine
is unchanged from the private cut's reviewed implementation.

The hosted CI runs `scripts/ci/core.sh`. Fork code runs only on GitHub-hosted
runners without repository secrets. The trusted lane builds the base's checker,
reads candidate content as data, preserves core obligations and validates exact
source enrollment using the base's Python validator. Workflow and gate contract
changes require a reviewed base-policy change before updating those contracts.

## Engine dependency invariants (2026-10-31 amendment)

- `ENG-IR-OWNER`: canonical types stay in IR. The engine row admits only
  kernel, checker, projections, cell and IR, for normal, build and dev edges.
- `ENG-NO-REVERSE`: IR cannot depend on the engine, for any dependency kind,
  including before the engine becomes a workspace member.
- `ENG-RING0-PURE`: host runtime dependencies remain forbidden in ring 0,
  including dev dependencies.
- `ENG-PREDECLARED`: the engine policy applies when its package arrives;
  absence requires no placeholder. Present packages still need boundary files
  whose declarations match the executable policy.

`engine_declared_edges_conform_for_every_dependency_kind` checks allowed,
unrelated, reverse and host edges against injected Cargo metadata.
`engine_predeclaration_does_not_require_a_placeholder` checks absence;
`machine_boundaries_match_dependency_policy` covers present boundary data.
