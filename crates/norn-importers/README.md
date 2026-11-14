# norn-importers

Ring 1 readers for systems that declare their schema in their own format:
source bytes and their pins in, `SourceSchemaIR` and a loss report out, behind
the `norn-formats` `SchemaImporter` contract. Pure: no network, file system or
clock. Format-family readers (XSD) stay in `norn-formats`; a reader that knows
one system's own declarations lives here.

## Apache OFBiz entity-engine XML (`ofbiz`)

Reads `entitymodel` files (entities, `extend-entity`, `view-entity`),
`entitygroup` files and `entity-engine-xml` seed data.

- **Datasets**: each entity is a `table` (`package-name` is its namespace, its
  `description` or `title` its description); each view-entity is a `view`
  with lineage from its member entities.
- **Fields**: native type, a type candidate (`declared` for exact mappings,
  `inferred` for floats and Y/N indicators), required (primary-key members and
  `not-null="true"`), and the field description. Fields an `extend-entity`
  adds belong to the entity.
- **Constraints**: primary keys, unique indexes, and `one` / `one-nofk`
  relations as `foreign_key` / `logical_reference`, with OFBiz's key-map
  defaults (no key-map joins on the target's key; no `rel-field-name` means
  the same name).
- **Code sets**: seed rows of a referenced table become an
  `observed_candidate` code set of the referencing field.
- **Provenance extension** `norn.importers.ofbiz/0.1.0`: every file with its
  role and SHA-256, the licence and the notice's copyright line, inverse
  (`many`) relations, non-unique indexes, kept entity attributes, entity
  groups, unresolved references, code-set gaps, and status lifecycle and
  enumeration candidates.

**What OFBiz does not declare.** An entity's relation to `StatusItem` or
`Enumeration` does not say which status type or enumeration type the field
takes: OFBiz relations carry no type, and a relation title names a role. So
each such field is a typed gap in `codeSetGaps` that keeps the relation, its
title, its target and its target field, for a later mapping step or a mapping
proposer to resolve; a warning names the field. Status types become lifecycle
candidates of their own (their states, and their valid changes with
transition names and conditions), bound to no field. The Moqui convention of
grouping by relation title, which an earlier platform script applied to both
systems, is deliberately not used for OFBiz: there it would bind the wrong
values.

**Loss report.** Every element, attribute and seed row is inventoried before
lowering. What is recorded in `SourceSchemaIR` or its extension is
`format.source_schema_only` (lowering to `EntityIR` is a later mapping step);
unresolvable references are `format.reference_unresolved`; view aliases,
links and conditions, data rows and storage or security settings are
`format.source_deferred`; physical names and cache settings are dropped as
source bookkeeping, and the schema-location attribute as schema plumbing.
Anything the reader does not know stays unexplained and the import refuses.

**Pins and attribution.** Originals are never committed. `sources/` holds a
manifest that pins every file of a release by SHA-256; the reader refuses any
byte that does not match. OFBiz is Apache-2.0 ("Apache OFBiz, Copyright
2001-2026 The Apache Software Foundation"); the manifest pins its `LICENSE` and
`NOTICE` and the output carries both. Test fixtures are authored for these
tests.

```sh
cargo run -p norn-importers --bin norn-import-read -- ofbiz \
  crates/norn-importers/sources/ofbiz-9ee2530c.json --originals <checkout> [--emit <file>]
```

prints counts and fails on any error or unexplained element. `--emit` writes
the whole import, and refuses any path inside the repository.

## Boundary

- Ring: Ring 1 — native driver and host perimeter.
- Allowed dependencies: `norn-formats`, `norn-ir`, `quick-xml` (pinned like
  `norn-formats`), `serde`, `serde_json`; `norn-checker` in tests only. The
  machine-checked rule is `boundary.json`, enforced by `norn-skeleton-guard`.
- Readers execute nothing, fetch nothing, perform no effects and grant no
  permissions. DTDs are refused, inputs are size- and depth-bounded. The
  `norn-import-read` bin is the only file IO.
