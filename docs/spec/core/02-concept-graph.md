# Core 02: Versioned concept graph

Purpose [Owner]: one graph of things, happenings, containers and definitions, with explicit checked semantic roles. [Common conventions](README.md) apply.

## IR contract

Reuse `ObjectTypeDef {name, extends, implements, properties, links, owned_collections, identity, invariants, lifecycles}`, `InterfaceDef`, `ObjectTypeExtension`, `Property {id, property_type, constraints, computed}`, `Link {id, target, cardinality, acyclic, inverse, properties}`, `Lifecycle` and `CatalogArtifact` and its typed member projections.

| Target extension/projection | Field-level contract |
|---|---|
| Concept facets | `identity: member identity`, `role: thing/happening`, existing type definition, `actions: [Pin]`, `laws: [Pin]`, `policies: [Pin]`, `access: [Pin]`, `sources: [Pin]`; roles do not create separate object stores. |
| Defined concept | `id`, `version`, `base: type member identity`, `predicate: Expr<bool>`, `dependencies: [member identity]`, `bounds`; no writable membership flag or copied objects. |
| Happening instance | ordinary object ID, `definition: member identity`, typed participant/resource links, state properties, `activation_ids`, `event_at: UtcTimestamp` (source occurrence instant), `recorded_at: Revision` (local committed position, per 01); lifecycle authority is the ordinary transaction path. |
| Meta-ontology row | `definition_pin`, `kind`, `member_id`, `owner`, typed edges to members, source span, law origins; generated read-only from admitted IR. |

Props that look like references lower to `Link` or `OwnedChildCollection`; no second ID-valued property relation semantics. Required links and owned drafts are checked at final commit, allowing atomic creation of the entire graph. Item catalog metadata is separate from resource/holding identity.

## Declared role contract

GRAPH-ROLE-1: `InterfaceDef` and `ObjectTypeDef` accept optional `role: thing | happening`.
The effective role is declared or inherited through specialization. Disagreeing
parents refuse with `core.graph.role_conflict`; contradicting a parent refuses
with `core.graph.role_mismatch`. Omission with no inherited role defaults to `thing`.
That default is inherited by subtypes too. Names and lifecycle presence infer nothing.
Roles affect the catalog, presentation defaults and generated metrics, never runtime authority.
Core Agent, Resource and Claim declare `thing`; EconomicEvent, Process, Conversion,
Exchange, Commitment, Hold and LedgerEffect declare `happening`.

## Defined-concept wire contract

A pack declares a separate `defined_concept` artifact in the existing envelope/payload family.
Its strict payload contains `id`, `version`, `base: MemberRef`, `predicate: ZenExpression`,
`dependencies: [MemberRef]`, and `bounds: ResourceLimitProfile`. The base entity has an
exact required envelope dependency. Bounds reuse the resource profile's existing fields
and units, capped by the admitted profile; there is no separate classifier limits type.
Classifications keep base object IDs, never store writable membership, and trigger no effects.
Malformed payload objects refuse `defined_concept.validation.invalid_payload`;
non-object payloads fail envelope decoding with `ir.validation.invalid_envelope`.
Artifact-set admission requires an exact required pin for the base and every dependency,
a declared base type/interface, a pure Boolean predicate over its exact type closure,
and `norn_limits_v1` bounds at or below every admitted field. Missing/ambiguous members,
invalid predicates or excessive bounds refuse `core.graph.incomplete_classification`.
Per-artifact checking validates transport shape; cross-artifact admission checks semantics.
GRAPH-CLASS-1 uses an admitted pure classifier over complete, schema-checked base rows.
It returns original base IDs and refuses missing read paths, duplicate IDs, partial
snapshots, and aggregate record/byte/fuel exhaustion without publishing partial membership.
The cell adapter binds the active installed definition hashes and model to the classifier,
captures base-range/object/authorization dependencies, and refuses any concurrent commit.
Known stored nulls remain distinct from missing inputs; hidden rows retain invalidation reads.
No classifier stores membership or emits effects; admission creates no writable membership.
GRAPH-DAG-1 checks classification dependencies over exact artifact/version identities,
including retained declarations. A cycle containing a candidate refuses `core.graph.cycle`;
self references count as cycles and ambiguous identities never select a version.
The shared ordering primitive has a two-vertex bounded harness and generated-graph
correspondence against an independent transitive-closure oracle; these are bounded/sampled evidence.

## Evaluation and enforcement

