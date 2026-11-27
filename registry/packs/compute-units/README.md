# `compute-units` pack: Extended UCUM / ISO 80000 / IEC 80000-13 Units & Compute/AI REA Metering

Defines exact extended UCUM / ISO 80000 / IEC 80000-13 unit catalogs outside
`registry/core/artifacts/core.units.json` (preserving embedded cell base digests)
alongside first-class ISO 15944-4 REA compute and AI metering concepts.

## Unit catalogs

- **`units.compute.UnitDefinition`** (`version: 0.1.0`): 22 exact UCUM / IEC 80000-13 units
  - **Sub-second time** (base `s`): `ms` (`1/1000`), `us` (`1/1_000_000`), `ns` (`1/1_000_000_000`)
  - **Information / storage** (base `By`, IEC 80000-13): `bit` (`1/8`), `By` (`1/1`),
    `KiBy` (`1024/1`), `MiBy` (`1_048_576/1`), `GiBy` (`1_073_741_824/1`), `TiBy` (`1_099_511_627_776/1`),
    `kBy` (`1000/1`), `MBy` (`1_000_000/1`), `GBy` (`1_000_000_000/1`), `TBy` (`1_000_000_000_000/1`)
  - **AI & compute** (UCUM curly-brace countable/compound): `{token}`, `{input_token}`,
    `{output_token}`, `{request}`, `{vCPU}.s`, `{vCPU}.h` (`3600/1` of `{vCPU}.s`),
    `{GPU}.s`, `{GPU}.h` (`3600/1` of `{GPU}.s`), `{seat}`
- **`units.physical.UnitDefinition`** (`version: 0.1.0`): 10 exact ISO 80000 physical, logistics & energy units
  - **Length** (base `m`): `m` (`1/1`), `cm` (`1/100`), `mm` (`1/1000`), `km` (`1000/1`)
  - **Area** (base `m2`): `m2` (`1/1`)
  - **Volume** (base `m3`): `L` (`1/1000`), `mL` (`1/1_000_000`), `m3` (`1/1`)
  - **Energy** (base `J`): `J` (`1/1`), `kW.h` (`3_600_000/1`)

Each catalog carries `UNITS-SCALE-1` exact rational factor laws (`exact_time_factor`,
`exact_information_factor`, `exact_compute_count_factor`, `exact_compute_time_factor` on
`units.compute.UnitDefinition`, and `exact_rational_factor` on `units.physical.UnitDefinition`)
binding every code to its exact `(dimension, base_code, numerator, denominator)` tuple with
positive integers.

## REA Compute & AI Metering concepts

- **`compute.TokenPool`** (`implements core.Resource`, `role: thing`): conserved token
  pool (`unit = "{token}"`, `available >= 0` in `{token}`).
- **`compute.UsageEvent`** (`implements core.EconomicEvent`, `role: happening`):
  metered consumption of `{token}`, `{input_token}`, `{output_token}`, `{GPU}.s`,
  `{GPU}.h`, `{vCPU}.s`, `{vCPU}.h`, `GiBy`, or `{request}` (`metered_in_compute_unit`),
  with `{token}`-scoped exact `Norn.rate` pricing (`token_usage_billed_exact` and
  `compute.usage.price`: `Rate<USD, {token}> × Quantity<{token}> → Money<USD>`).
- **`compute.QuotaHold`** (`implements core.Hold`, `role: happening`): rate-limit,
  budget-lock, or GPU reservation hold with `LIFE-SOUND-1..8` lifecycle
  (`open -> consumed | released | expired`).
- **`compute.CapacityCommitment`** (`implements core.Commitment`, `role: happening`):
  reserved GPU hours, monthly token plan, or storage tier commitment with
  `LIFE-SOUND-1..8` lifecycle (`requested -> promised -> fulfilled | cancelled | breached`).

## Evidence

| Claim | Evidence | Where |
| --- | --- | --- |
| Canonical IR compiles byte-for-byte from typed TypeScript | structural | `tools/norn-ts/test/analytics-and-units.test.ts` |
| Full `norn-checker` admission over `registry/core/artifacts/*` and `VAL-RATE` dimensional checking (`Rate<USD, {token}> × Quantity<{token}> → Money<USD>`, mismatch refusal on `{GPU}.s` and unpinned codes) | structural | `crates/norn-checker/tests/analytics_and_units.rs` |
| `UNITS-SCALE-1` exact rational invertibility and associative composition (`ms -> s`, `KiBy/GiBy -> By`, `kW.h -> J`, `KiBy ≠ kBy`) | checked proof (reference semantics over `ExactRat`) | `verification/bi-additivity-v1/BiAdditivity.lean` |
