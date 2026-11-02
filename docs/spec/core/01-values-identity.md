# Core 01: Canonical values and identities

Purpose [Astra]: one representation for exact values, references, scope, time and content identity across execution, transport and evidence. This spec adopts the [common conventions](README.md).

## IR contract

Reuse `PropertyType`, `PropertyConstraints`, `DecimalLiteral`, `UnitRef`, `RuntimeValue`, `KeySpec`, `IdentityRule`, `ObjectTypeName`, `PropertyId` and Function `Digest`/`BlobRef`. Extend their checked projections; do not invent domain scalar types.

| Target semantic record | Required fields and constraints |
|---|---|
| Value contract | `type: PropertyType`, `constraints: PropertyConstraints`, unit and scale derived solely from `type` (`PropertyType::Decimal` requires scale), `rounding` derived solely from the optional shared `PropertyType::Decimal.rounding` (`Rounding`, default `refuse`); arithmetic declares a rounding site, never ambient locale. |
| Object identity | `scope: Scope`, `id: opaque stable ID`, `created_under: Pin`; independent of mutable business keys and concrete subtype labels. |
| Key binding | `object_id`, `key: KeySpec`, `canonical_components: [RuntimeValue]`, `valid_from`, `valid_to?`, `recorded_at`, `source_aliases: [Alias]`; uniqueness domain is explicit root type + scope + key name. |
| Alias | `connector_instance`, `source_type`, `source_key`, `object_id`, valid/recorded intervals; source IDs cannot become native authority. |
| Temporal context | `effective_at: UtcTimestamp`, `recorded_at: Revision`, `calendar: Pin`, `timezone_version`, `dst_policy: reject/earlier/later`; intervals are half-open `[from,to)`. |
| Content reference | existing `BlobRef {digest, version?, size_bytes}` plus declared retention/access policy; bytes must match digest before use. |

Definition identity is exact artifact/version/digest plus stable member ID. Renaming a label changes presentation only. A same-version edit changes digest and invalidates admission/evidence; it cannot replace an immutable published release.

The Decimal wire declaration accepts optional `rounding` using the shared `Rounding` modes (`refuse`, `toward_zero`, `half_even`, `half_away_from_zero`); omission means exact/refuse. The declared scale is the sole result-scale authority at a rounding site. The Norn-owned rescale adapter delegates to `ScaledArithmetic` at scales 0–18; the existing exact storage/transport range remains 0–28. Until expression/store binding is qualified, artifact admission refuses non-refuse declarations as `core.values.inexact`: wire decoding alone does not activate a rounding policy.

The declaration adapter is checked against generated signed-integer references. A production-core Kani harness covers all u32 scale derivations and fixed full-width coefficient/decimal overflow witnesses; this scope does not cover IR decoding, adapter parsing or the remaining expression/store binding.

Within the single `ScaledArithmetic`, exact `+`/`−` derive the maximum operand scale, `×` derives their sum and comparisons align scales exactly, refusing a derived scale above 18 or coefficient overflow; division, rescale and `round` require an explicit result scale and rounding at a destination Decimal or `round(x, n, mode)` site.

The explicit exact-expression runtime uses this same VM/core, retains intermediate scales and meters refusal paths; division requires a destination with its effective mode, and final Decimal quantization happens only at the destination. Plain Number and OrderedFold keep their existing execution paths. Expression admission, explicit round lowering and store activation remain separate cuts; unsupported operations refuse.

A destination Decimal with omitted rounding supplies effective Refuse just like explicit Refuse: exact quotients are accepted, nonzero remainders refuse, and their type-environment fingerprints canonicalize omission to Refuse; predicates without a destination require explicit round(value, scale, mode) around division.

VAL-ROUND keeps `norn.expr.env/1` bytes and pins unchanged by omitting canonical Refuse, adding a rounding field only for newly admitted non-default policies that strict older decoders reject; a version bump is required only when existing bytes or their meaning change.

## Evaluation and enforcement

