# `analytics` pack: BI Additivity (`BI-ADD-1`) & Analytics Preregistration

Grounded in **W3C RDF Data Cube (`qb:`)**, **Open Semantic Interchange (OSI 0.1.1)**,
and Norn's `DATA-FORECAST` / `DATA-PUBLISH-GUARD` acceptance contracts
(`docs/spec/acceptance/data-engineering.md`).

## Definitions

- **`bi.CertifiedMetric`** (`thing`): governed metric contract carrying exact
  `additivity` (`"additive" | "semi_additive" | "non_additive"`), `grain`,
  `unit_code`, strictly positive `freshness_sla_ms`, `owner -> core.Agent`, and
  a `LIFE-SOUND-1..8` certification lifecycle (`draft -> certified -> deprecated`,
  `immutable: ["deprecated"]`, `endings: { certified: "resting", deprecated: "cancelled" }`).
- **`bi.MetricSlice`** (`happening`): observed W3C Data Cube slice linked to
  `bi.CertifiedMetric`, carrying `numerator`, `denominator`, and scale-6 `value`
  with laws `denominator_non_zero` (`self.denominator !== 0`) and
  `non_additive_ratio_consistent` (when `non_additive`: `denominator > 0` and
  `|numerator / denominator - value| <= 0.5 * 10^-6`, written without division
  so non-terminating ratios like `1/3 = 0.333333` admit while out-of-tolerance
  or unweighted-average ratios refuse).
- **`analytics.Study`** (`happening`): analytical / experimentation / ML study
  with `train_cutoff_at`, `holdout_start_at`, `diagnostics_passed`, and a
  `LIFE-SOUND-1..8` lifecycle (`preregistered -> evaluated -> published | withheld`,
  `immutable: ["published", "withheld"]`, `endings: { published: "completed", withheld: "cancelled" }`).
  Governed structurally by `temporal_split_ordered` (`ANALYTICS-SPLIT-1`:
  `train_cutoff_at <= holdout_start_at`) and `publish_requires_diagnostics`
  (`DATA-PUBLISH-GUARD`: `publication_status != 'published' || diagnostics_passed`).
  *(Note: freezing `population_digest` / `method_digest` / split fields prior to
  terminal states and setting `diagnostics_passed` are left to host/action
  contracts outside these structural laws.)*
- **`analytics.Publication`** (`happening`): governed publication decision
  record linked to `analytics.Study` and `reviewer -> core.Agent`, enforcing
  `publication_requires_diagnostics` structurally *(no lifecycle immutability
  declaration is attached to this type)*.

## Evidence

| Claim | Evidence | Where |
| --- | --- | --- |
| Canonical IR compiles byte-for-byte from typed TypeScript | structural | `tools/norn-ts/test/analytics-and-units.test.ts` |
| Full `norn-checker` admission, `LIFE-SOUND-1..8`, `PROOF-LIFECYCLE-2/3` bounded graph & `LAW_BLOCKED_EXIT` when `withhold` remedy is removed and `diagnostics_passed = false` | structural + bounded | `crates/norn-checker/tests/analytics_and_units.rs` |
| `BI-ADD-1` rollup homomorphism, non-additive ratio counterexample, semi-additive stock theorem, and `ANALYTICS-SPLIT-1` no-lookahead separation | checked proof (reference semantics over `Int`) | `verification/bi-additivity-v1/BiAdditivity.lean` |
