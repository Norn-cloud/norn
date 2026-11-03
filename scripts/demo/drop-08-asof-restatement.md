# Drop 8: as-of answers and atomic restatement

Pending main landing of #497; no PR-head run qualifies this drop.
Run `bash scripts/demo/drop-08-asof-restatement.sh` on clean fetched main with
Rust 1.98 and Python 3.9 or newer. It prints one native binary SHA-256 and a fixed
seed/ChaCha algorithm, verifies all ten named scenarios exist, and executes them live.

The authenticated fixture retains K1 294/300 = 0.98 and K2
292.5/300 = 0.975, with loss 6 to 7.5 g and the unchanged denominator.
A staged generation retains old complete answers until governed activation
switches every declared slice atomically. Interrupted, incomplete, stale,
unapproved and host-mismatched candidates cannot replace the old generation.
Old slot versions remain readable. A 16-case generated correction sequence
checks activated deltas against recomputation through redelivery/restart.
The clock-ahead regression and a separate 16-case property retain the pinned
cutoff through lower receipt times, preparation failure, restart and recovery.

The typed restatement law/actions/reviewer policy are admitted together.
Native action-path tests supply a fixture allow/deny authorizer; they do not
qualify production identity, delegation or a complete Cedar policy deployment.
This drop shows neither full source-document lineage nor forecast publication,
and makes no generated application-proof claim. Historical selection and
runtime/sampled durability evidence remain separate from exhaustive-finite
selector evidence supplied by its owning lane.
