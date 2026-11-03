#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/drop-common.sh"
drop_main "$@"
if ! grep -Fq 'fn val_roundtrip_transport_codec_commit_and_reopen' crates/norn-cell/tests/exact_values.rs; then
  printf 'not yet: #357 (exact-value profile and refusal correspondence)\n'; exit 1
fi
if [[ ! -f registry/core/artifacts/core.units.json || ! -f crates/norn-checker/tests/core_units.rs ]]; then
  printf 'not yet: #383 (pinned unit vocabulary and admission cases)\n'; exit 1
fi
if [[ ! -f verification/canonical-values-v1/decimal_kani.rs ]]; then
  printf 'not yet: #365 (production decimal core and bounded harnesses)\n'; exit 1
fi
if ! command -v kani >/dev/null || [[ $(kani --version) != *'0.68.0'* ]]; then
  printf 'not yet: #365 (install Kani 0.68.0 to rehearse the bounded proofs)\n'; exit 1
fi
printf 'Exact transport, commit/reopen, rounding and unit-catalog admission cases\n'
cargo test --locked -p norn-cell --test exact_values -- --nocapture
cargo test --locked -p norn-expr --test profile_conformance -- --nocapture
cargo test --locked -p norn-checker --lib property_codec::tests -- --nocapture
cargo test --locked -p norn-checker --test core_units -- --nocapture
cargo test --locked -p norn-checker --lib quantity_ledger::decimal::tests -- --nocapture
printf 'Bounded production-core Kani: i8 coefficient/scale 0..2; powers 0..6; overflow at 39\n'
for harness in canonical_decimal_preserves_bounded_values \
  decimal_scale_multiplier_is_exact_in_bounded_profile decimal_scale_overflow_refuses; do
  timeout 300 kani verification/canonical-values-v1/decimal_kani.rs --harness "$harness" --output-format terse
done
printf 'not yet: #339 (general UnitRef arithmetic, all governed surfaces and full-engine proof)\n'
