# Law-carrying interpretations

Specification for #213, Lane B; approved design:
https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558465085

## B1 contract and admission boundary

Source records (`norn.mapping/1`), term correspondences (link packs) and
law interpretations (`norn.interpretation/1`, role `law_interpretation`)
have distinct closed contracts. Evidence in one role never grants another.
An interpretation is an independently versioned mapping artifact, with exact
source and target layer artifact IDs, versions and Norn1 semantic digests.
Layers hold no singleton mapping field. Namespace prefixes grant no identity.

This first cut retains the typed endpoints through ArtifactPayload, strict
decoding, published schemas and the generated SDK. Every interpretation
refuses admission with `mapping.interpretation.unsupported` until member
conformance and preservation obligations are implemented. Record compilation
also refuses it. Endpoint syntax alone grants no retained-layer authority.

`typed_mapping_roots::map_roles_*` covers role separation, exact endpoint
fields and round-trip retention. `mapping::map_roles_*` covers admission and
record-compilation refusal using a synthetic capacity/credit interpretation.
These are structural tests, not proofs of preservation.

MAP-PINS additionally resolves each endpoint uniquely in retained base artifacts
as a business pack, with a matching Norn1 digest and exact semantic version.
Exactly one required dependency must name that same pack/version/kind.
Missing, stale, ambiguous, same-version edited or candidate-only endpoints
refuse; an optional dependency cannot confer authority. MAP-MULTI checks two
independently identified edges without a singleton layer field. Successful pin
checks still leave admission unsupported pending member and law conformance.

MAP-MEMBER-CONTRACT-1 retains an explicit `members` list through strict roots,
schemas and the SDK. Each source/target uses the existing exact `MemberRef`,
including lifecycle property/transition IDs and independent event link roles.
Optional transforms use the existing expression language; declaring one is not
proof or permission. Required coverage, relation targets/cardinality and
dimension/unit/scale come from pinned definitions, never candidate assertions.
The positive EVENT-MULTI contract test retains two distinct role bindings;
missing member identities, unknown keys and null transforms refuse decoding.
A proptest checks generated member lists survive the payload-root round trip.
This contract cut adds no pure bounded engine function suitable for Kani;
serde/schema code operates on unbounded allocated inputs. Admission still
refuses all interpretations pending the conformance and obligation cuts.

MAP-TOTAL-1 derives the complete source member inventory from retained direct
composition-pack definitions, including effective object properties, links,
lifecycle states/transitions and direct interface requirements. Unique,
kind-compatible source/target bindings must cover that inventory exactly;
missing, duplicate, unresolved or transformed members refuse with
`core.composition.mapping_gap`. Non-injective target bindings also refuse.
Nested packs, extensions as members and inherited interface contracts remain
unsupported; admission still refuses semantic preservation. Named MAP-MEMBERS
tests check a complete contract and all generated omissions/orderings. Kani is
infeasible for this allocation-heavy resolver over serde trees, graph-derived
effective catalogs and dynamically sized sets; the generated omission property
checks the production admission boundary instead. Unit/relation compatibility
is the next conformance cut, not established by this inventory check.

MAP-UNITS checks resolved scalar kinds (including exact unit/currency path and
scale), nullability, relation target interpretations and both cardinalities.
Bindings must follow their declared type-owner interpretation. The initial
identity profile requires unchanged lifecycle IDs/machines; complex links,
unresolved interface value types and conversions refuse, never approximate.
The ordinary source-record EVENT-MULTI property test compiles/applies two role
maps and preserves the event key, distinct object keys and rule provenance.
This tests observed-data transport, not native mutation or law preservation.
Named unit/scale/role/arity negatives accompany it. Kani is infeasible for these
serde/catalog/compiled-expression paths with dynamically allocated trees;
generated runtime inputs check the production mapper instead. Preservation
obligations remain unsupported and interpretation admission still refuses.

## B2 effective law instance contract

LAW-INSTANCE-CONTRACT-1 is a separate closed projection, never a rewritten
LawIR: original law member/digest, concrete subject member/digest, attachment,
origin, all paths and explicit typed substitutions. SDK/schema round trips
retain every path of a diamond and binding; no caller-supplied proof flag is
accepted. A proptest preserves generated paths/substitutions. This contract
does not resolve instances or establish non-vacuity; B2's resolver follows.
`LawRoot::App` remains and the explicit pack root is added. Pack-scoped laws
and structured requirements refuse until retained membership is implemented.
The LAW-CONTAINED refusal tests enforce that intermediate boundary. Kani is
inapplicable to this serializer/schema contract over allocated member trees;
there is no new bounded pure algorithm. LAW-* admission semantics follow.

LAW-COMPLETE-1 checks an interface law against its required-member environment
even with no implementors, then checks every concrete subject selected by
`EffectiveCatalog::target_types`; the original LawIR remains unchanged.
LAW-INTERFACE/EMPTY positives and unknown/unpinned/member refusals exercise
ordinary admission, including generated missing members with zero implementors.
Empty implementation sets establish no non-vacuity or proof evidence. Interface
deadlines/state selectors, undeclared transition contracts and app/pack roots
without retained instance resolution refuse in this intermediate profile.
Kani is infeasible for the dynamic effective catalog and expression compiler;
the generated refusal property checks their production admission boundary.

Remaining B1 conformance scenarios: MAP-MEMBERS (source-derived
totality), MAP-UNITS (dimension/unit/scale), EVENT-MULTI (event identity and
distinct object links). Missing required members, unsupported transforms or
incompatible relations must refuse. Source observations confer no mutation
authority. B2 LAW-* and B3 LAYERS-*, WRITER-*, EDGE-* follow B1 conformance in
the approved design; this cut establishes none of those semantics.
