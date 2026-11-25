# ml pack: model versions and their promotion (#1171)

A model's definition is a `Norn.model` (ModelIR: input, features, target,
typed outputs, trust, output contracts); its internals stay opaque. This
pack holds what happens to a trained model:

- **ml.ModelVersion**: one trained version. It names its training run and
  its evaluation run (`tracking.Run`, by `run_id`, linked as `trained_by`
  and `evaluated_by`), the artifact (URI, sha256, size), the training input
  revision, the pinned evaluation dataset revision, the revision the
  evaluation actually read, a typed metric, and whether every output
  contract held. Status: `candidate` → `production` → `retired`.
- **ml.modelversion.register**: create-only, and only when both runs are
  `FINISHED`. The kernel stamps `registered_by` (protected) with the verified
  principal (ACTOR-STAMP-1); a caller never supplies it.
- **ml.modelversion.promote**: the `promote` transition, guarded by the
  law `ml.promotion_policy` (invariant `evaluated_on_pinned_dataset`): no
  version reaches production unless its evaluation read the pinned dataset
  revision and every contract held. A failed guard refuses with
  `core.projection.publication_guard` (ANALYSIS-PUBLISH-1: an evaluation on
  another revision than the pinned one is temporal leakage). Separation of
  duties: `independentOf: ["registered_by"]` (ACTOR-INDEPENDENT-1), so the
  principal who registered a version never promotes it.
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
- **ml.access**: members register, request promotion and record prediction
  batches; admins and owners retire.

`sdks/python/models/run-demo.sh` runs registration, the law, the separation
of duties (a dev host has one principal, so its own promotion is refused) and applying a
model (`sdks/python/models/norn_models.apply_model`, which checks every output
contract on every row before it writes anything) end to end on a dev host. The approval gate needs a hosted cell's roles (a dev host declares
none); `crates/norn-cell-do/tests/ml_approval.rs` admits the definition against
the hosted grant table.
