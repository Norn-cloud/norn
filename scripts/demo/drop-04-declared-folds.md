# Drop 4: declared folds produce exact allocation plans

Run `bash scripts/demo/drop-04-declared-folds.sh` from clean, fetched main.
Install the frozen `tools/norn-ts` dependencies with Bun's `--backend copyfile`;
Rust 1.98 and Python 3 are required.

The script checks fresh TypeScript extraction against the retained jewelry
RuleIR, then builds one native evaluator test binary, prints its SHA-256 and
runs seven named correspondence/refusal scenarios. Test output is live; no
stored output is replayed. The source is
`tools/norn-ts/fixtures/jewelry/cp3-folds.source.ts`.

The native assertions cover FIFO cost 60/residual value 20, moving-average
cost 64/residual value 16, independent capacity allocation, deterministic
ordering and bounded-fold refusals. The jewelry plan checks sale cost 200
and residual 80 g/value 800, scrap 20 fine g/value 180, funding 100 fine g/
105 gross g/value 980, output value 972.16, loss value 7.84 and 5 g non-fine
byproduct. It also checks reordered products, the strict 0.005 approval
boundary, omitted loss, invalid purity, wrong units and inexact arithmetic.

These are pure evaluator plans. `complete` and `needs_approval` are returned
data; no cell is opened, no stock or books are posted, and no manager grants
approval. The TypeScript test always checks extraction correspondence; its
optional WASM checker branch runs only when a checker package is configured.
Do not infer artifact-set admission from a compiler-only run. T1-C1 owns the
native business cycle and real authorization. No ERPNext sampled parity,
generated solver counterexample or checked application proof is claimed.

Publish only after these commands pass against a recorded main source.
If a committed wrapper is rehearsed before its PR lands, pin both wrapper
and main source explicitly in the drop note; PR-head engine runs count zero.
