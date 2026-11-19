# Rust safety baseline

The lint, dependency and `unsafe` rules for the Rust workspace, and where each one is set. The files are the source of truth; this page explains them.

| File | What it sets |
|---|---|
| `Cargo.toml` `[workspace.lints]` | Lint levels for every crate |
| `clippy.toml` | How some Clippy lints behave, plus banned types and methods |
| `deny.toml` | Dependency policy for `cargo deny` |
| crate roots (`lib.rs`) | Stricter local rules, and staged downgrades |

## How it runs

Every crate in `crates/` opts in with `[lints] workspace = true`. The skeleton guard (`crates/norn-skeleton-guard`) is its own Cargo workspace, so its `Cargo.toml` repeats the same table under `[lints.*]`.

`scripts/ci/core.sh` enforces it:

- `clippy` phase: `cargo clippy --locked --workspace --all-targets -- -D warnings`, and the same for the guard. With `-D warnings`, every `warn` lint fails the build too.
- `wasm` phase: Clippy with `-D warnings` for `norn-checker-wasm` and `norn-cell-do` on `wasm32-unknown-unknown`.
- `doc` phase: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
- `guards` phase: `cargo deny --locked check` for the workspace and for the guard.

## Workspace lint table (`Cargo.toml`)

**Rust**

