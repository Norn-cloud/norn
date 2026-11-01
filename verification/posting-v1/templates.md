# Stock/GL template obligations

This specification implements part (b) of #214. `stock_templates.rs` constructs
typed synthetic LedgerPackIr fixtures and checks their affine certificates with
the existing checker. It is not installation, runtime posting or an admitted
ontology mapping. The synthetic ontology pin is explicitly not law evidence.

## Closed CP1 templates

| Operation | Quantity vector, scale 6 | GL vector, scale 2 |
| --- | --- | --- |
| receipt(q, value) | stock +q; external-source -q | stock +value; SRBNB -value |
| delivery(q, value) | stock -q; external-destination +q | COGS +value; stock -value |

Positive GL is debit. Each vector has zero constant and zero coefficient
residual per unit/layer. Quantity q > 0; GL value may be zero. All parameters
are canonical bounded integers at the declared precision. There are no float
parameters or caller-provided legs. Fixed-arity templates are instantiated in
authoritative manifest order, not expanded by a callback.

`receipt_delivery_templates_certify_every_parameter_coefficient` checks both
certificates and CP1 evaluated legs: receipt 5 units / journal 80, delivery
4 units / journal 60. `specialization_cannot_drop_legs_or_change_coefficients`
rejects dropped legs and coefficient changes. `account_substitution_invalidates_the_pinned_certificate`
shows that substituting a declared same-unit account still conserves but cannot
reuse the original certificate. Correct initial master-account selection remains
a separate admission obligation. `exact_parameter_scale_and_bounds_are_admission_obligations`
rejects wrong scale, negative amounts and overflow of the sealed range.

## Binding and transaction obligations

- POST-VALUE / FOLD-PARAMETER-ORIGIN: only lane c's pinned fold supplies value,
  allocations, before/after witnesses and immutable StockEvents. Quantity
  comes from authorized lines, masters fix accounts, units and valuation method.
  A conservation certificate alone cannot establish correct valuation.
- POST-VALUE / GL-CUMULATIVE-ROUNDING: for every warehouse/account group,
  value = round2(Vafter) - round2(Vbefore), exact half-up after deterministic
  aggregation. Never round each line. Receipt debits stock, delivery credits
  stock. A balanced misvalued journal fails correspondence admission.
- POST-ATOMIC: stock vector, events, GL, voucher/lines, outbox and both receipts
  belong to one existing guarded CellStore transaction. GL writes still pass
  `accountingLeg` balance, partition and immutability admission.
- POST-ORDER: pin the complete owned manifest and partition witness, including
  empty sets, plus every master/configuration version read; recheck all at commit.
- POST-IDENTITY: bind raw action intent, actor/scope, definition pins and the
  complete proposal. Identical replay precedes stale-read refusal after current
  authorization. Lost acknowledgement after COMMIT returns the existing result.
- POST-AUTHORITY: protected StockEvents and GLLegs require the pinned declared
  rule, immutable output types and lifecycle checks; no planner exception.
- EXTERNAL-FUNDING: the explicit external source is the only unrestricted
  quantity account for receipts, admitted through existing ledger configuration.
  Stock remains gross-funded and nonnegative; delivery destinations are explicit.
  A template certificate does not grant unrestricted-account authority.

## Inherited-law receipt requirements

Retain `core.law.quantity_conservation`, `core.law.value_conservation`,
`core.law.stock_valuation` and `core.law.posted_immutable` identities through
checked mappings to the licensed core registry. The posting rule must pin
the quantity pack/certificates, ontology/unit bindings, admitted master account
graph, fold revision, event/output types and inherited law receipts. Reject
missing, stale, substituted or weakened required bindings before activation.
Unknown proof or correspondence evidence remains unknown in the Guarantees view.
The tests here establish structural affine conservation only; they do not issue
those receipts, prove non-weakening, or establish fold/runtime correspondence.

Run: `cargo test --locked -p norn-checker --test stock_templates`.
Production conformance must additionally exercise BALANCED-MISVALUE,
AGGREGATE-HALF-CENTS, EMPTY-PARTITION-RACE, EXTERNAL-FUNDING,
LOST-ACK-AFTER-COMMIT and protected-output refusal through the same alpha action
surface. Transfers, returns, reversals and rounding-account corrections follow CP1.
