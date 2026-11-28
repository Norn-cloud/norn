# norn-authoring

Ring 1 native Rust TypeScript authoring compiler and formatter embedding `tsrs_*@7.1.0-beta.tsrs5`. The crate boundary is `boundary.json`, enforced by `norn-skeleton-guard`.

## What lives here (Slice 1)

- `profile`: embeds `tools/norn-ts/profile/norn-effect-1.json` and `tools/norn-ts/profile/norn-authoring.d.ts`, validates the profile schema on load, and provides admitted/rejected symbol lookup tables.
- `vfs`: `BoundedVfs`, a read-only, symlink-free, in-memory `tsrs_vfs::FS` implementation serving embedded `tsrs_vfs::bundled` `lib.*.d.ts` files, virtual `/@norn/authoring/index.d.ts`, and caller-supplied TypeScript modules under strict file-count and byte caps (fail-closed on quota breach).
- `program`: `AuthoringProgram`, wrapping `tsrs_compiler::new_program` with the pinned `norn-effect` compiler options, region-isolated allocations, syntactic/semantic diagnostic collection, and alias-following symbol identity resolution via `tsrs_checker`.
- `fmt`: `format_source`, a zero-Node/zero-`typescript@6` port of `tools/norn-ts/src/fmt.ts` satisfying `FMT-SEMANTIC-1` (never executes authored source and never deletes unused imports).

## Boundary

- Ring: Ring 1 — native authoring toolchain and host perimeter.
- Allowed `norn-*` dependencies: `norn-canonical`, `norn-ir`.
- No `unsafe` code (`unsafe_code = "forbid"` inherited from workspace lints).
- Never executes authored TypeScript source or reads the host filesystem inside `BoundedVfs`.
