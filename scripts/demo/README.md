# Jewelry rehearsal and fallback

Run on Boat/CI from a clean checkout with Rust 1.98, Bun and frozen authoring dependencies:

```sh
(cd tools/norn-ts && bun install --frozen-lockfile --ignore-scripts --backend copyfile)
bash scripts/demo/run.sh
```

Catalog inspection: `bash scripts/demo/drop-03-catalog.sh`. The [Drop 3 note](drop-03-catalog.md) describes its bounded reads, actual refusals and uncovered behavior.

Declared allocation plans: `bash scripts/demo/drop-04-declared-folds.sh`. The
[Drop 4 note](drop-04-declared-folds.md) distinguishes exact pure evaluation
from the pending native cell cycle.

Three systems, one ontology: `bash scripts/demo/drop-three-systems.sh`. The
[note](drop-three-systems.md) separates mapped observation proposals from
durable promotion through the Observed inbox.

Fallback status: **checker-admitted / refused; obligations generated; runtime execution not yet (#341 #342 #343)**.

The default exits successfully only when the current admission demonstration succeeds; it always says INCOMPLETE. `--require-complete` exits 1 while required beats are missing. Unexpected compile/check failures stop execution. No tenant, external integration or stock/books state is manufactured. The script has no output fixtures or mocked adapters.

CURRENT: `jewelry.source.ts` declares a small measurement schema and lifecycle, not a complete REA implementation. Melt implements `core.Conversion`; the Rust example reads the generated core LawIR reference to its invariant without restating conservation and invokes the existing checker and obligation generator. Its `--challenge-json` mode exports only the checker-generated challenge for evidence projections. The normal mode prints admitted definitions, EN/AR controlled-renderer slots, exact generated challenge/assessments, an actual invalid-binding refusal and a protected-law-edit refusal. The shared Core 06 overlay validator refuses replacing the inherited conversion invariant with `core.composition.overlay_conflict` / `weakens_inherited`; additive guards retain the exact law digest and remain pending solver/progress evidence. Beat 4 runs the actual shared kernel commit checks against the generated core invariant: balanced measurements pass; omitted/negative loss and wrong scale refuse without fact, event or outbox writes. This is measurement conservation, not stock/value effect closure. A law admission, transition-frame assessment or goal inventory is not a generated solver counterexample or a proof. The local approval object is example-only and grants no deployment authority. EN/AR controlled rendering calls the shared RequirementView with locale direction and stable slot IDs. Structured bilingual authoring still uses a Rust wording adapter (TARGET #58). No parallel renderer or proof authority is introduced here.

The [gap matrix](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6559837435) is the checkpoint record. [Acceptance](../../docs/spec/acceptance/jewelry-demo.md) pins the transaction cycle; [data acceptance](../../docs/spec/acceptance/data-engineering.md) is being reconciled to scrap yield in #353. The rehearsal never substitutes the post-demo inventory fixture.

## TARGET app declarations — not compilable CURRENT imports

The following is the application fallback for a definition review if runtime slices miss CP3. It follows the [authoring reference](../../docs/authoring/README.md). Names below refer to pinned Norn packs/declarations, not Rust helpers, executable callbacks or kernel opcodes. No solver statement is authored by hand. Missing constructors/semantics are #340–#346, owned by their core lanes.

```ts
// TARGET notation, not @norn/authoring exports.
const jewelry = pack({
  uses: [core, inventory, accounting, sales],
  bindings: { organization, book, currency: "USD", valuationBasis,
    accountRoles: { stock, wip, processLoss, cash, payable, receivable, salesRevenue, cogs },
    revision, openingBalances, atomicCell },
  concepts: {
    Metal: concept({ is: inventory.Holding, props: { metal: MetalKind },
      links: { item: inventory.Item.one, assays: Assay.many, acceptedMeasurement: Assay.one },
      laws: { purity: law({ shall: m => m.acceptedMeasurement.purity.between(0, 1000) }) } }),
    Assay: concept({ is: core.EconomicEvent,
      props: { grossGrams: Grams, purity: PerMille, fineGrams: FineGrams,
        effectiveAt: Instant, recordedAt: Instant, evidenceId: EvidenceId },
      links: { lot: Metal.one, assessor: core.Agent.one, source: SourceDocument.one } }),
    ScrapExchange: concept({ is: core.Exchange,
      links: { seller: core.Agent.one, draftLot: ownedDraft(Metal), assays: Assay.many,
        consideration: core.Commitment.one },
      states: { assaying: { RECORD_ASSAY: "assaying", OFFER: "offered" },
        offered: { ACCEPT: "bought", DECLINE: "returned", RECORD_ASSAY: "assaying" } },
      policies: { acquisition: requireTwoDistinctPeopleForSameLotRevision },
      actions: { RECORD_ASSAY: appendEvidenceAndInvalidateOffer,
        OFFER: bindExactPriceCurrencyExpiryAndMeasurement,
        ACCEPT: receiveMetalAndCreatePayableAtomically, DECLINE: releaseDraftAndHolds } }),
    Melt: concept({ is: core.Conversion,
      links: { inputs: Metal.many, outputs: Metal.many, loss: LossEvidence.one,
        approval: ApprovalEvidence.one },
      states: { draft: { START: "melting" }, melting: { MEASURE: "measured" },
        measured: { APPROVE: "approved", REJECT: "needsRevision", POST: "posted" },
        approved: { REVISE: "measured", POST: "posted" }, needsRevision: { REVISE: "measured" } },
      actions: { START: reserveAndTransferToWip,
        MEASURE: appendOutputAndLossEvidence, APPROVE: bindAuthorizedManagerAndExactRevision,
        REJECT: requireRevision, REVISE: appendEvidenceAndInvalidateApproval,
        POST: atomicallyConsumeWipAndPostOutputLossAndValue },
      laws: { fineMass: law({ shall: m => m.inputFine.eq(m.outputFine.add(m.lossFine)) }),
        grossMass: law({ shall: m => m.inputGross.eq(m.outputGross.add(m.nonFineByproduct).add(m.processLoss)) }) },
      policies: { excessLoss: requireApprovalWhenLossExceeds("0.005") } }),
  },
  actions: { SELL: linkedDeliveryAndReceivableWithOriginalCost,
    RECONCILE_ASSAY: appendMeasurementAndVarianceMovementWithCounterpart },
  laws: { stockMatchesBooks: boundStockSubledgerEqualsBook,
    valueBalance: perCurrencyDebitsEqualCredits },
})
```

Every named action/helper above is a required **declaration to author**, with explicit complete reads/effects; none currently executes. In particular SELECT/POST cannot call hidden domain code. Required links from core interfaces remain inherited; their full admission/composition is pending. Assays repeat with distinct accountable people. Scrap receipt and payable are a new exchange; returns reference original cost allocations. Changed measurements/outputs invalidate offers/approvals. Reconciliation appends movements and never edits old purity. POST checks current authority, full input membership and all synchronous counterparts in one commit. Cancellation after START must compensate WIP; rejection cannot abandon funded inputs.

Review targets, not results: start 100 fine g/value 1000; sell 20 g for 300 (stock 80/value 800); acquire 25 gross g at 800‰ for 180 (stock 100 fine g/value 980); fund WIP 100/980; post 99.2 fine g/value 972.16 plus loss 0.8 g/value 7.84 and non-fine byproduct 5 g. Loss ratio 0.008 needs approval; exactly 0.005 does not. Missing loss, purity 1100‰, stale approval and inherited-law weakening must refuse without partial effects. These vectors are never printed as completed transactions by the runner.

The separate analytical population is 16 weeks of gold/silver melts, 48/16 gold training/holdout, with 294/300 → 292.5/300 restatement. Its authenticated bad purity-1.20 observation is retained/quarantined; native invalid writes refuse. Feed replay, accepted population, freshness, lineage, IVM/recompute, bitemporal results and publication remain explicit `not yet` lines until the owning lanes land them.

CURRENT Guarantees projection: beat 5 renders `scripts/guarantees_matrix.py` over the registry/coverage and the checker-generated Melt challenge. Without a current verification result it shows unknown. `--result RESULT.json` reuses the existing full validator before showing checked proof or named-test evidence; reference proofs retain their scope and do not become production proofs. The input/result digests identify the immutable snapshot. Generated goals and structural admission remain unknown, and the matrix grants no authority.
