# norn-cell-wasm

The cell ABI (T4-CELL-4, #2678): the JSON-in/JSON-out surface
`apps/cell-shim` calls so the Durable Objects host runs the kernel's own
`CellHost` commit contract — `norn_kernel::check_commit` and the
`CellCommit`/`Observed` wire types — instead of re-implementing it in
TypeScript. Ring 1; see `AGENTS.md`.

## Functions

Every argument and result is JSON text. Invalid *input* throws an `Error`
(`invalid <argument>: …`); a refused commit is a verdict, not an error.

| JavaScript | Rust | Result |
|---|---|---|
| `decodeCommit(commitJson, modelJson)` | `decode_commit_json` | `{format: "norn-cell-abi/plan@4", consumes, objects, writes, links, linkReads, effects}` — the consumed delivery, every canonical object the check reads, each write `{object, objectType, values}` (canonical object, type written as, exact values text), each link write `{link, values}` (canonical link), each bounded outgoing adjacency read `{source, link, maxRecords, maxCanonicalBytes}`, and each enqueue with its exact `Command`/`Timer` text |
| `checkCommit(commitJson, viewJson, modelJson)` | `check_commit_json` | `{format: "norn-cell-abi/verdict@4", accepted: false, refusal}` or `{…, accepted: true, seq, written, linked, receipt, entry}` |
| `resolveKey(requestJson, modelJson)` | `resolve_key_json` | `{canonical, isA}` for `{key, concrete}`, `{canonicalLink}` for `{link}` |
| `decodeObserved(observedJson)` | `decode_observed_json` | `{id}` |
| `validateInput(inputJson)` | `validate_input_json` | nothing; throws unless the text is one JSON value |
| `version()` | `version_json` | `{abi: "norn-cell-abi/4", crateVersion, planFormat, verdictFormat, maxSafeInteger}` |

`modelJson` accepts the legacy `ObjectTypeDef` array for models without
interfaces, or `{ "objectTypes": [...], "interfaces": [...], "propertyTypes": [...] }`
(`propertyTypes` optional) for the
complete admitted model. The kernel's shared `TypeCatalog` checks canonical
identity through type hierarchies, interface membership and invariants, link
declarations, admissible target types, and cardinality. Without types no link
commits (fail closed); an implemented interface missing from the model also
fails closed.

`viewJson` is `{seq, pending, objects, linkVersions, linkReads}`: what the shim read
inside its commit transaction (the current ledger position, whether the
consumed delivery is pending, each plan object's `{version, concrete, links}`,
each plan link's version, in order, and one complete bounded adjacency per
plan `linkReads` entry). Each `linkReads` item is a canonically ordered array of
`{key, version}` live targets; `[]` is a complete empty set and `null` means
the host could not prove completeness within the plan's slice-profile bounds.
The shim must enforce both the row count and aggregate UTF-8 target-key byte
limit before materializing target keys for JavaScript. `maxCanonicalBytes`
is the aggregate UTF-8 byte count of target key strings. Targets are ordered
by canonical target object type, then key, in UTF-8 byte order. A set over
either bound must be represented as `null`, which refuses with
`commit.unsupported_link_read`; an oversized, duplicated, or misordered array
is malformed ABI input and makes `checkCommit` throw.

The view also accepts optional `implicitObjects` (`{key, state}` with the
same object-state shape), `implicitLinkReads` (`{source, link, targets}`),
and `implicitPresenceReads` (`{source, link, excluded, present}`). Missing
arrays default to empty. `targets` is a complete ordered array of
`{key, version}` or `null` when completeness cannot be established;
`present` is `true`, `false`, or `null` when the presence query is unavailable.

When admission needs more host state, its refused verdict includes
`readRequest` and `readLimits: {maxRecords, maxCanonicalBytes}`. Requests are:

- `{kind: "object", key}`: add `{key, state}` to `implicitObjects`.
- `{kind: "link_targets", source, link}`: add the complete bounded target
  snapshot to `implicitLinkReads`.
- `{kind: "link_presence", source, link, excluded}`: query whether a live
  target exists outside the canonical `excluded` keys and add the answer
  with the same source, link and exclusions to `implicitPresenceReads`.

The host answers within the same commit transaction and retries `checkCommit`
with these additional reads until a final verdict is reached. It must enforce
both returned limits before materializing data: `maxRecords` bounds the
records and `maxCanonicalBytes` bounds aggregate UTF-8 key bytes. Implicit
object/adjacency reads also share a byte budget; presence exclusions include
the source key in their byte budget. Over-budget adjacency and unavailable
presence queries use `null`; truncated data must never claim completeness.
Duplicate, misordered or oversized implicit snapshots are malformed ABI input.

`norn-cell-abi/2` (#3013) added link writes; `norn-cell-abi/3` (#3018) adds
the model argument, canonical keys, and the kernel's `commit.link_target`,
`commit.link_cardinality`, and `commit.type_mismatch` refusals. ABI v4 adds
complete bounded link-read snapshots so the kernel can compare the current
adjacency set and every target link version inside the host transaction. Its
`verdict@4` format adds the link-read refusal fields: `sourceKey`, a `link`
name string, and `actual`/`max` for bounded reads. The new refusal codes are
`commit.link_read_conflict`, `commit.unsupported_link_read`,
`commit.link_read_too_large`, `commit.duplicate_link_read`, and
`commit.invalid_link_read`. Link mutation refusals retain a full key in `link`.

## Exact payloads (T3-NUM-1, #2770)

Fact values, events, command and timer payloads, and kernel inputs cross as
`serde_json::value::RawValue` — validated JSON, never converted to `f64` —
and come back as JSON *strings*, so the shim's `JSON.parse` of a plan or
verdict never touches a payload number. Integers the shim handles as numbers
(versions, ledger positions, generations, due times) are refused above
2^53 − 1. `tests/abi.rs` pins the numeric goldens.

## Law traces

`tests/law_traces.rs` samples the `CellHost` law machine
(`norn_testkit::cell_host`) with a fixed seed, runs each sequence on the
reference `MemoryCell`, and records every outcome and the cell state after
each operation into `apps/cell-shim/test/fixtures/cell-host-laws.json`. The
shim replays that file. The test fails when the file is stale:

```bash
NORN_CELL_TRACES_BLESS=1 cargo test -p norn-cell-wasm --test law_traces
```

## Build

```bash
# once: rustup target add wasm32-unknown-unknown
#       cargo install wasm-bindgen-cli --version 0.2.129 --locked
crates/norn-cell-wasm/scripts/build-pkg.sh   # -> apps/cell-shim/wasm (gitignored)
```
