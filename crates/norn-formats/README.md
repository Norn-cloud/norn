# norn-formats

Ring 1 format-family readers: source bytes → `SourceSchemaIR`, with a `LossReport` for every source element. Pure: no network, file system or clock. The crate boundary is `boundary.json`, enforced by `norn-skeleton-guard`.

- `xsd`: the XML Schema reader. Pins and sizes are checked before any byte is parsed, each file is parsed once, and hostile input is refused with a typed `format.xsd.invalid` diagnostic: files over 16 MiB (64 MiB per bundle), nesting deeper than 256, more than 250,000 elements, or namespace declarations beyond 64 per element, 128 in scope, 4,096 declaring elements or 1 MiB of declared prefixes and URIs per file, a namespace URI over 512 bytes, or more than 64 MiB of expanded names per file. Redeclarations count against the in-scope cap, so a name lookup walks at most 128 declarations. Scopes are parent-linked: an element stores only its own declarations and shares the rest by reference, and the output carries each node's own declarations only, so memory and output grow with the declared bytes, not with nodes times bindings. Qualified names, particles (sequence/choice/all with exact `minOccurs`/`maxOccurs`), facets, `nillable` and annotations are kept in the source graph; DTDs and unbound QNames are refused; `import`/`include` locations are recorded as evidence and never fetched. A field's type candidate follows simple types and simple content to its XML Schema built-in; unbounded numerics stay `json` so nothing narrows to a fixed decimal scale.
- `owl`: the OWL 2 reader. Its input is an RDF graph as canonical, sorted N-Triples (`ntriples`, a bounded line parser: no XML, DTD or entity expansion), which a pinned normaliser writes from the pack's hash-pinned originals; the reader refuses any graph whose SHA-256 is not the pinned one. Each class in an owned namespace becomes an interface (`fibo-be-le-lp:LegalEntity` → `fibo.be.le.lp.LegalEntity`): named superclasses become `extends`, domains and restrictions become links and properties with cardinality (`some` → required many, `exactly 1` → required one, `max 1` → optional one, `FunctionalProperty` → one), and datatype members take the type `tables/norn.formats.owl.datatypes.json` chooses. Inherited members only narrow; where two parents conflict, or a class's axiom cannot narrow what it inherits, the edge or axiom is not lowered, with the reason. Every declaration, axiom, annotation predicate, individual and leftover statement has one loss entry; nothing is dropped, and what is not lowered is `deferred` with a `format.owl.*` code naming why (disjointness, equivalence, class expression, cardinality bound, value restriction, property axiom, external reference, unplaced property, inheritance conflict, individual, annotation).
- `pack`: the format-independent projection of a pack's selected datasets (identifier-safe field names, native types, `parent_child` nesting, `flatten` paths that read nested single values, refusing any step that can occur more than once) and `check_contract`, which verifies that a pack's hand-written source contract claims no more than that projection.
- `contract`: the `SchemaImporter` trait and the loss ledger every reader fills before lowering.
- `type_tables`: deterministic RuleIR v2 decision tables evaluated through `norn-decisions`.

## Packs check their contracts with `norn-format-read`

A standard or system is a pack, not Rust, and a pack never commits schema payload derived from third-party originals. It commits:

- `source.read.json`: the format, the `OntologySource` admission (licence, release, per-file SHA-256) and what the pack projects;
- `source.ts`: a hand-written consumer contract, a `Norn.sourceSchema` of only the fields its mapping reads;
- its mapping and laws, as ordinary Norn definitions.

`norn-format-read` reads the hash-pinned originals and checks the compiled contract against their projection: every contract field must exist upstream with the same native type and the reader's type candidate, and may be required only where upstream requires it. Originals are fetched by whoever runs the check, never committed. `--emit <file>` writes the whole projection for review, and refuses any path inside the repository.

```sh
cargo run -p norn-formats --bin norn-format-read -- <pack>/source.read.json --originals <dir>
cargo run -p norn-formats --bin norn-format-read -- <pack>/source.read.json --originals <dir> --emit /tmp/projection.txt
```

The check is not in PR CI yet: it needs the originals, fetched by pin. It moves to the nightly lane as a follow-up. Offline, `tests/pack.rs` exercises the reader, the projection and the contract check on a synthetic invoice-shaped schema.

## OWL imports read a normalised graph

An OWL source is pinned by `registry/standards/imports/<atlas id>.read.json`: the originals of the `owl:imports` closure of its roots, file by file (`admission`), the licence each must declare, the owned namespaces and the SHA-256 of their normalised graph. Nothing is committed from the originals.

```sh
scripts/bootstrap-ontology-tools.sh   # the hash-locked rdflib
target/ontology-tools/venv/bin/python -I scripts/importers/owl_sources.py fetch registry/standards/imports/edmc-fibo-2026q3.read.json --output <dir>
cargo run -p norn-formats --bin norn-format-read -- registry/standards/imports/edmc-fibo-2026q3.read.json --originals <dir> --emit-dir <out>
```

`<out>` gets `artifacts.json`, `source_schema.json`, `loss.jsonl` (the axiom report, one entry per line) and `summary.json`. `owl_sources.py pin <manifest>` recomputes the closure and its pins for a new release.

## Boundary

- Ring: Ring 1 — native driver and host perimeter.
- Purpose: one reader per format family (XSD and OWL now; JSON Schema, OpenAPI and CSV later): source bytes → SourceSchemaIR, with a loss report that accounts for every source element, plus the format-independent projection a pack's generated `Norn.sourceSchema` module carries. Files only.
- **Formats, never standards.** No standard- or system-specific code lives here. UBL, ISO 20022, Peppol, a tax authority's schema or a vendor API is a declarative pack under `registry/packs/<name>/`: a `source.read.json` manifest pins its originals by hash and selects what the pack maps; the pack's mappings and laws are ordinary Norn definitions admitted by the checker. Adding a standard in an existing format family needs zero Rust.
- Allowed dependencies: `norn-formats ← ir, decisions + expr (type tables), quick-xml (XSD), sha2 (provenance pins); checker (dev only)`; the OWL reader adds no dependency. The `norn-*` edges are enforced by `norn-skeleton-guard`; ring-1 third-party dependencies are governed by `cargo deny check`.
- Readers execute no source content, fetch nothing (external `schemaLocation`s are evidence, never fetched), perform no effects and grant no permissions. Output is a proposal: checker admission is a separate gate. The `norn-format-read` bin is the only file IO: it reads only the files a manifest pins and never writes into the repository.
- Upstream originals are not vendored. A source enters only through an `OntologySource` admission (release pin, licence, redistribution policy, per-file SHA-256); unadmitted bytes fail closed.
- Every reader implements `contract::SchemaImporter` and passes the shared conformance harness (`tests/support`): deterministic canonical output, a `LossReport` inventoried before lowering with zero unexplained elements and only the closed `DropReason`s, and zero checker findings per layer. Type tables under `tables/` are RuleIR v2 decision tables; never re-sort their rows (first hit is semantic).