Admission computes effective types from retained exact definitions, resolves interfaces and specialization, rejects cycles and conflicting diamond members. Reuse `EffectiveCatalog::target_types` for law subject resolution. Inherited constraints are conjoined, never replaced. A lifecycle transition selects its declared source state, evaluates all inherited/local guards and produces explicit effects for 04.

Derived/classified membership is evaluated at one authorized snapshot using 03. Unknown inputs cannot establish membership for access or law discharge; report incomplete classification. Predicate dependencies must be acyclic and cannot depend recursively on their own access filter. Membership changes emit no actions. An activated durable obligation survives later membership exit.

Definition catalog reads project admitted IR and exact source identity.
`norn-cell::catalog::CatalogSnapshot` serves the active set's `norn.catalog.*` virtual
relations through bounded `SliceQuery/SliceResult`, ordinary `norn.read` and kernel PDP
in the exact cell scope. Hosts add its resource descriptors only to Cedar projection.
Keys preserve exact MemberRef identities and edge endpoints. No analytical QuestionIR runs. Updating a definition requires ordinary admission + activation (06); catalog-row writes never activate executable meaning. Display labels can change without replacing semantic member IDs.

| Invariant | Enforcement layer | Stable target diagnostic |
|---|---|---|
| GRAPH-DAG-1: no interface/containment/classification cycles | Admission | `core.graph.cycle` |
| GRAPH-REFINE-1: specialization satisfies every inherited member contract | Admission | `core.graph.weakened_member` |
| GRAPH-LINK-1: required links, target types and declared cardinality hold on whole post-state | Construction + commit | `core.graph.invalid_link` |
| GRAPH-STATE-1: only declared transitions change lifecycle properties | Admission + commit | `core.graph.invalid_transition` |
| GRAPH-CLASS-1: classification is pure and uses one complete snapshot | Admission + evaluation | `core.graph.incomplete_classification` |
| GRAPH-META-1: catalog exactly projects admitted IR and grants no write authority | Construction + host | `core.graph.catalog_write` |

Evidence: structural graph/conformance checking, runtime link/state enforcement, sampled catalog/IR correspondence. Generated laws refer to effective members and explicit classification predicates; interface validation still runs with zero implementors. Empty sets do not establish non-vacuity.

Catalog facet edges use existing exact `MemberRef` identities for props, links,
states/transitions, admitted writers, law subjects, action access pins and source maps.
They confer no authority; catalog, law-row and transition-row definition submissions
refuse with `core.graph.catalog_write` at ordinary checker admission.

## Named acceptance scenarios

- **GRAPH-DIAMOND:** Given a diamond interface graph with one inherited property, when effective types resolve, then preserve one member identity and all origin paths; conflicting refinements refuse.
- **GRAPH-DRAFT:** Given a new happening requiring two resource links, when created with both in one transaction, then commit; missing one refuses all writes.
- **GRAPH-CLASSIFY:** Given `HighValueScrap := Scrap.fine_g > 100`, when a 101 g lot becomes 99 g through a permitted reconciliation, then membership changes with no implicit action; previously activated approval obligations remain.
- **GRAPH-STATE:** Given a posted immutable event, when an assignment or import attempts to reopen it, then refuse the canonical write; an external observation of such behavior remains quarantined evidence under 07.
- **GRAPH-CATALOG:** Given a retained law/transition, when requesting its catalog row, then ID, version and semantic fields match IR; writing that row cannot alter the transition.

## Main and gaps

Present: `crates/norn-ir/src/entity_v2/`, `definition_catalog.rs`, `crates/norn-checker/src/entity_model/`, `interfaces.rs`, `crates/norn-kernel/src/decision_commit.rs`; typed builders in `tools/norn-ts/src/builder/`. Main has effective interfaces, links, flat lifecycles and read-only catalog substrate. General defined concepts, full containment-law closure and coordinated graph migration are gaps. #227 is an open lane C application of these contracts, not new graph semantics; #316 adds structured requirements, not graph authority.

## Work breakdown (one coherent unit per PR)

1. Name graph-role/facet projections using existing entity/member IDs; GRAPH-CATALOG conformance, no new root kind (CI + core laws).
2. Add pure defined-concept schema/admission and cycle diagnostics; depends on 03 expression typing.
3. Evaluate authoritative classification and dependency capture; GRAPH-CLASSIFY plus incomplete-input refusals.
4. Complete graph-wide required-link/owned-draft validation in 04 commit; GRAPH-DRAFT/STATE negatives.
5. Connect effective member origins to 05 B2 law instantiation; GRAPH-DIAMOND, then lane C #227 core-aligned definitions.
