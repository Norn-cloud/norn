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
  `FINISHED`.
- **ml.modelversion.promote**: the `promote` transition, guarded by the
  law `ml.promotion_policy` (invariant `evaluated_on_pinned_dataset`): no
  version reaches production unless its evaluation read the pinned dataset
  revision and every contract held.
- **Sign-off**: `promote.approval.json` is an APPROVAL-1 definition. Installed
  with `POST /v1/definitions:admit` then `:install` (with the signoff), it
  gates promote: a call parks as a decision an admin or owner approves, and
  the approved call still goes through the law.
- **ml.access**: members register and request promotion; admins and owners
  retire.

`sdks/python/models/run-demo.sh` runs it end to end on a dev host.
