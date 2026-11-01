---
name: norn-authoring
description: Write or review Norn app definitions, packs, laws, mappings, and the authoring SDK against the Norn semantic contracts, distinguishing current tools from target syntax.
---

# Norn authoring

## Read before editing

- Read [the authoring reference](../../../docs/authoring/README.md), [the decision record](../../../docs/architecture/decisions/2026-11-01.md), and the relevant file among the eight [core specs](../../../docs/spec/core/README.md). The decision record governs older designs; report unresolved ambiguities to the lead.
- Select the named acceptance scenarios: [completeness](../../../docs/spec/acceptance/completeness-gate.md), [jewelry](../../../docs/spec/acceptance/jewelry-demo.md), or [master ontology MVP](../../../docs/spec/acceptance/master-ontology-mvp.md).
- Inspect [`tools/norn-ts`](../../../tools/norn-ts/README.md), its profile declarations and fixtures before writing executable examples. Mark unsupported constructs TARGET; never present target `@norn/dsl` helpers as current imports.

## Author or review

1. Identify the invariant, governing concept/happening/pack/app, units, scope and atomic cell boundary. Reuse pinned core/domain instances; declare transitive bindings explicitly. Accounting laws bind organization, book, currency, valuation basis, account roles, revision and opening balances.
2. Model things and happenings as concepts. Distinguish catalog Item from Holding/lot. Use typed links for object references; declare owned-draft identity/creation. State is folded history; no methods, hidden mutable state or implementation inheritance.
3. Define statecharts, typed action inputs, guards, terminal paths and refusals. Every policy requiring evidence or approval needs an executable action path. Repeated independent assays require a repeatable path and distinct accountable people. Exchanges need both economic sides and their link.
4. Enumerate reads and **all writers**: action effects, set, creation, imports, migrations, timers/results and triggered rules. Show direct effects and synchronous closure separately from later commands. Balance all synchronous effects in one commit. Validate full read sets, including absence and partition membership; model cross-cell commitments explicitly.
5. Attach structured laws where they govern, retaining inherited IDs/origins through containment, `is` and mappings. Layers add conjunctive constraints. Overlays bind pinned scope, refuse conflicts, and revalidate migration/progress; new behavior is a declared action.
6. Separate compile-time unit/type bindings from effective-dated settings. Pin the selected setting/definition version at commit; declare date basis. Backdating requires restatement. Parameter-local bounds use the same law IR; arbitrary setting changes are not automatically tightening.
7. Keep external computation behind typed, capability-scoped Functions/perform. Record receipts; accept results through checked actions. Commit outbox intent atomically; model duplicates, unknown outcomes, reconciliation and compensating effects. Never execute a Function inside a pure decision/law or dispatch effects from a fork.
8. Map external ontology and observed events as ordinary integration data with provenance, typed many-object roles, temporal identity and coverage. Keep nonconforming evidence; authoritative promotion is checked. Discovery/conformance are questions over data plus admitted definitions.
9. Use pure acyclic defined concepts over existing identities. Do not trigger behavior from membership or erase activated obligations when membership changes. For metrics/views, specify grain, fan-out, units, temporal additivity, freshness and zero-denominator behavior.
10. Generate initialization, per-writer preservation, non-vacuity and boundary obligations from actual semantics. Keep proof bodies with packs against generated statements. Bind evidence to immutable inputs; report unsupported/stale evidence as unknown. Inspect explain/`expects` at TARGET assurance levels; current diagnostics/receipts do not establish the whole target contract.

## Jewelry review prompts

- Sell: delivery plus payment/receivable, original-cost allocations and balanced per-currency effects. Scrap buyback is a new exchange unless it is a true sale return.
- Assay: append versioned evidence. A posted holding's changed fine-metal measure requires an explicit reconciling movement and variance counterpart; never overwrite purity/history.
- Melt: reserve inputs, fund WIP/`melting`, record output and loss provenance, derive declared totals/ratio, provide APPROVE/REJECT paths, invalidate stale approvals, and post output + loss + value effects atomically.
- A balanced constructor covers its own pairs; it does not certify account selection, valuation or arbitrary writers. Cardinality alone does not prevent double counting.

## Reject these shortcuts

- No domain code/opcodes in the kernel, including stock helpers renamed as generic operations. Domain compositions must expose their reads and effects.
- No hand-written per-feature theorem statements/models as application evidence. Proof bodies may be Lean only against generated statements and count after checking.
- No raw IR/`unsafeZen`/JDM in the normal authoring profile. Current `Norn.law`/`unsafeZen` leakage is the open [#58 gap](https://github.com/Norn-cloud/norn/issues/58), not an endorsed pattern; use supported typed expressions or report missing SDK semantics.
- No special observation construct, `observe()`, mining authority, `workflow()` or separate business runtime. Current `Norn.flow` is compiled lifecycle syntax.
- No hidden future trigger to finish balancing today's transaction, silent history rewrite, weakened inherited law or “unknown = proved.”

## Validate and return

- Use the reference's CURRENT command sequence: profile → compile/admission. Add named positive/refusal/correspondence scenarios for changed semantics; verify no partial writes, replay/idempotency and relevant races.
- Run the repo-required gates in its prescribed environment. SDK changes need its typecheck/tests; engine changes need affected tests, enrollment/provenance, cheap and required full gates. Never claim TARGET examples ran against today's SDK.
- Report changed definitions, invariants/scenarios, exact-head gate results and remaining semantic gaps. Review verdict: **approve** or **needs-attention**, with severity/confidence for findings. Leave merge/deploy decisions to the applicable repo rules and task authorization.
