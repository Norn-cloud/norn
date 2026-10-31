# DD qualification patches

Source: crates.io archives `timely 0.31.0`, `differential-dataflow 0.25.1`,
`columnation 0.1.2`. MIT licences remain beside their sources. Only library
sources, licences, README and normalized manifests are retained. Example, bench,
test, development binary and dev-dependency manifest entries are omitted.
Own backend code forbids unsafe; upstream dependency unsafe code is
unchanged.

Investigation on 2026-10-28:

- (a) `default-features = false` removes getopts, but bincode and paste are
  unconditional dependencies. It cannot resolve the maintenance advisories.
- (b) Timely HEAD `d49754514be62a3cacde26a5da6e1cd988a66680` and DD HEAD
  `aa8745f93ea8abe131104fc7885ba4fd47e63902` still require them.
- (c) The local patches remove unused DD paste; replace columnation's paste
  dependency with maintained pastey 0.2.3; replace Timely's bincode codec with
  postcard 1.1.3 (std, no default features). All codec call sites migrate, and
  the public wrapper is renamed `Postcard`, without an old-codec alias.

**Wire compatibility:** postcard changes Timely's network and capture-file
encoding. Only matched patched peers may communicate; old capture files cannot
be loaded. WellDB currently uses neither network workers nor Timely capture
files. Its checkpoint contract is a complete source snapshot plus verified
export replay, rebuilding the graph. This is an experimental dependency profile,
not a released Timely wire-format migration.

**Upgrade cost:** three small manifest edits, one wrapper rename, and codec
calls in Timely lib.rs, channels/mod.rs, capture/event.rs. Requalify upstream
updates by comparing against the pinned crates.io archives and rerunning codec,
conformance, wasm and performance gates. No advisory ignore is admitted. Vendored Timely's README doctest include is
corrected to the retained crate-local README; trailing blank EOF lines are
trimmed in five retained upstream files.

Public-port qualification: cross-slice `PointStamp` equality now compares through
the longer coordinate list, padding missing entries with the minimum in both
directions. A regression covers trailing minimum and non-minimum coordinates.
An upstream documentation reference to unshipped Lean proofs is removed; this
port claims no formal qualification for the dependency or WellDB.

## Immutable archive provenance

| crate | upstream commit | crates.io archive SHA-256 |
| --- | --- | --- |
| timely 0.31.0 | `eba4ae5298442cc2475e5ef82277bb135e4a7ea4` | `ced05836c8e471cc85b5a12fbbe9a72568d98ea3b5a79e4a965918bc002108d9` |
| differential-dataflow 0.25.1 | `6e53ac674feacac83b0c3c5d123bdd5db488ef9d` | `2677690d1dfbf4d8427eef2fdcf350ab0b258d3be20d01dd37dca07ca6f50bcc` |
| columnation 0.1.2 | `0e4fe371ac319826dc7a460c704fda54c52a471a` | `b90b1614014f6958477dcdb77a2d489555db48ca61efe94c5cde2b0446933ed1` |

## Proposed upstream PR text (not opened)

Title: Replace unmaintained paste/bincode dependencies in the single-worker profile

DD can remove its unused paste dependency; columnation can use pastey as a
source-compatible proc-macro replacement. Timely can migrate serialization to
postcard, retaining alignment and length framing. This changes network/capture
encoding and renames the codec wrapper, so it needs an explicit upstream format
version decision before release. A feature-selected postcard codec would let
applications opt in without retaining bincode in their dependency closure.

Validation must cover padded container/message round trips, capture event
round trips, single-worker arranged join/reduce on native and both wasm32
targets, and cargo-deny with no maintenance exceptions. WellDB's PR records the
actual outcomes; proposed checks here are not evidence of passing them.