1. Construction decodes exact numeric tokens without a float round trip; rejects non-finite, range/scale overflow and unknown units. Canonical equality ignores representational trailing zeroes but preserves declared type/unit and original source evidence bytes separately.
2. Admission resolves key components, normalization order, type/unit compatibility and uniqueness roots. Text admission follows existing NFC-only semantics; it must not silently normalize signed source bytes. Missing and explicit null differ; nullable is declared.
3. Commit canonicalizes keys with the admitted `IdentityRule`, fences key absence/occupancy and atomically appends identity/alias changes. A key collision never silently merges objects. Reference lookup uses scope + stable ID and validates target type at the selected revision.
4. Posted object identities and aliases are never reused after tombstoning. A governed merge records survivor and redirect history; a split creates new IDs with lineage. Neither rewrites old event references. Ordinary deletes cannot leave required links dangling.
5. Arithmetic uses compatible dimensions; currency conversion requires an explicit rate observation, time and rounding movement. Division by zero refuses. No number combines money with fine grams. Timestamp comparison uses instants; calendar arithmetic uses its pinned calendar, including DST choice.

| Invariant | Requirement / layer | Stable target diagnostic |
|---|---|---|
| VAL-EXACT-1 | Exact transport → evaluator → commit/replay equality; all layers | `core.values.inexact` |
| VAL-UNIT-1 | Compatible dimension/unit before arithmetic; admission/evaluation | `core.values.unit_mismatch` |
| ID-STABLE-1 | Label/key changes preserve object and definition identity; commit | `core.identity.reuse` |
| ID-UNIQUE-1 | One canonical key binding across specialisations per scope/interval; commit | `core.identity.collision` |
| ID-REF-1 | Required references resolve within authorized scope; admission/commit | `core.identity.unresolved_reference` |
| TIME-PIN-1 | Explicit effective/recorded time and pinned calendar; admission/commit | `core.values.time_ambiguous` |
| BLOB-INTEGRITY-1 | Exact retained content or explicit unavailable result; evaluation | `core.values.content_unavailable` |

Evidence expected: structural codec/schema checks; runtime-enforced uniqueness/reference constraints; sampled transport/replay correspondence; generated arithmetic side conditions over the same exact-number semantics. Do not infer universal identity correctness from a hash function.

## Named acceptance scenarios

- **VAL-ROUNDTRIP:** Given `9007199254740993` and `0.10` exact inputs, when sent through each governed surface and replayed, then numeric identity is unchanged; float coercion refuses.
- **VAL-ROUND:** Given scale 2 with midpoint-away rounding, when evaluating `1.005` and `-1.005`, then results are `1.01` and `-1.01`; a zero divisor refuses without effects.
- **ID-COLLISION:** Given two subtypes sharing a key root, when concurrent creates use the same normalized key, then exactly one commits; another tenant may use its own same key.
- **ID-RENAME:** Given a posted event linked to object A, when A's label/business key changes and is later tombstoned, then historical links still resolve A; reusing A's identity refuses.
- **TIME-DST:** Given an ambiguous local timestamp without a pinned disambiguation policy, when admitted, then refuse; an explicit earlier/later choice replays identically.
- **BLOB-MISSING:** Given an archived receipt with unavailable bytes, when requested for recomputation, then expose unavailable evidence and do not synthesize a result.

## Main and gaps

At the README baseline: `crates/norn-ir/src/entity_v2/{object,property}.rs`, `runtime.rs`, `function.rs`; `crates/norn-canonical/src/{lib,number}.rs`; `crates/norn-checker/src/property_codec.rs`; `crates/norn-cell/tests/exact_values.rs`; `crates/norn-kernel/src/observations.rs` are substrate. General stable alias/merge/split history, bitemporal keys and calendar policies are not established. WellDB exact-decimal work #103 is open; its numeric limits must match the shared profile before engine equivalence is claimed.

## Work breakdown (dependency order; each PR ≤400 changed lines)

1. Freeze shared exact value/unit profile and add VAL-ROUNDTRIP/VAL-ROUND conformance; reuse existing codecs (CI + core laws).
2. Add scoped stable identity/key-binding contract with ID-COLLISION admission tests; unsupported alias mutations refuse.
3. Enforce key/alias history and reference fences at commit; add concurrent collision/tombstone tests.
4. Add temporal/calendar/content-reference pins and TIME-DST/BLOB-MISSING tests; align 06/08 temporal lookup.
5. Add governed merge/split plans after 04/06 migration checks; prove old-reference preservation with ID-RENAME extensions.

The shared type checker marks actual Decimal reads and declared destinations; typed consumer admission admits derived exact predicates and, within the exact profile, rejects destination-free division or unsupported inexact functions as `core.values.inexact`, and the typed consumer API binds that admission and the shared exact VM to the same environment and source. Structured Decimal destinations refuse until member-wise runtime binding is available. Kernel invariant binding follows in its own cut. Other consumer/store activation and explicit round lowering remain separate cuts.
