# Synthetic ERPNext measurements (D1a)

This profile extends the ERPNext map-only system pack with fictional custom
measurement fields. They are not standard ERPNext fields or a running connector.
The approved design is [demo beat 6](https://github.com/Norn-cloud/norn/issues/213#issuecomment-6558867021).

Invariants and correspondence scenarios:

- `D1a-exact-measurements`: mapping preserves six-place gram/purity values,
  input/output/loss roles, metal and source document/child-row identity.
- `D1a-history`: sixteen UTC weeks ending October 25, 2026 contain 64 gold
  and 16 silver melts. The first 48 gold melts precede the last 16; bounded
  deterministic variation gives nonzero predictor and residual variance.
- `D1a-showcase`: four gold inputs total 300 g; outputs total 294 g and losses
  total 6 g. Gross mass times purity equals fine mass for every generated row.
- `D1a-evidence-only`: mapping emits ordinary measurement evidence, never a
  stock mutation or a fabricated Holding. Canonical lot/event links remain
  declared gaps until lane C's corrected Holding/conversion contracts qualify.
  Catalog Item is not Holding. Missing source fields refuse mapping.

Purity bounds and conservation are not enforced by this mapping. D3 supplies
law findings/quarantine; a purity-1.20 observation must remain representable here.
Ingress authentication, durable acceptance, canonical promotion and signed
synthetic feed replay depend on D2 and the ingress lane. Historical corrections
require new evidence and reconciliation; this cut supplies no restatement API.

The directly attached `demo.erpnext.purity_in_range` law governs the measurement
population. Purity `1.20` remains source evidence: observation evaluation emits
a finding and quarantine; the same native proposal refuses. Mapping preserves
the source value. Runtime enforcement is qualified by the Core 07 integration
cuts, independently from this declaration's checker admission.
