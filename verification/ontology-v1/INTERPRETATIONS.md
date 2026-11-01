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

Remaining B1 conformance scenarios: MAP-PINS (retained identities and required
dependencies), MAP-MULTI (independent edges), MAP-MEMBERS (source-derived
totality), MAP-UNITS (dimension/unit/scale), EVENT-MULTI (event identity and
distinct object links). Missing required members, unsupported transforms or
incompatible relations must refuse. Source observations confer no mutation
authority. B2 LAW-* and B3 LAYERS-*, WRITER-*, EDGE-* follow B1 conformance in
the approved design; this cut establishes none of those semantics.
