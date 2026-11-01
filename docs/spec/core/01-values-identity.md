# Core 01: Canonical values and identities

Purpose [Astra]: one representation for exact values, references, scope, time and content identity across execution, transport and evidence. This spec adopts the [common conventions](README.md).

## IR contract

Reuse `PropertyType`, `PropertyConstraints`, `DecimalLiteral`, `UnitRef`, `RuntimeValue`, `KeySpec`, `IdentityRule`, `ObjectTypeName`, `PropertyId` and Function `Digest`/`BlobRef`. Extend their checked projections; do not invent domain scalar types.

| Target semantic record | Required fields and constraints |
|---|---|
| Value contract | `type: PropertyType`, `constraints: PropertyConstraints`, unit and scale derived solely from `type` (`PropertyType::Decimal` requires scale), `rounding?: enum(exact, midpoint_away_from_zero, half_even)`; arithmetic declares a rounding site, never ambient locale. |
| Object identity | `scope: Scope`, `id: opaque stable ID`, `created_under: Pin`; independent of mutable business keys and concrete subtype labels. |
| Key binding | `object_id`, `key: KeySpec`, `canonical_components: [RuntimeValue]`, `valid_from`, `valid_to?`, `recorded_at`, `source_aliases: [Alias]`; uniqueness domain is explicit root type + scope + key name. |
| Alias | `connector_instance`, `source_type`, `source_key`, `object_id`, valid/recorded intervals; source IDs cannot become native authority. |
| Temporal context | `effective_at: UtcTimestamp`, `recorded_at: Revision`, `calendar: Pin`, `timezone_version`, `dst_policy: reject/earlier/later`; intervals are half-open `[from,to)`. |
| Content reference | existing `BlobRef {digest, version?, size_bytes}` plus declared retention/access policy; bytes must match digest before use. |

Definition identity is exact artifact/version/digest plus stable member ID. Renaming a label changes presentation only. A same-version edit changes digest and invalidates admission/evidence; it cannot replace an immutable published release.

The initial unit vocabulary is the ordinary pinned `core.units` pack, not a kernel allowlist. Its `core.units.code` enum admits `g`, `mg`, `kg`, `1`, `%`, `[ppth]`, `{each}`, `s`, `min`, `h`, `d` and `wk`. Each unit has a value type containing its fixed code, dimension and positive exact rational factor to gram, unity, each or second. Count remains distinct from dimensionless values. The pack provenance pins UCUM essence 2.2 and its source digest; currencies remain ISO 4217 money. Full UCUM and QUDT admission are deferred to #351. Selecting another unit descriptor refuses; conversion requires an explicit declared expression and rounding site. Catalog membership alone does not establish arithmetic compatibility.

## Evaluation and enforcement

Unit-bearing entity declarations pin the vocabulary with one required exact-version `entity` dependency on `core.units`. Artifact-set admission resolves that visible catalog for object, interface, extension and shared value-type declarations, including nested record members. An absent, ambiguous or incompatible pin, unknown code or unsupported system refuses as `core.values.unit_mismatch`. The initial vocabulary supports UCUM only; admission never infers aliases or converts values.

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
- **VAL-UNIT-CATALOG:** Given the pinned bounded unit catalog, known codes and their exact factor records admit; unknown codes, currencies and substituted dimensions or factors refuse. A gram descriptor cannot contain a kilogram code.
- **VAL-UNIT-PIN:** Given a decimal UnitRef, the exact visible vocabulary pin and code/system must resolve; missing/wrong/ambiguous pins and unknown/case-changed/system-mismatched units refuse, including inside a record.
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
