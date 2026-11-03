# Norn: a governed active ontology

Status: target architecture, 2026-11-01. Labels identify authority: **[Owner]** explicit owner decisions; **[Astra]** architecture ruling/corrections; **[Default]** adopted design choices, subject to explicit revision. A directive's label applies to its entire paragraph or list item. Implementation status is separate from authority.

## One model

[Owner] Norn derives applications, API, MCP, CLI, UI, data and guarantees from typed concepts with state, actions, rules and permissions. It combines model-driven application development with readable laws, evidence-bearing layered ontologies, code-first authoring and governed agent participation. Reactive reads are the default; Convex-class general-app DX is a target, not a measured performance claim.

[Owner] Everything authors model is a concept: things (resources, agents), happenings (events, processes, conversions, exchanges, commitments), containers (packs/apps), and definitions in a thin generated meta-ontology. A workflow is a happening with states, not a separate workflow runtime. A concept bundles `is`, properties/derivations, links, states, actions, laws, policies, access and sources. Documentation/skill facets are deferred.

[Owner] This is not OOP: no hidden mutable state, methods or implementation inheritance. State is a deterministic fold of immutable events; behavior is checked data; specialisation only tightens contracts. Authoring callbacks compile away. Arbitrary code cannot become an inline rule, guard or fold.

## Layers and business physics

[Owner] The business constitution is authored in Norn: REA Agent, Resource, EconomicEvent, Commitment, Claim, Hold and LedgerEffect, plus Process, Conversion and Exchange. Economic Claim is a right to resources; it is distinct from a verification claim about a definition. Core laws cover quantity/value conservation, holds and availability, immutable history, commitment terminality and ownership distinct from custody. The core pack is not exempt from checking.

[Owner] Packs form a DAG: core → inventory/accounting/sales → jewelry → app. Jewelry composes domain packs, adds constructs such as a metal ledger and overlays scoped customisations. A layer may have multiple mappings to other ontologies. Chemistry is a future mapping target, not demo scope.

[Owner] Laws attach to their governed concept, happening, pack or app and propagate through containment and specialisation. Layers need not repeat inherited laws. A composing pack can own cross-module laws. Its bindings identify organisation, book, currency, valuation basis and account roles explicitly.

[Astra] Catalog Item and Holding are distinct. References use one typed-link model, including owned drafts. Source-record mappings, term correspondences and law-carrying interpretations have different authority. Only checked interpretations generate law-transport obligations; source evidence alone never proves preservation.

## Laws are theorems

[Owner] Definitions define the transition system and are its axioms. Laws state conjectures about it; checked proofs make them lemmas available to higher layers. Experts validate FRET-style scope, condition, component, timing and response plus deontic grade, rendered in English or Arabic from one semantic IR. They review statecharts and counterexample scenarios, not solver formulas. This is inspired by FRET, not a claim of full FRET compatibility.

[Owner] Enforcement order is construction, definition admission, then commit refusal. Construction applies only to constructors whose guarantees actually follow by construction. Admission rejects unsafe writers where the checker can establish that result. Runtime safety remains enforced without proof availability; proofs move refusals earlier, not remove safety.

[Astra] All writers enter one checked transaction calculus: creation, assignment, transitions, rules, imports, timers, external results and migrations. Tooling exposes direct effects and the transitive synchronous writer closure. Every synchronous effect necessary for a safety law commits together. Balancing later in a background trigger is invalid.

[Astra] Generate initialization, each-writer preservation, non-vacuity and boundary obligations from the actual declarations. Unknown expressions, opaque writers, unsupported timing and missing mappings remain visible obligations or refusals. Permissions are normative statements, not access grants; neither policy precedence nor solver success authorizes weakening a protected law.

[Owner] Generated statements live under `.norn/proofs`; maintained proof bodies live beside pack definitions and import generated statements. Lean checks proof bodies; agents may propose them but cannot certify them. Published packs carry immutable evidence. Track auto-proof rate with an explicit denominator of eligible generated goals; never count skipped or unsupported goals as solved.

[Owner] Route finite statecharts to Quint/TLA+, relational definitions/mappings to Datalog/chase, exact arithmetic side conditions to SMT, and unbounded invariants to Lean. [Astra] The demo starts with generated Quint/Apalache jobs plus executable arithmetic checks; more backends follow. Handwritten models remain test oracles until correspondence is demonstrated, never substitutes for generated application statements.

[Default] Evidence is a vector: structural, runtime-enforced, sampled correspondence, exhaustive-finite/bounded model checking, reference proof, implementation proof, or unknown/refuted. Bind statements, writers, dependencies, assumptions, limits and tool digests. A receipt proves admission identity, not a business theorem. Missing/stale/timeout evidence stays unknown.

## The guarantee

[Owner] Norn proves your application does exactly what your rules say, and shows you, in your own language, every surprising consequence of those rules before your business runs on them.

