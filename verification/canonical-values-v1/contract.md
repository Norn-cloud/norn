# Canonical exact values: initial Core 01 profile

Implements the numeric transport portion of VAL-EXACT-1 from Core 01 (#339).
This profile reuses `RuntimeValue`, `Decimal`, `PropertyType`, `UnitRef` and
the existing scalar codec. It introduces no alternate numeric representation.

- Runtime numbers have a 96-bit unsigned coefficient, sign and canonical
  scale 0–28. The maximum magnitude is `79228162514264337593543950335`.
  Inputs outside that exact range refuse rather than round.
- Governed ingress parses JSON number-token text. An already rounded binary
  float cannot recover its source identity; construction helpers accepting
  in-memory floats are not an exact ingress contract.
- Canonical equality removes fractional trailing zeroes and negative zero.
  Declared property type, unit and scale remain in admitted definitions;
  they are not reconstructed from a numeric token's spelling.
- Decimal columns store canonical text; runtime JSON and ledger replay retain
  exact numbers. Source evidence bytes are separate from canonical values.
- Units retain `UnitRef {system, code}` identity. No implicit conversion is
  qualified here. Vocabulary membership and dimension-aware arithmetic remain
  missing capabilities; syntax-only unit validation does not prove VAL-UNIT-1.
- Explicit `round(value, 2)` uses midpoint-away-from-zero: `1.005` → `1.01`,
  `-1.005` → `-1.01`. Division by zero returns an evaluator error.
  Existing `norn-zen/6` implicit arithmetic scale reduction is unchanged;
  declared rounding-site admission and half-even authoring are not qualified.

`property_codec::runtime_error_code` adds registered `core.values.inexact`
alongside existing adapter errors for `RuntimeValueError::Inexact` and
`NonFinite`. It does not recategorize malformed JSON as numeric inexactness.
Scalar decimal encoding preserves the typed range error for exact string
inputs instead of erasing it into a generic value-kind mismatch.

Correspondence: `val_roundtrip_runtime_tokens_reach_the_real_evaluator_exactly`
checks runtime token → actual evaluator, including both profile boundaries;
`val_round_explicit_midpoint_away_and_zero_divisor` checks explicit rounding
and zero division; `val_roundtrip_transport_codec_commit_and_reopen` checks
`9007199254740993` and `0.10` through JSON, scalar encoding, SQLite commit,
ledger/hash verification and reopen. These are sampled executable checks,
not a universal proof or qualification of every API/CLI/MCP ingress.

IR, codec, evaluator and storage reuse are covered separately. No new
authoring contract, atomic zero-divisor transaction scenario, stable identity,
calendar policy or generated arithmetic proof is established by this cut.

The codec proptest generates signed 64-bit whole parts and six-digit
fractions: numeric and string inputs must yield identical canonical storage,
and decode/JSON round trips must preserve their exact value.
`diagnostic_kani.rs` exhaustively checks the three production error variants
against the canonical diagnostic adapter, including absence for malformed JSON.
This narrow harness proves classification and absence of panic, not the
heap-backed arbitrary-length decimal/JSON parser or evaluator arithmetic.