- `unsafe_code = "forbid"`. See [Unsafe code](#unsafe-code).
- `unsafe_op_in_unsafe_fn = "deny"`.
- Warn: `missing_docs`, `unreachable_pub`, `unused_lifetimes`, `unused_extern_crates`, `rust_2018_idioms`.
- `unexpected_cfgs` warns, with `cfg(kani)` declared so Kani harnesses compile.

**Rustdoc** (all deny): `broken_intra_doc_links`, `private_intra_doc_links`, `bare_urls`, `invalid_rust_codeblocks`.

**Clippy**

- `clippy::all` and `clippy::pedantic` warn.
- Panics and unchecked operations are denied: `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `as_conversions`, `arithmetic_side_effects`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`, `mem_forget`, `large_stack_arrays`, `await_holding_lock`.
- `allow_attributes_without_reason` is denied: every `#[allow]` must say why.
- `unsafe` hygiene is denied: `undocumented_unsafe_blocks`, `missing_safety_doc`, `multiple_unsafe_ops_per_block`, `unnecessary_safety_comment`.
- A chosen set of `clippy::restriction` lints is denied (the whole group is not enabled, because some of its lints contradict `pedantic` or each other): `map_err_ignore`, `clone_on_ref_ptr`, `string_slice`, `lossy_float_literal`, `float_cmp_const`, `fn_to_numeric_cast_any`, `suspicious_xor_used_as_pow`, `rest_pat_in_fully_bound_structs`, `unneeded_field_pattern`, `empty_drop`, `same_name_method`, `verbose_file_reads`.
- Exhaustiveness, ignored results and avoidable copies are denied (#3299): `wildcard_enum_match_arm`, `match_wildcard_for_single_variants`, `unwrap_in_result`, `let_underscore_must_use`, `let_underscore_future`, `redundant_clone`, `unnecessary_to_owned`, `needless_collect`, `or_fun_call`, `format_collect`, `format_push_string`, `inefficient_to_string`, `large_types_passed_by_value`, `needless_pass_by_value`, `large_enum_variant`, `large_futures`, `large_stack_frames`, `box_collection`, `vec_box`, `rc_buffer`.
- `disallowed_types = "allow"` at workspace level. Core crates turn it back on (below).

## `clippy.toml`

**Tests may relax.** `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`, `allow-dbg-in-tests` and `allow-print-in-tests` are all `true`. The deny tier targets production code.

**Banned types** (`disallowed-types`): `std::collections::HashMap` and `HashSet`, because their iteration order is not deterministic. Use `BTreeMap`/`BTreeSet` or `IndexMap`/`IndexSet`. The workspace leaves this lint off; it is denied at the crate root of `norn-kernel`, `norn-canonical`, `norn-checker` and `norn-graph`.

**Banned methods** (`disallowed-methods`, part of `clippy::all`, so an error under `-D warnings`):

| Method | Why |
|---|---|
| `std::process::exit` | Skips destructors and the execution ledger. Return an error to `main`. |
| `std::mem::forget` | Hides resource leaks. |
| `std::env::set_var` | Mutating the process environment is racy. Pass config explicitly. |
| `std::process::Command::new` | Spawning a process is a capability. |
| `std::time::SystemTime::now` | A wall-clock read breaks replay. Inject a clock. |
| `jiff::Timestamp::now`, `jiff::Zoned::now` | Same, inside jiff. |
| `jiff::tz::TimeZone::system`, `try_system`, `jiff::tz::db` | The host time zone breaks replay. Resolve zones from the pinned `jiff-tzdb`. |

### Capability discipline preview

The last group (process spawn, wall clock, host time zone) is a preview of capability discipline: ambient authority must come in through an explicit value the caller passes, not be grabbed inline. There is no `norn-caps` crate yet; the `Command::new` message says to use a capability wrapper "once norn-caps lands". Until then the ban stops new inline uses, and the code that needs a clock gets one passed in.

## Dependency policy (`deny.toml`)

Run it with `cargo deny check`.

- **Platforms.** Only `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin` are evaluated, so Windows-only edges do not widen the policy. All features are on.
- **Advisories.** Vulnerabilities and unsound advisories fail for any crate in the graph. Yanked crates fail. Unmaintained advisories fail only for crates a workspace member depends on directly (`unmaintained = "workspace"`).
- **Licenses.** Allowed: Apache-2.0 (also with LLVM-exception), MIT, MIT-0, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Unicode-DFS-2016, Zlib, CC0-1.0, CDLA-Permissive-2.0. Copyleft licenses are not on the list, so any such dependency fails. Confidence threshold 0.8. Workspace crates themselves are not checked.
- **Bans.** Wildcard versions fail (path dependencies are fine). Duplicate major versions only warn for now. Some crates may only be used through one wrapper crate:

  | Crate | Only through |
  |---|---|
  | `worker` | `norn-cell-do` |
  | `rusqlite` | `norn-cell` |
  | `cedar-policy` | `norn-authz` (and `cedar-policy-symcc`) |
  | `zen-engine` | `norn-decisions` |
  | `zen-expression` | `norn-expr` (and, inside the vendored set, `zen-engine` and `zen-tmpl`) |

- **Sources.** Only crates.io. Unknown registries and every git source fail. The ZEN crates are vendored under `vendor/zen` as path dependencies.

## Unsafe code

`AGENTS.md`: no `unsafe` in kernel or evaluator crates.

- The workspace sets `unsafe_code = "forbid"`, so every crate that inherits the workspace lints forbids it, and a crate cannot `#[allow]` it back.
- `norn-kernel`, `norn-expr`, `norn-decisions`, `norn-datalog` and `norn-graph` also say `#![forbid(unsafe_code)]` in `lib.rs`, so the rule survives if the crate leaves the workspace lints.
- The vendored ZEN crates (`vendor/zen/core/*`) do not use the workspace lint table, but each of their `lib.rs` files has `#![forbid(unsafe_code)]`.
- If `unsafe` is ever allowed somewhere, the deny lints above require a `// SAFETY:` comment on each block, one unsafe operation per block, and a `# Safety` doc section on each `unsafe fn`.

## Crate-root rules and staged burn-down

Some crates add rules at the top of `lib.rs`:

- `norn-kernel`, `norn-canonical`, `norn-checker`, `norn-graph`: deny `clippy::disallowed_types` (no `HashMap`/`HashSet`).
- `norn-ir`: denies `clippy::float_arithmetic`, `integer_division` and `modulo_arithmetic`.

A crate that cannot meet a lint yet may downgrade it locally, at the top of `lib.rs` or on a module, with an `#[allow(..., reason = "TODO(rust-safety): ...")]`. New code must not add violations. Today two crates do this:

- **`norn-ir`**: allows `missing_docs` (183 undocumented public items) and `clippy::pedantic` (about 44 findings), counted 2026-08-19. It re-denies the #3299 lints that `pedantic` would otherwise cover. It is clean at the deny tier.
- **`norn-checker`**: the modules that moved here from `norn-ir` (`envelope`, `posting`, `ontology`, `quantity_ledger`, `sealing`) keep the same `missing_docs` and `clippy::pedantic` allows on their `mod` items, with the same re-denies.

To burn down: fix the findings, delete the `allow`, and run the `clippy` phase. Find what is left with `grep -rn 'TODO(rust-safety)' crates`.

## Property tests

`crates/norn-ir/tests/proptest_ir_roundtrip.rs` is the pattern for new IR types: generate arbitrary values of a canonical IR type and assert the serde round trip is lossless. Add one when you add an IR wire type.
