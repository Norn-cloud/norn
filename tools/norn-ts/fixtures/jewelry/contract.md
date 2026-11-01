# Authored jewelry contract

Dependency: stock definitions #227; model references #232 and #243. Evidence in
this unit is structural admission and exact input normalization, not posting.

- `sourceProvenance`: compiled stock and jewelry artifacts each retain the path
  of their own authored root module.
- `exactPurity`: six-place karat or three-place parts-per-thousand fineness
  maps exactly to a numerator over 24,000,000; no binary floating point.
- `exactPurityRefusal`: missing, nonpositive, out-of-range, malformed or excess
  precision inputs refuse with `purity_unrepresentable`; disagreeing valid
  representations refuse with `purity_conflict`. Each supplied representation must
  be valid before conflict detection, including when the other is valid.
- `checkedFineMass`: positive integer micrograms times the numerator is at most
  2^96-1 (runtime Decimal’s largest integer). Refuse with `fine_mass_overflow` before multiplication; malformed or
  nonpositive gross input refuses with `invalid_gross_mass`.
- `refining_conserves` / `fineConserved`: the mixed-purity fixture maps to the
  Lean/statechart normalized fine ticks exactly; adding one tick violates it.

MetalLot extends stock.Item; custody and refining vouchers extend stock.Voucher.
Owned lines link voucher and lot; returns also link the original receipt line.
Metal identity, warehouse and ownership come from the lot. No title-transfer
operation is defined. Loss includes fine ticks, reason, value and account.

Remaining binding requirements for the shared atomic stock/GL lowering:
per-metal AND per-owner input = output + loss; available inputs; named custody
owner and no title reassignment; nonempty loss reason; company loss posts its
value to a declared process-loss account; custody loss has zero company value
and no GL effect. Cancellation is linked exact reversal after dependents reverse.
These map to `refining_conserves`, `refused_is_noop`, `fineConserved`, `lossValued`,
`custodyIsolated` and `refusedIsNoop`. They are NOT enforced by structural schema
admission or the pure normalization fixture. No submit/cancel actions, runtime
receipts or inherited-law proof claims are emitted until that adapter exists.
