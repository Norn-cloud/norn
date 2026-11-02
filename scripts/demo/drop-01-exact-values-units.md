# Drop 1: Exact values, bounded units and decimal proofs

Values landed before the law renderer. Publication requires a passing main Boat rehearsal.

Run `bash scripts/demo/drop-01-exact-values-units.sh` from a clean checkout
contained in freshly fetched `origin/main`, with Rust, GNU `timeout` and Kani
0.68.0 installed. It requires #357, #365 and #383.

Shows live conformance cases for exact numeric transport, cell commit/reopen,
range refusal without mutation, the pinned 12-code unit catalog and rejected
descriptor substitutions. It runs existing engine proptests and three Kani
harnesses against the production decimal core, each with a 300-second limit.
Normalization is bounded to signed 8-bit coefficients and scales 0–2; powers
cover exponents 0–6; the overflow case is exponent 39. Tool output is retained;
timeouts or failed checks stop the script and do not count as a passing drop.

This is a conformance/proof walkthrough, not a stock transaction or general
unit-aware arithmetic. It does not prove parsers, rounding, complete ledger
transactions or all governed surfaces. No application theorem is handwritten.
