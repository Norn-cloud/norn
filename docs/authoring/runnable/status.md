# Core release status

Source baseline: public main `2859d63d435a89ce82dc3902d24924c0882e95a8`
(2026-11-02). This supersedes the inspection baseline in the older core-spec
matrix for this runnable path. Open PRs do not count as released behavior.

Every primitive remains **partial** across IR, admission, evaluator, commit,
authoring and proof. The tests below qualify only the named behavior, not the
whole primitive or a universal proof. This is runtime/test evidence; proof
coverage remains bounded or unknown as described in the [core contracts](../../spec/core/README.md).

| Primitive | Released behavior exercised | Remaining target | Release test |
|---|---|---|---|
| 01 Values/identity | Exact decimal canonicalization | Complete unit/profile coverage | `cargo test --locked -p norn-ir --test runtime_values golden_decimals_are_exact_and_canonical -- --exact` |
| 02 Concept graph | Typed links and deletion integrity | Complete concept/meta-ontology authoring | `cargo test --locked -p norn-cell --test cell_host link_targets_are_typed_and_hierarchy_deletes_cannot_dangle_them -- --exact` |
| 03 Query/compute | Bounded scans refuse decode overflow | Full exact/IVM/OLAP correspondence | `cargo test --locked -p norn-cell --test reactive_query oversized_candidate_is_refused_before_decoding -- --exact` |
| 04 Transactions | Commit and outbox enqueue are atomic | Complete synchronous writer closure | `cargo test --locked -p norn-cell --test cell_host commit_and_enqueue_are_one_transaction -- --exact` |
| 05 Laws/authority | Missing law types/bindings refuse | Complete per-writer generated proof dispatch | `cargo test --locked -p norn-checker --test law_ir law_types_and_absent_bindings_fail_closed -- --exact` |
| 06 Composition/config | External releases must be pinned | Full retained-law composition and governed parameter execution | `cargo test --locked -p norn-checker --test ontology_pack ontology_pack_rejects_floating_external_releases -- --exact` |
| 07 World boundary | Unfinished claims reopen as outcome-unknown | Full reconciliation/compensation contract | `cargo test --locked -p norn-cell --test cell_host a_claim_without_a_result_survives_reopen_as_outcome_unknown -- --exact` |
| 08 Worlds/projections | Root tombstones stay world-scoped | Complete historical/restatement/lineage qualification | `cargo test --locked -p norn-cell --test reactive_query tombstones_in_another_world_do_not_hide_root_rows -- --exact` |

The doc-test executes these exact commands in the integration gate, requiring
one selected, passing test per row. It fails if a name drifts or selects no test.
A green row does not promote its remaining target to complete. Update this
baseline and its evidence together when qualifying a later release.
