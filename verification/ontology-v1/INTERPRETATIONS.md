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
That subject set is global and fail-closed: an unavailable indexed type refuses
even outside the retained scope, because an incomplete catalog cannot show that
no subject was omitted. Retained membership narrows projection afterwards.
LAW-INTERFACE/EMPTY positives and unknown/unpinned/member refusals exercise
ordinary admission, including generated missing members with zero implementors.
Empty implementation sets establish no non-vacuity or proof evidence. Interface
deadlines/state selectors, undeclared transition contracts and app/pack roots
without retained instance resolution refuse in this intermediate profile.
Kani is infeasible for the dynamic effective catalog and expression compiler;
the generated refusal property checks their production admission boundary.

LAW-TRANSITIVE/DIAMOND emits one instance per law/subject with all origin paths;
the catalog selects subjects and the declaration graph provides provenance.
Aggregate path-work, member-copy and byte limits refuse incomplete output.
Generated orderings preserve instances; empty sets establish no non-vacuity.
App/pack resolution awaits shared instance IR. Kani is infeasible for this
allocated admission/expression/digest pipeline; correspondence is sampled.

## B3 retained authority construction

LAYER-AUTHORITY-1 constructs checked retained input only from ordered exact
source documents and their retained admission receipts. Each layer is rechecked
against already verified predecessors and `verify_admission_receipt` must bind
the same candidate, base and limits. Missing, reordered, duplicated or forged
receipts, edited envelopes and candidate-repaired bases refuse. The authority
identity retains ordered exact-source/receipt digests plus registry and limits;
Norn1 receipt identity never substitutes for exact source identity. This cut
does not permit flat multilayer challenges or grant proof/runtime authority.
LAYERS-CHAIN/FORGED/EXACT tests and generated receipt alterations check the
production constructor. Kani cannot execute its allocated JSON/admission/hash
pipeline; bounded cardinality and byte limits are checked before construction.

LAYER-CHALLENGE-1 consumes checked retained authority instead of flattening
its layers into independently admissible definitions. Approval and reconstruction
bind the retained source/receipt identity and require the candidate's exact base
to match. A changed authority requires new approval. The generator profile
changes to bind that authority in every challenge; flat callers retain their
existing admission refusals. Every writer still comes from the reconstructed
whole set. Worker-supplied goals/proof flags cannot supply authority or omit goals.
Named LAYERS/WRITER tests and generated candidate source spellings check challenge
identity. Kani remains infeasible for full allocated admission and induction
construction; solver evidence is unknown unless independently checked.

LAW-CONTAINED-PATH-1 retains exact containment paths as `instancePaths` of
lane E's existing `PackInstanceRef`, separately from definition-member paths.
The last reference identifies the governed concrete instance; identical law/type
bindings in different instances remain distinct. Import diamonds retain all
paths to the same instance. Non-instance scopes use an empty list. Strict
wire decoding requires the field and exact nested pins; no namespace or
producer evidence supplies membership. Named LAW-CONTAINED contract/refusal
tests and generated paths verify round trips. This contract cut leaves
retained app/pack resolution unsupported. Kani is inapplicable to this
allocated serde/schema contract; resolver verification follows separately.

LAW-CONTAINED-1 resolves stable pack/app IDs only in exact retained scope,
verifying pack pins/member kinds and traversing explicit imports/instances.
All import-diamond paths survive; unrelated packs confer no membership.
Unknown/duplicate roots, stale pins, missing members, implicit nesting, scope
mismatches, cycles or exhausted budgets refuse `core.laws.unresolved_scope`.
LAW-CONTAINED tests and generated orders check this structural resolver.
Kani cannot execute dynamic serde/semantic hashes; sampling grants no authority.

LAW-CONTAINED-INSTANCE-1 filters effective subjects by exact retained instance
membership and retains separate applications for different governed instances.
A diamond keeps all paths to the same instance. The original law and type
substitution stay unchanged. Interface declarations still validate when their
scoped implementation set is empty; emptiness never proves non-vacuity.
Missing scoped subjects refuse, and candidate definitions cannot repair the
retained membership graph. LAW-TIGHTEN accepts a new conjunctive identity;
LAW-WEAKEN refuses replacement, conditions on an inherited identity, stale
containment pins and removal of a required member. Kani is infeasible for
this full catalog/serde/admission pipeline; generated ordering tests exercise
the production projection. Ordinary source validation remains structural;
resolved application/protected-law admission requires this complete projection.

EDGE-GOALS-1 generates protected proposal obligations for every complete B1
interpretation: totality, domain coverage, identity/unit preservation, transition
simulation and preservation of the protected law set. The edge identity binds
exact source bytes, endpoints, every member interpretation and retained authority.
The proposal includes normal writer/induction goals, but records admission as
unsupported and every edge assessment as unknown. Only the known interpretation
unsupported finding is eligible for proposal generation; all other admission
errors refuse. Normal admission and protected executable challenges still refuse
interpretations. EDGE-NEW/OMIT/NOPROOF and generated omissions exercise this
boundary; review metadata and echoed inventories never discharge a goal.
Kani is infeasible for allocated admission/serde/induction generation.

LAYERS-EDGE-CHAIN-1 repeats EDGE-NEW/OMIT/NOPROOF and generated required-member
refusals against three independently admitted layers, including two extensions.
The flattened set cannot manufacture retained authority. Changed retained units
or strengthened inherited-law conditions refuse under the original approval.
Named independent node/edge/step exhaustion cases also refuse retained membership.
These are production receipt/resolver/challenge conformance checks, not proofs.
Kani remains infeasible for the allocated receipt/serde/hash pipeline.
App/pack law roots resolve during private approval construction. Exact retained
base and protected-law digests carry that checked fact into edge proposals; an
unresolved root cannot obtain approval or edit/reuse a previous approval.
The global type catalog may contain implementors from other apps. Scoped
projection selects its intersection with retained membership; an outside subtype
is excluded, while any unresolved explicitly declared member refuses.
