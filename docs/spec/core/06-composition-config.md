# Core 06: Versioned composition and configuration

Purpose [Owner]: packs compose reusable ontology, laws and behavior; apps configure stable instances. [Common conventions](README.md) apply.

## IR contract

Reuse `BusinessPackRoot`, `CompositionPackRoot {pack_id, pack_version, artifacts}`, `PackArtifactRef`, `OntologyLinkRoot`, `MappingIr`, `SettingRoot`, `CustomizationRoot`, `DefinitionSource`, admission receipts and existing migration plan types.

| Target extension | Required fields |
|---|---|
| Pack instance | `instance_id`, `scope`, exact pack `Pin`, `imports: alias→instance+Pin`, `exports: [member identity]`, `type_unit_bindings`, runtime parameter bindings; no implicit global instance. |
| Law interpretation | `id: Pin`, `role: law_interpretation`, pinned source/target ontology endpoints, required object/interface/property/relation/unit/transition interpretations; required domain comes from source contract. |
| Member interpretation | source/target typed member IDs, relation role/target/cardinality, dimension/unit/scale, lifecycle property/transition IDs, checked transform expression, explicit gap where permitted. Required semantics cannot be a gap. |
| Retained input | ordered source layers, exact source bytes/digests separately from Norn1 receipt identity, predecessor pins, registry/limits profile and verified receipts; private checked construction only. |
| Parameter | stable member ID, `PropertyType`, scope and explicit precedence, half-open effective interval, exact value, approval/review, superseded version, recorded revision. |
| Migration plan | old/new exact generation, data revision, affected instances/member mappings/state transitions, timers/commands/parameter bindings, law obligations, projection rebuild plan, activation boundary and restatement plan if backdated. |

Source-record mappings and term correspondence stay distinct from law interpretation. Multiple interpretation artifacts from one layer are valid; do not add a singleton mapping slot or infer endpoints from namespaces. Extend `ArtifactPayload` strict roots, schemas and SDK, not only `mapping.rs`.

### Shared structural instance contract (PACK-PIN-1)

`CompositionPackRoot.instances` declares the single shared instance model:
`PackInstanceRef {instanceId, pack: InterpretationEndpoint}` and
`PackInstance {instanceId, scope: ScopeRef, pack: InterpretationEndpoint, imports: BTreeMap<alias, PackInstanceRef>, instances: Vec<PackInstanceRef>}`.
`InterpretationEndpoint {artifactId, version, sha256}` pins the exact Norn1 semantic envelope; law interpretations reuse that same type.
IDs are opaque and unique within each scope of the retained candidate/base graph. Renaming labels or upgrading a pack does not change instance identity.
Imports and contained-instance references resolve in the declaring instance's exact scope, with matching pack pins; there is no implicit global instance or namespace lookup.
Duplicate IDs refuse with `core.composition.duplicate_instance`; missing, inaccessible or mismatched pins refuse with `core.composition.unresolved_pin`; import/containment cycles refuse with `core.composition.instance_cycle`.
This cut checks structure only. Generic type/unit substitution, B2 law instantiation and B3 retained authority remain unqualified and grant no executable authority here.

## Semantics

1. Resolve an acyclic exact dependency graph and explicit instances. Type/unit substitutions instantiate inherited laws and member contracts before checking executable writers. Two instances of one generic ledger may use USD and fine grams; currency-specific laws never apply to grams by accident.
2. B1 checks source-required totality, member kind, relation role/target/cardinality, identity and unit/scale compatibility. B3 reconstructs each admitted layer using `verify_admission_receipt` against already checked predecessors. Reject forged/reordered/missing receipts, same-version byte changes and lossy numbers; flat multilayer input still refuses without retained authority.
3. Generate mapping totality/domain coverage, identity/unit preservation, transition simulation and law-preservation goals, plus per-writer refinement over all layers. Neither a reviewed mapping nor a receipt proves those goals. Bind complete paths and source/member interpretations into the challenge.
4. Resolve runtime settings by explicitly ordered scope precedence, then effective interval at the action's declared effective date, within its recorded-time snapshot. Equal-precedence overlapping candidates refuse. When a higher-precedence scope has no active candidate, resolution falls through to the next scope. Only when no scope has an active candidate, require an explicit default or refuse with `core.composition.parameter_missing`; never leave the setting unbound. Replace binary64 economic settings with common exact types; persist selected version in the commit.
5. Overlays are scoped conjunctions on pinned definitions, independent of declaration order; contradictory guards or impossible progress refuse within the supported analysis profile. New behavior has a new action identity. Restrictiveness is logical implication: effective guard = `base ∧ overlay`, monotone by construction; no numeric order is inferred or declared. The initial analysis profile is specified below.
6. Plan migration against pinned definitions/data. Keep in-flight instances/pending effects on their original version unless explicitly transformed; preflight keys, links, states, settings, authority and laws. Atomically activate within a cell with conflict checks and projection generation switch. Larger migrations expose governed resumable availability; rollback is a new validated migration. Never reinterpret old event bytes silently.

### Parameter slice contract

`SliceKey::Parameter {from, settingKey, effectiveAt, precedence: Vec<ScopeRef>, defaultFrom?}` projects one numeric setting from an earlier complete authorized link-list slice. `effectiveAt` names the action's input timestamp; `defaultFrom`, when present, names an earlier single-object setting slice. Scope ordering is explicit and closed; no aliases or globals are inferred. Candidate data and link membership stay in the transaction read set.

