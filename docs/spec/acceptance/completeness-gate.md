# Same-binary core completeness gate

[Astra] This suite qualifies the eight primitives together. [Default] Completeness covers bounded governed-business applications, not arbitrary computation or external truth. Run every scenario on one recorded runtime binary digest; only admitted packs, parameters, synthetic data and Function receipts may differ. Adding an app-specific opcode fails the gate.

## Harness and evidence contract

Each case records definition/dependency/evaluator pins, scope/world, effective/knowledge times, source frontiers, exact input/expected outputs, refusal/finding codes, commit dumps and evidence. Use real evaluator/cell/surface paths. Run crash/race schedules deterministically; compare committed facts, links, ledger, commands and timers, not only an HTTP status. A negative case must prove unchanged authoritative state unless it explicitly expects durable observed evidence.

Named cases are required, not claimed passing here. Structural, runtime, sampled parity, bounded checking and checked proof remain separate columns. No skipped case counts as passed; unsupported semantics is a missing capability, even if its refusal is correct. Every law needs initialization, preservation for all writers, non-vacuity and boundary goals; a goal inventory is not a proof result.

| Case | Given | When | Then |
|---|---|---|---|
| COMPLETE-ALLOCATE | Ordered lots `2×10`, `3×20`; independent capacity grants 2+3 | Consume/allocate 4 using declared folds | FIFO cost 60, residual 1/value 20; capacity residual 1; same evaluator, no stock helper. |
| COMPLETE-UNITS | USD ledger and fine-gram ledger as distinct pack instances | Post balanced 25 USD and 100 g transfers | Each balances within its dimension; summing USD+g or USD+EUR refuses. |
| COMPLETE-TAX | Net 100 USD; tax 10% before July 1, 12% from July 1 | Post on June 30 and July 1; later revise settings | Tax 10/12 and gross 110/112; original receipts retain setting pins. |
| COMPLETE-UPGRADE | In-flight process v1; v2 adds an approval | Activate v2, continue old instance, attempt migration | Existing instance stays v1; new instance uses v2; missing state/approval mapping refuses migration atomically. |
| COMPLETE-RACES | Balance 5; two requests for 4; folded empty/populated partitions | Interleave consumption, insertion, removal and update | At most one consumption succeeds; every changed membership invalidates stale preparation; no partial effects. |
| COMPLETE-PAYMENT | Payment 25; provider succeeds but reply is lost | Redeliver request, reconcile, cancel/refund | Same operation ID; exactly one consequence only under provider contract; otherwise unknown; refund is separate 25 operation. |
| COMPLETE-AUTHORITY | Delegated actor with an approved command and a fork | Revoke before commit/dispatch; attempt fork perform | No new action/attempt after revocation; fork never performs; late dispatched outcome still recorded. |
| COMPLETE-HISTORY | Imported event links supplier, voucher and holding; one invalid source record | Run ordinary questions against declared meta-ontology | Multi-object lineage retained; conformance finding/quarantine visible; no special mining opcode. |
| COMPLETE-DATA | Declared inventory-value question across ERPNext/Shopify | Run exact, IVM and OLAP at identical frontier and time | Canonical totals/lineage equal; freshness explicit; restatement changes only later knowledge views; see beat 6. |

## Release gate

[Owner] The [jewelry demo](jewelry-demo.md), [data-engineering beat](data-engineering.md) and [ontology MVP](master-ontology-mvp.md) are required alongside this suite. Data scientists/engineers are a primary demo audience.

[Default] Pass only when all named scenarios run without skips, every required runtime guard is active, no application-specific runtime branch exists, and the declared assurance profile is met. Unknown proof may remain only where the admitted release policy explicitly permits runtime safety enforcement; report it. Never claim progress/liveness from a commit-time check.

Implementation PRs run focused correspondence plus repository-required CI/Boat gates from [core conventions](../core/README.md). Docs alone neither prove nor qualify runtime behavior. Preserve existing oracle scenarios until declared replacements demonstrate parity. CI results attach to exact heads; the coordinator decides release/merge readiness, and this pack does not enqueue work.
