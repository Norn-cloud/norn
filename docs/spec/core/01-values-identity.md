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
| Temporal context | resolved `effective_at: UtcTimestamp`, `recorded_at: Revision`, `calendar: Pin`, `timezone_version`, `dst_policy?: reject/earlier/later` (required for local-wall-time construction); intervals are half-open `[from,to)`. |
| Content reference | existing `BlobRef {digest, version?, size_bytes}` plus declared retention/access policy; bytes must match digest before use. |

Definition identity is exact artifact/version/digest plus stable member ID. Renaming a label changes presentation only. A same-version edit changes digest and invalidates admission/evidence; it cannot replace an immutable published release.

The Decimal wire declaration accepts optional `rounding` using the shared `Rounding` modes (`refuse`, `toward_zero`, `half_even`, `half_away_from_zero`); omission means exact/refuse. The declared scale is the sole result-scale authority at a rounding site. The Norn-owned rescale adapter delegates to `ScaledArithmetic` at scales 0–18; the existing exact storage/transport range remains 0–28. The bounded exact-site binding activates these modes through the shared expression VM and effective-property store canonicalizer; unsupported arithmetic sites refuse as `core.values.inexact`.

The declaration adapter is checked against generated signed-integer references. A production-core Kani harness covers all u32 scale derivations and fixed full-width coefficient/decimal overflow witnesses; this scope does not cover IR decoding, adapter parsing or the remaining expression/store binding.

Within the single `ScaledArithmetic`, exact `+`/`−` derive the maximum operand scale, `×` derives their sum and comparisons align scales exactly, refusing a derived scale above 18 or coefficient overflow; division, rescale and `round` require an explicit result scale and rounding at a destination Decimal or `round(x, n, mode)` site.

The explicit exact-expression runtime uses this same VM/core, retains intermediate scales and meters refusal paths; division requires a destination with its effective mode, and final Decimal quantization happens only at the destination. Plain Number and OrderedFold keep their existing execution paths. Declared scalar and constructed record/list destinations lower to the same explicit rescale operation; unsupported operations refuse.

A destination Decimal with omitted rounding supplies effective Refuse just like explicit Refuse: exact quotients are accepted, nonzero remainders refuse, and their type-environment fingerprints canonicalize omission to Refuse; predicates without a destination require explicit round(value, scale, mode) around division.

VAL-ROUND keeps `norn.expr.env/1` bytes and pins unchanged by omitting canonical Refuse, adding a rounding field only for newly admitted non-default policies that strict older decoders reject; a version bump is required only when existing bytes or their meaning change.

Temporal construction accepts either local wall time with its pinned calendar/timezone version and explicit `dst_policy` (reject, earlier or later), or an already-resolved UTC `effective_at`. Local construction resolves once to the UTC instant; ambiguous local time without a policy refuses. Persist that resolved instant with the temporal pins. Replay reuses the resolved UTC instant rather than resolving the wall time again under current timezone rules.

The initial unit vocabulary is the ordinary pinned `core.units` pack, not a kernel allowlist. Its `core.units.code` enum admits `g`, `mg`, `kg`, `1`, `%`, `[ppth]`, `{each}`, `s`, `min`, `h`, `d` and `wk`. Each unit has a value type containing its fixed code, dimension and positive exact rational factor to gram, unity, each or second. Count remains distinct from dimensionless values. The pack provenance pins UCUM essence 2.2 and its source digest; currencies remain ISO 4217 money. Full UCUM and QUDT admission are deferred to #351. Selecting another unit descriptor refuses; conversion requires an explicit declared expression and rounding site. Catalog membership alone does not establish arithmetic compatibility.

## Evaluation and enforcement

Unit-bearing entity declarations pin the vocabulary with one required exact-version `entity` dependency on `core.units`. Artifact-set admission resolves that visible catalog for object, interface, extension and shared value-type declarations, including nested record members. An absent, ambiguous or incompatible pin, unknown code or unsupported system refuses as `core.values.unit_mismatch`. The initial vocabulary supports UCUM only; admission never infers aliases or converts values.