The initial native numeric-setting fact profile uses `setting_key`, exact numeric `value`, `effective_from`, nullable `effective_to`, `review_state`, and five present nullable scope fields (`org_id`, `tenant_id`, `workspace_id`, `environment_id`, `business_unit_id`) projected to `ScopeRef`. It projects to the existing `SettingRoot`; source facts are changed only by ordinary governed actions. The binding contract alone refuses execution until the runtime projection and receipt pin are qualified; rule bindings remain unsupported.

### Initial overlay analysis profile

Per the [coordinator ruling, point 2](https://github.com/Norn-cloud/norn/issues/344#issuecomment-6559163104):

- An overlay may only conjoin (a) quantifier-free guards over the target's typed fields, admitted parameters and action inputs using exact-decimal linear arithmetic, enum and identity equality, and booleans; or (b) additional obligations or laws on the same target.
- The checker generates a satisfiability obligation for `base ∧ overlay` and dispatches it to the existing SMT path. Unsatisfiable on every admissible input refuses with `core.composition.overlay_conflict`.
- For each statechart transition an `eventually` / `until` law requires to be enabled, check that conjunction for satisfiability within the reachable-state invariant. Unsatisfiable refuses with the same code and `reason: impossible_progress`.
- Satisfiability is a necessary refusal check, not a progress proof. Passing leaves progress `unknown` unless a generated liveness obligation is discharged.
- Anything outside the fragment refuses with `core.composition.overlay_conflict` and `reason: unsupported_predicate`, including function or `perform` calls, quantifiers, non-linear arithmetic and opaque constraints. Never assume compatibility.

| Invariant / layer | Stable target diagnostic |
|---|---|
| PACK-PIN-1: exact resolved instances/versions; admission | `core.composition.unresolved_pin` |
| MAP-TOTAL-1: required typed semantics covered; admission | `core.composition.mapping_gap` |
| LAYER-AUTHORITY-1: retained layers verified in order; admission | `core.composition.invalid_retained_input` |
| OVERLAY-MONOTONE-1: no law/domain replacement; admission | `core.composition.overlay_conflict` |
| PARAM-EXACT-1: unique effective scoped exact value; admission/commit | `core.composition.parameter_ambiguous` |
| PARAM-BOUND-1: missing candidates require explicit default or refusal; admission/commit | `core.composition.parameter_missing` |
| MIGRATE-PIN-1: explicit migration for in-flight work; admission/commit | `core.composition.migration_required` |

Evidence: structural identity/admission, runtime activation/parameter pins, generated interpretation/refinement goals; semantic preservation stays unknown until checked. Published releases pin IR, dependencies, parameter schema, migrations, Function/tool digests and evidence.

## Named acceptance scenarios

- **MAP-PINS/ROLES/MULTI/MEMBERS/UNITS:** Given two mappings from one layer, when checked, then retain both; stale pins, role swaps, omitted required transitions or unit changes refuse.
- **LAYERS-CHAIN/FORGED/EXACT:** Given three separately admitted layers, when retained authority verifies, then extensions resolve; tampered/reordered predecessors or same-version source changes refuse.
- **WRITER-NEW/EDGE-NEW/EDGE-OMIT/EDGE-NOPROOF:** Given a law challenge, when a writer/mapping changes, then regenerate goals; removed/substituted goals refuse; review evidence alone leaves proof unknown.
- **PARAM-TAX:** Given USD 100 net, 10% before July 1 and 12% thereafter, when posting June 30 and July 1, then tax is 10 and 12 with distinct pinned versions; later setting edits leave both receipts unchanged.
- **PARAM-NO-MATCH:** Given no candidate at the declared scope/effective date/recorded snapshot, when resolving, then select only an explicit default and persist its version; without one refuse `core.composition.parameter_missing` with no writes.
- **MIGRATE-FLIGHT:** Given a process started under v1, when v2 activates, then it continues under v1; an explicit supported migration changes its pin atomically or refuses without changes.

## Main and gaps

Present: `crates/norn-ir/src/{business_pack,mapping,setting,customization}.rs`, `crates/norn-checker/src/obligations/`, `crates/norn-cell/src/migration.rs`. Settings and live-data migration are real substrate. `SettingValue::Number(f64)` is not an exact economic parameter. B1–B3 remain the [design on #213](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558465085); generic pack instances, protected law transport and complete in-flight migration are gaps.

## Work breakdown (each PR ≤400 lines)

1. B1 role/endpoints in existing mapping roots, then typed member interpretations, then MAP-* negatives/schema/SDK projection cuts.
2. Coordinate 05 B2 law instances; B3 checked retained-layer construction and receipt revalidation, then full writer/edge challenge reconstruction, then LAYERS/EDGE tests.
3. Add stable generic instance/type-unit binding admission, then explicit import/export/alias conformance.
4. Upgrade governed parameter exact typing and scope/interval resolution, then PARAM-TAX commit history tests with 01/04.
5. Add migration plan coverage for states/timers/commands, then atomic activation, then MIGRATE-FLIGHT and rollback/restatement tests with 08.