[Astra] Verification establishes that the running application corresponds to its declared specification: for the pinned release, its declared assumptions and identified properties, every execution covered by the verified semantics satisfies those properties at the displayed assurance level (proved, exhaustive-finite, bounded, witnessed, refuted or unknown). Validation, whether the specification represents reality, remains the domain expert's responsibility. Norn supports it with non-vacuity checks, counterexample scenarios, generated examples, mutation testing, reconciliation against observed data and faithful English/Arabic renderings, but none of these certifies business truth. Customers see what is proved, enforced, bounded, assumed or unknown. Unknowns are reported over all generated obligations. The assurance stack and its roadmap are tracked in [#718](https://github.com/Norn-cloud/norn/issues/718).

## Runtime, world and configuration

[Default] Multi-object synchronous transactions are atomic within a cell; cross-cell coordination uses explicit commitments and compensation. Placement must not silently turn an atomic contract into a saga. Complete read validation includes absent keys and collection membership. Retry pure contention only; stale approved intent returns a typed conflict.

[Owner] Typed, scoped, effective-dated parameters let apps configure installed packs. [Default] Type/unit parameters are compile-time bindings, separate from governed runtime settings. In-flight happenings remain pinned to their admitted version until validated migration. Historical interpretation and recorded setting versions never change silently.

[Owner] Functions are the sole code boundary, behind `perform`: typed WASM components or external workers, bounded and capability-scoped, returning recorded receipts. [Default] FunctionManifest is canonical; WIT and Effect contracts are checked projections. Start with qualified Rust/Python components, with workers for unsupported dependencies. Determinism, sandboxing and business correctness are separate claims.

[Owner] The sole host exception is Norn-owned, versioned built-in Functions distributed in the engine: they are never installable through authored or pack definition admission, and host execution carries no sandbox or isolation claim.

[Default] Commit and outbox are atomic; dispatch is at least once. An external consequence is unique only under its provider's deduplication/reconciliation contract. Unknown outcomes and failed compensation are durable states. Compensation appends new effects and never erases the original event.

## Data engineering, integrations, questions and agents

[Owner] Norn is also a data engineering tool, with data scientists and engineers a primary demo audience. The ontology is the semantic layer. Declared derived concepts, folds and questions generate pipelines across exact in-cell evaluation, incremental IVM and OLAP over DuckDB/Iceberg in WellDB. Results expose freshness, per-source cursors, bitemporal selection and number-to-source-document lineage. Imported violations produce findings and quarantine; governed writes refuse. Backfills/restatements preserve prior knowledge, and statistical outputs publish only through temporal/quality/authority guards. [Beat 6](../spec/acceptance/data-engineering.md) qualifies this contract.

[Owner] An integration pack contains a connector, the source system's ontology, mappings to shared concepts and governed write-back actions. Map each system once, then instantiate per scope; reduced integration cost is a hypothesis to measure, not a proven scaling result.

[Owner] Logs, agent traces, computer-use traces and ERP history are ordinary mapped data. An event can link multiple objects with distinct roles. Discovery and conformance are questions over events and the meta-ontology, not special IR kinds. Proposed definitions still pass ordinary admission and approval. Imported nonconformance remains evidence rather than being discarded or silently promoted to authoritative facts.

[Owner] Types generate measures where their additivity permits; business ratios are explicit. Boards are views. [Astra] Grain, fan-out, time and units govern aggregation; link cardinality alone cannot prevent double counting. Incomplete/stale projection results cannot discharge universal laws.

[Owner] Agents are authors and actors under the same checker and governed actions as people. [Astra] Authenticated principal, acting session and accountable delegator are distinct; delegation attenuates authority and revocation applies at commit/dispatch. Forks never perform external effects; promotion replays commands under current authority.

## Prior-art lineage

[Default] Adopt the model-driven vocabulary of SAP RAP, Salesforce and Palantir; the reactive DX expectation of Convex; Effect's typed success/error/requirements and Schema; XState's explicit statecharts; Alchemy's provider reconciliation; Morphir's logic-as-data; REA's economic relationships; FRET's structured requirements; and OCEL's multi-object event perspective. These are design influences, not claims of compatibility or borrowed implementation.

[Default] The functional Decider and transactional outbox inform the pure/impure boundary. Temporal and Restate inform recorded results and durable recovery, without introducing a second business authority. WIT informs portable Function contracts; Dafny/F*/Lean package practice informs pinned evidence; Aeneas informs implementation correspondence. OWL can be interchange, while checked closed-world reasoning controls admission.

## Baseline and delivery

[Astra] Public main at `1227c72b2637a489ea2c51e51a62354376c0a322` has typed IR, checker, kernel/cell, core interfaces, a definition catalog, settings, migration and structured safety requirements. It does not establish the complete thesis loop. See the [six-column status matrix](../spec/core/README.md) for bounded claims; older report statements that settings, catalog or migration are absent are superseded.

[Owner] MVP means these contracts plus [sell → scrap → melt](../spec/acceptance/jewelry-demo.md) and the [master ontology subset](../spec/acceptance/master-ontology-mvp.md). [Default] The [same-binary completeness gate](../spec/acceptance/completeness-gate.md) prevents application opcodes from masquerading as core completeness. Authoring syntax is deliberately owned by the separate authoring reference.

Public governing sources: [architecture ruling](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6556703019), [B1–B3 design](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558465085), [decision record](decisions/2026-11-01.md). This is a redacted synthesis; private identifiers and infrastructure are not part of the contract.
