# BI Additivity (`BI-ADD-1`), Analytics Split (`ANALYTICS-SPLIT-1`), and Exact Unit Scaling (`UNITS-SCALE-1`)

Standalone Lean 4 reference proofs (`import Std` only, zero `sorry`, no custom
axioms — only Lean core `propext` and `Quot.sound` — checked under
`lean -DwarningAsError=true`).

## Laws and evidence

| Law | Evidence | Where |
| --- | --- | --- |
| `BI-ADD-1` additive homomorphism (1D lists, concatenation order, 2D entity × time tables) | checked proof (reference semantics over `Int`) | `BiAdditivity.lean`: `additive_homomorphism`, `additive_comm_append`, `additive_rollup_2d` |
| `BI-ADD-1` non-additive weighted ratio soundness + unweighted-average and sum counterexamples | checked proof (reference semantics over `ExactRat`) | `BiAdditivity.lean`: `weightedRatio_append_sound`, `ratio_not_averageable`, `ratio_not_summable` |
| `BI-ADD-1` semi-additive balance spatial homomorphism, temporal double-count theorem, and unchanged-holding preservation (`lastSnapshot [s, s] = some s`) | checked proof (reference semantics over `Int`) | `BiAdditivity.lean`: `semi_additive_spatial_commutes`, `semi_additive_temporal_sum_double_counts`, `exists_semi_additive_temporal_double_count`, `lastSnapshot_unchanged` |
| `ANALYTICS-SPLIT-1` no-lookahead separation, disjoint windows, and late-label exclusion | checked proof (reference semantics over `Int`) | `BiAdditivity.lean`: `no_lookahead_split`, `disjoint_train_holdout`, `late_holdout_label_excluded` |
| `UNITS-SCALE-1` exact rational invertibility, associative composition, and concrete UCUM / IEC 80000-13 / ISO 80000 factor identities (`ms -> s`, `us -> s`, `ns -> s`, `bit -> By`, `KiBy/MiBy/GiBy/TiBy -> By`, `KiBy ≠ kBy`, `kW.h -> J`) | checked proof (reference semantics over `ExactRat`) | `BiAdditivity.lean`: `scale_invertible`, `scale_compose`, `scale_respects_eqv`, `ms_roundtrip`, `kiBy_roundtrip`, `giBy_roundtrip`, `kWh_roundtrip`, `giBy_composes_kiBy_cubed`, `kiBy_neq_kBy`, `kWh_eq_3600_kJ` |

## What is not claimed

- These theorems prove algebraic properties of their own `Int` and `ExactRat`
  reference definitions. They do not prove correspondence to Rust expression
  evaluation, SQL/DuckDB/DataFusion query lowering, or cell storage execution.

## Running

```sh
lean -DwarningAsError=true verification/bi-additivity-v1/BiAdditivity.lean
```
