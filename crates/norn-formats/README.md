# norn-formats

Ring 1 format-family readers: source bytes → `SourceSchemaIR`, with a `LossReport` for every source element. Pure: no network, file system or clock. The crate boundary is `boundary.json`, enforced by `norn-skeleton-guard`.

- `xsd`: the XML Schema reader. Qualified names, particles (sequence/choice/all with exact `minOccurs`/`maxOccurs`), facets, `nillable` and annotations are kept in the source graph; DTDs and unbound QNames are refused; `import`/`include` locations are recorded as evidence and never fetched. A field's type candidate follows simple types and simple content to its XML Schema built-in; unbounded numerics stay `json` so nothing narrows to a fixed decimal scale.
- `pack`: the format-independent projection a pack's generated `Norn.sourceSchema` module carries: selected datasets, identifier-safe field names, native types, `parent_child` constraints for selected nested types, and `flatten` paths that read nested single values into a dataset.
- `contract`: the `SchemaImporter` trait and the loss ledger every reader fills before lowering.
- `type_tables`: deterministic RuleIR v2 decision tables evaluated through `norn-decisions`.

## Packs read their sources with `norn-format-read`

A standard or system is a pack, not Rust. Its `source.read.json` names the format, the `OntologySource` admission (licence, release, per-file SHA-256) and the module to generate. Originals are fetched by the person regenerating, never committed:

```sh
cargo run -p norn-formats --bin norn-format-read -- registry/packs/ubl/source.read.json --originals <dir>          # write source.ts
cargo run -p norn-formats --bin norn-format-read -- registry/packs/ubl/source.read.json --originals <dir> --check  # verify it
```

The reader refuses any byte that does not match a pin. The pack's `README.md` says where its originals come from.

## Boundary

- Ring: Ring 1 — native driver and host perimeter.
- Purpose: one reader per format family (XSD now; JSON Schema, OpenAPI, OWL and CSV later): source bytes → SourceSchemaIR, with a loss report that accounts for every source element, plus the format-independent projection a pack's generated `Norn.sourceSchema` module carries. Files only.
- **Formats, never standards.** No standard- or system-specific code lives here. UBL, ISO 20022, Peppol, a tax authority's schema or a vendor API is a declarative pack under `registry/packs/<name>/`: a `source.read.json` manifest pins its originals by hash and selects what the pack maps; the pack's mappings and laws are ordinary Norn definitions admitted by the checker. Adding a standard in an existing format family needs zero Rust.
- Allowed dependencies: `norn-formats ← ir, decisions + expr (type tables), quick-xml (XSD), sha2 (provenance pins); checker (dev only)`. The `norn-*` edges are enforced by `norn-skeleton-guard`; ring-1 third-party dependencies are governed by `cargo deny check`.
- Readers execute no source content, fetch nothing (external `schemaLocation`s are evidence, never fetched), perform no effects and grant no permissions. Output is a proposal: checker admission is a separate gate. The `norn-format-read` bin is the only file IO, and it reads only the files a manifest pins.
- Upstream originals are not vendored. A source enters only through an `OntologySource` admission (release pin, licence, redistribution policy, per-file SHA-256); unadmitted bytes fail closed.
- Every reader implements `contract::SchemaImporter` and passes the shared conformance harness (`tests/support`): deterministic canonical output, a `LossReport` inventoried before lowering with zero unexplained elements and only the closed `DropReason`s, and zero checker findings per layer. Type tables under `tables/` are RuleIR v2 decision tables; never re-sort their rows (first hit is semantic).
