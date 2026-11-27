# ml pack: model versions and their promotion (#1171)

A model's definition is a `Norn.model` (ModelIR: input, features, target,
typed outputs, trust, output contracts); its internals stay opaque. This
pack holds what happens to a trained model:

- **ml.ModelVersion**: one trained version. It names its training run and
  its evaluation run (`tracking.Run`, by `run_id`, linked as `trained_by`
  and `evaluated_by`), the artifact (URI, sha256, size), the training input
  revision, and the pinned evaluation dataset revision. Once an authenticated
  `ml.forecast.evaluate` `FunctionReceipt` is accepted (`RECEIPT-CONSUME-1`),
  it records the receipt digest (`evaluation_receipt`), the evaluated dataset
  revision (`evaluation_revision`), the typed metric, and whether every output
  contract held. Status: `candidate` → `evaluated` → `production` → `retired`.
- **ml.modelversion.register**: create-only, and only when both runs are
  `FINISHED`. Registers a `candidate` without caller-supplied evaluation
  metrics or contract booleans. The kernel stamps `registered_by` (protected)
  with the verified principal (ACTOR-STAMP-1); a caller never supplies it.
- **ml.modelversion.accept_evaluation**: the `accept_evaluation` transition
  (`candidate` → `evaluated`), declaring `acceptReceipt` pinned to the
  `ml.forecast.evaluate` v1 Function manifest digest. Derives
  `evaluation_receipt`, `evaluation_revision`, `metric_name`, `metric_value`,
  and `contracts_passed` exclusively from the host-authenticated receipt.
- **ml.modelversion.promote**: the `promote` transition (`evaluated` →
  `production`), guarded by the law `ml.promotion_policy` (invariant
  `evaluated_on_pinned_dataset`): no version reaches production unless it
  carries an authenticated evaluation receipt (`evaluation_receipt != null`),
  its evaluation read the pinned dataset revision, and every contract held. A
  failed guard refuses with `core.projection.publication_guard`
  (ANALYSIS-PUBLISH-1: an evaluation on another revision than the pinned one
  is temporal leakage). Separation of duties: `independentOf: ["registered_by"]`
  (ACTOR-INDEPENDENT-1), so the principal who registered a version never
  promotes it.
- **ml.modelversion.retire**: the `retire` transition, under the same
  separation of duties.
- **Sign-off**: `promote.approval.json` is an APPROVAL-1 definition. Installed
  with `POST /v1/definitions:admit` then `:install` (with the signoff), it
  gates promote: a call parks as a decision an admin or owner approves, and
  the approved call still goes through the law.
- **ml.PredictionBatch** and **ml.predictionbatch.record** (M4): one
  application of a model version to an input snapshot (`input_type`,
  `input_revision`, the number of predictions written). Create-only, and only
  for a version in `production`; the batch is linked to it as `applied`. Each
  prediction is an object of the model's `predictions` type, carrying
  `subject` (the scored object's id) and `model_version`.
- **ml.access**: members register, accept authenticated evaluation receipts,
  request promotion and record prediction batches; admins and owners retire.

`crates/norn-cell/tests/supply_lab_stress.rs` (Axis 6) exercises the full
durable SQLite cell lifecycle end to end: closed registration, host receipt
retention (`SqliteCell::retain_function_receipt`), `ml.modelversion.accept_evaluation`,
replay/wrong-run/wrong-manifest refusals, separation of duties
(`ACTOR-INDEPENDENT-1`), `ml.promotion_policy`, and forecast recording.
`sdks/python/models/run-demo.sh` (`e2e_promotion.py` and `e2e_apply.py`)
requires host-retained evaluation receipts (`NORN_EVALUATION_RECEIPT_1` and
`NORN_EVALUATION_RECEIPT_10`; wiring supervisor receipt intake into
`norn-dev-host` is tracked in `#1934`). The approval gate needs a hosted
cell's roles (a dev host declares none); `crates/norn-cell-do/tests/ml_approval.rs`
admits the definition against the hosted grant table.
