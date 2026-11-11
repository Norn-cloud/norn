# Standards and systems atlas

A machine-readable map of the standards, ontologies and business systems an ERP
and business OS has to speak, and how each one reaches Norn's ontology. It is
the routing table for importers, packs and edge-case mining: what exists, who
publishes it, whether we may reuse it, which Norn concepts it maps to, which
issue owns the mapping, and how much it matters.

- `atlas/<domain>.json`: one file per primary domain. Each entry is one
  standard, ontology, code list, protocol, data model or system.
- `atlas.schema.json`: the entry contract.
- `COVERAGE.md`: generated domain × count × mapped % table, the next mappings,
  and concept demand. Never edit it by hand.

Validate and regenerate:

```sh
python3 scripts/standards_atlas.py --check   # CI (cheap phase)
python3 scripts/standards_atlas.py --write   # after editing an atlas file
python3 scripts/test_standards_atlas.py
```

## Fields that carry decisions

- **`licence`**: an SPDX expression (`LicenseRef-…` for custom terms,
  `NOASSERTION` when none was found), the terms read, and `status`. `verified`
  means someone read the terms on the publisher's primary source on the
  `checked` date; everything else is `unverified`. Public access never implies
  a right to redistribute.
- **`reuse`**: what Norn may do with it.
  - `code-reusable`: permissive terms; schemas, code or data may be copied or
    adapted into this Apache-2.0 repository with attribution (keep the
    upstream NOTICE). Needs a verified, asserted licence.
  - `vocabulary-only`: use its identifiers, codes and terms and map to them;
    do not redistribute or modify its artifacts.
  - `reference-only`: read and cite. Paid standards prose, proprietary
    systems, and copyleft ERP source (GPL/AGPL/LGPL: ERPNext, Odoo Community,
    Dolibarr, Tryton, iDempiere) are behaviour references only; no source is
    copied. ISO terms also forbid feeding ISO text to AI models.
- **`norn`**: `concepts` must name artifacts in `registry/core` or
  `registry/packs` (the validator checks); `irTargets` names the IR a mapping
  lowers into (for example `LawIR` for legal rules, `ValueType` for code
  lists); `proposedConcepts` names concepts the atlas needs that the registry
  does not have yet. `mapping` is `none`, `planned` (an open issue in this
  repository, `importerIssue`, owns it), `partial` or `done` (both cite
  repository paths in `mappingEvidence`).
- **`priority`**: `sapParity` ranks the entry for the roadmap to SAP-class
  breadth. `P0` is needed now, `P1` next, `P2` later; `watch` and `ignore`
  stay out of the mapping queue. Deployment-specific priorities are not
  recorded in this public atlas.
- **`origin`**: `catalogue-2026-10-21` entries come from the earlier standards
  research catalogue, with its licence and version checks; `atlas-2026-11-11`
  entries were researched for this atlas.

## Rules

- Research is not admission. An atlas entry admits no payload, mapping or
  Core change; importers pin, hash and admit sources under their own issues.
- External schemas never become Core authority. A concept enters Core only by
  the admission rule in Norn-cloud/norn#1128 (three standard families, two domains or
  money/identity, one real system).
- No business, customer or deployment data, and no links to private repositories; the schema accepts only issues in this repository.
- When a licence or version is re-read, update `licence.checked`; when it
  cannot be read, say why in `licence.basis` and keep it `unverified`.