1. Construction decodes exact numeric tokens without a float round trip; rejects non-finite, range/scale overflow and unknown units. Canonical equality ignores representational trailing zeroes but preserves declared type/unit and original source evidence bytes separately.
2. Admission resolves key components, normalization order, type/unit compatibility and uniqueness roots. Text admission follows existing NFC-only semantics; it must not silently normalize signed source bytes. For the CP2 inbox bridge, NFC canonicalizes storage lookups only: a raw identity spelling that differs from NFC produces a durable “identity normalized; possible collision” finding and refuses convergence within that bucket until the separate opaque/signed source-byte retention and byte-exact comparison bridge lands; opaque IDs are bytes, never NFC equivalence classes. Missing and explicit null differ; nullable is declared.
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

ID-STABLE-1 distinguishes authored mutation from observation using the existing commit provenance. An authored Create of an identity with a committed deletion refuses `core.identity.reuse` atomically: "this identity was deleted; restore it explicitly or use a new identity". Observed CDC/source-bound reappearance preserves the same source-bound identity, lifts its tombstone and advances its version monotonically. Replay preserves both outcomes and their retained identity/version evidence. Explicit restoration requires a later governed action carrying retained identity evidence (#748); until that action exists, ordinary authoring cannot restore a deleted identity.

The authored boundary is shared kernel `check_commit` with its existing sealed per-write observation authority, plus canonical store `FactChange::Create`. Raw actorless `CellStore::commit(CommitBatch)` and `Put` remain trusted embedding primitives, not authored contracts; authored adapters use the checked boundary. Event JSON cannot establish observation authority, and no `FactMutation` flag is introduced. An authenticated provenance/replay binding for raw paths is the separate C2 follow-up #763.

Evidence expected: structural codec/schema checks; runtime-enforced uniqueness/reference constraints; sampled transport/replay correspondence; generated arithmetic side conditions over the same exact-number semantics. Do not infer universal identity correctness from a hash function.

The ledger's allocation-free decimal core (`quantity_ledger/decimal.rs`) preserves exact coefficient/scale value while removing trailing zeros, and refuses power-of-ten overflow. Proptests cover arbitrary `i128` coefficients and `u8` scales. The standalone `verification/canonical-values-v1/decimal_kani.rs` harness imports those production functions directly: normalization uses signed 8-bit coefficients and scales 0–2; multiplier checks use exponents 0–6 plus the overflow boundary 39. These bounded implementation checks do not prove the full ledger, runtime Decimal parser, rounding semantics or temporal evaluator. Run each harness on Boat with a 300-second bound; an absent verdict remains unknown.

## Named acceptance scenarios

- **VAL-ROUNDTRIP:** Given `9007199254740993` and `0.10` exact inputs, when sent through each governed surface and replayed, then numeric identity is unchanged; float coercion refuses.
- **VAL-ROUND:** Given scale 2 with midpoint-away rounding, when evaluating `1.005` and `-1.005`, then results are `1.01` and `-1.01`; a zero divisor refuses without effects.
- **VAL-UNIT-CATALOG:** Given the pinned bounded unit catalog, known codes and their exact factor records admit; unknown codes, currencies and substituted dimensions or factors refuse. A gram descriptor cannot contain a kilogram code.
- **VAL-UNIT-PIN:** Given a decimal UnitRef, the exact visible vocabulary pin and code/system must resolve; missing/wrong/ambiguous pins and unknown/case-changed/system-mismatched units refuse, including inside a record.
- **ID-COLLISION:** Given two subtypes sharing a key root, when concurrent creates use the same normalized key, then exactly one commits; another tenant may use its own same key.
- **ID-RENAME:** Given a posted event linked to object A, when A's label/business key changes and is later tombstoned, then historical links still resolve A; reusing A's identity refuses.
- **ID-AUTHORED-RECREATE:** Given a committed deletion, authored creation of the same identity refuses `core.identity.reuse` with the actionable message above and leaves facts, events, effects and receipts unchanged.
- **ID-OBSERVED-REAPPEAR:** Given a deleted source-bound identity, an authorized observed reappearance retains that identity, lifts the tombstone and advances its high-water version.
- **ID-TOMBSTONE-REPLAY:** Replaying the retained prefix preserves the deleted/reappeared identities and monotonic versions; replaying a refused authored request does not publish a mutation.
- **TIME-DST:** Given an ambiguous local timestamp without a pinned disambiguation policy, when admitted, then refuse; an explicit earlier/later choice resolves to a retained UTC instant and replays that same instant even after timezone rules change. An already-resolved UTC construction needs no DST choice.
- **BLOB-MISSING:** Given an archived receipt with unavailable bytes, when requested for recomputation, then expose unavailable evidence and do not synthesize a result.

## Main and gaps

At the README baseline: `crates/norn-ir/src/entity_v2/{object,property}.rs`, `runtime.rs`, `function.rs`; `crates/norn-canonical/src/{lib,number}.rs`; `crates/norn-checker/src/property_codec.rs`; `crates/norn-cell/tests/exact_values.rs`; `crates/norn-kernel/src/observations.rs` are substrate. General stable alias/merge/split history, bitemporal keys and calendar policies are not established. WellDB exact-decimal work #103 is open; its numeric limits must match the shared profile before engine equivalence is claimed.

## Work breakdown (dependency order; each PR ≤400 changed lines)

1. Freeze shared exact value/unit profile and add VAL-ROUNDTRIP/VAL-ROUND conformance; reuse existing codecs (CI + core laws).
2. Add scoped stable identity/key-binding contract with ID-COLLISION admission tests; unsupported alias mutations refuse.
3. Enforce key/alias history and reference fences at commit; add concurrent collision/tombstone tests.
4. Add temporal/calendar/content-reference pins and TIME-DST/BLOB-MISSING tests; align 06/08 temporal lookup.
5. Add governed merge/split plans after 04/06 migration checks; prove old-reference preservation with ID-RENAME extensions.

The shared type checker marks actual Decimal reads and declared destinations; typed consumer admission admits derived exact predicates and, within the exact profile, rejects destination-free division or unsupported inexact functions as `core.values.inexact`, and the typed consumer API binds that admission and the shared exact VM to the same environment and source. Constructed Decimal record/list destinations bind each numeric leaf separately; kernel invariants and action/owned-row expressions use the same typed binding, and all object/link/default/migration writers share effective-property materialization.

VAL-ENV binds `currency_source` in new `norn.expr.env/2` admissions; retained receipts and pins select explicit historical `/1` recomputation, while relabelling `/1` hashes as `/2` refuses (migration: re-admit for `/2`, never rewrite an old pin).

VAL-UNIT admits same-code addition, subtraction and comparison, scalar multiplication and same-code quantity division to a scalar; a numeric literal inherits the sole quantity unit with an explanation diagnostic naming that unit, while declared unitless operands, mixed codes, compound units and inverse units refuse until #351.

VAL-RATE is a Decimal refinement with mandatory `currencySource` and `perUnit`, no quantity `unit`, and one declared scale/rounding: Rate<C,U> × Quantity<U> → Money<C>, Money<C> ÷ Quantity<U> → Rate<C,U>, and same-type comparison/addition/subtraction are the bounded contract; all other compound operations refuse, multiplication stores at the destination Money scale/rounding (Refuse by default), and env `/2` fingerprints both bindings while `/1` refuses rates.

VAL-ROUND explicit `round(value, scale, mode)` enters its declared site before evaluating the operand and lowers to the same ScaledArithmetic rescale; a two-argument round at an exact destination inherits its effective mode, while a predicate has no implicit mode, and nested sites do not authorize adjacent division.

VAL-ROUND storage materializes explicit non-Refuse Decimal modes through the shared rescale adapter before effective bounds, including record members; the same effective-property canonicalizer serves object/link writes, defaults and migration adapters. Invalid observed source values remain evidence and fail strict value checking. Source evidence outside the strict value grammar remains unchanged and refuses authoritative promotion.

VAL-EXACT-1 kernel object invariants bind the catalog-derived environment and Boolean destination to typed admission and evaluation before commit, so exact products are admitted and unqualified division refuses.


VAL-EXACT-1 closed numeric schemas reuse exactly `{type: "number", nornType: <PropertyType::Decimal>}`; typed authoring emits the refinement, env `/2` pins its existing fields, unknown fields refuse, and a bare number retains legacy behavior while refusing as an operand of a declared exact site.

VAL-RATE's initial graph binding admits one direct expression node between input/output; typed decision callbacks with exact schemas lower to that shape, while sub-decisions, table/loop transforms and other exact graph forms refuse until their bindings exist. Currency sources must resolve to a fixed singleton field; the runtime schema checks its actual value. Rate aggregation refuses until its denominator-aware contract is implemented.

- **VAL-RATE-PRICE:** Given fixed USD, a 15 USD/g rate and 20 g, typed admission/evaluation produces USD 300 at the destination Money scale; a nonzero remainder under Refuse refuses.
- **VAL-RATE-MISMATCH:** Given USD/g, combining mg, an EUR destination, Rate × Rate or an erased numeric operand refuses; quantity-versus-literal comparison records `core.values.literal_unit` naming g.

ID-UNIQUE-1's bounded native profile reuses `EffectiveKey` and its declared normalization order. The shared SQLite transaction checks complete normalized tuples for every admitted declaring root/key across stored scope/world domains before publishing events, effects or a receipt; reopen and definition activation validate all such domains. Original property bytes and opaque IDs are preserved. This profile supports text/enum/integer/boolean tuple components, at most 16 normalization steps per component, and a cumulative scan of 4,096 keys/4,096 rows and 8 MiB of encoded-plus-normalized key and identity data. Unsupported normalized components or incomplete scans refuse. Entity-resolution policies, alias history and identity restoration are separate contracts; this cut does not activate them or prove their safety.

- **ID-NORMALIZED-ATOMIC:** sibling subtypes with raw-distinct but canonically equal keys cannot both commit; a refusal leaves the entire SQLite dump unchanged. Original source spellings remain stored.
- **ID-NORMALIZED-DOMAINS:** the same normalized tuple may exist in different stored scopes/worlds; a business-key rename preserves the opaque object ID.
- **ID-NORMALIZED-COMPOSITE:** typed tuple boundaries prevent concatenation collisions; unsupported normalized components and exhausted scan/step bounds refuse.

This cut's evidence is native runtime conformance and generated ASCII input sampling. The unchanged pure normalization helper is reused directly; the new transaction scan depends on SQL and is not a pure bounded Kani target. These tests do not establish alias history, tombstone restoration, arbitrary Unicode normalization correctness or general identity proofs.

The normalized native profile checks candidate activation with reads only as well as inside the install transaction. A normalized key over a newly added component on an occupied table refuses until backfill-aware identity admission exists; an empty new type is permitted. Candidate checks cover all worlds, including worlds other than the install request's world.

Raw-byte duplicates may refuse earlier through the existing physical unique index and retain its existing transport error; the new canonical collision code covers raw-distinct normalized duplicates. Both paths preserve atomicity. This cut does not qualify a new SQL-constraint error taxonomy.

**ID-NORMALIZED-REOPEN-BOUND:** every commit charges the same complete key/row/byte budget as reopen, including unrelated roots; a commit that would make reopening exceed the profile refuses atomically. Stored byte preflight uses `octet_length` before fetching values.

**ID-NORMALIZED-DISCLOSURE:** an actor-facing action exposes collision details only after the existing collection and object read policies authorize the committed conflicting subjects; unreadable, absent unknown or over-bound subjects use the fixed visibility refusal. Internal lookup subjects never appear in the safe refusal.


**ID-NORMALIZED-RETAINED:** decoding an inactive retained definition preserves its original metadata and hashes without applying a newly introduced admission profile. Verified export reconstruction runs only in a fresh, unpublished engine, binds each retained batch to its original model and receipt, and checks the complete final model/state against the current normalized-key profile before returning or publishing a writable cell. Current commits and candidate installations retain the same fail-closed profile; no serialized replay opt-out exists. A retained prefix may contain formerly accepted normalized duplicates if its final current state removes the collision.

- **ID-NORMALIZED-RETAINED-LOOKUP-PROVISION:** after upgrading away from a retained profile outside current admission bounds, historical definition lookup preserves its exact metadata/hash, and an identical provisioning retry reopens the current admitted model without adding receipts or changing live facts.
