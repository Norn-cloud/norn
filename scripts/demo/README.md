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

Fallback status: **checker-admitted / refused; obligations generated; runtime execution not yet (#341 #342 #343)**.

The default exits successfully only when the current admission demonstration succeeds; it always says INCOMPLETE. `--require-complete` exits 1 while required beats are missing. Unexpected compile/check failures stop execution. No tenant, external integration or stock/books state is manufactured. The script has no output fixtures or mocked adapters.

CURRENT: `jewelry.source.ts` declares a small measurement schema and lifecycle, not a complete REA implementation. `Norn.law` in the same TypeScript source authors the conservation law; the Rust example reads that compiled artifact and invokes the existing checker and obligation generator. Its IR mutations are negative conformance probes, not business-law definitions. It prints admitted definitions, EN/AR reviewed wording slots, exact generated challenge/assessments, an actual invalid-binding refusal and a protected-law-edit refusal. A law admission, transition-frame assessment or goal inventory is not runtime enforcement, a generated solver counterexample or a proof. The local approval object is example-only and grants no deployment authority. The reviewed Arabic uses the accounting term **مُرحَّلة** for posted. Structured bilingual authoring is still **TARGET ([#58](https://github.com/Norn-cloud/norn/issues/58))**: only its wording adapter remains in Rust. EN/AR controlled rendering now calls the real RequirementView over that same compiled law, with locale direction and stable slot IDs. This is display, not solver evidence; no parallel renderer is implemented here.

The [gap matrix](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6559837435) is the checkpoint record. [Acceptance](../../docs/spec/acceptance/jewelry-demo.md) pins the transaction cycle; [data acceptance](../../docs/spec/acceptance/data-engineering.md) is being reconciled to scrap yield in #353. The rehearsal never substitutes the post-demo inventory fixture.

## Authoring correspondence

`DEMO-TS-LAW`: profile → compile the TypeScript law, then admit the resulting artifact unchanged. Its source path and explicit entity dependency participate in the generated challenge, which binds the original compiler JSON bytes before typed projection. Re-serialization is confined to the deliberately edited refusal probes. No positive law definition is constructed in Rust. The live refusal probes edit that compiled law to `true` (protected-law refusal) and an absent property (predicate-type refusal); they make no execution/proof claim. Review the authored law and the TARGET bilingual adapter together until #58 supplies that profile constructor.

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
