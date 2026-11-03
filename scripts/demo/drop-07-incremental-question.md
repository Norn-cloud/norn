# Drop 7: incremental questions equal recomputation

Pending main landing of #497; no PR-head run qualifies this drop.
Run `bash scripts/demo/drop-07-incremental-question.sh` on clean fetched main.
Rust 1.98 and Python 3.9 or newer are required. The script records one native binary
SHA-256 and a fixed generated-input seed and ChaCha algorithm, verifies all three named
scenarios exist, and runs them through the real cell query/IVM implementation.

The main correspondence property declares 32 generated cases: updates,
deletes, date/material changes, declared exclusions/recovery, redelivery and
restart compare complete incremental results against exact recomputation at
the same frontier. Other scenarios refuse a foreign population/scope binding
and a skipped receipt without partially replacing the published result.
This is sampled runtime correspondence, not a universal proof.

The native fixture supplies normalized facts and declared exclusion context.
It does not capture signed feeds or turn an imported law finding into that
context automatically. Source-document lineage joining, OLAP correspondence
and the complete DATA-INGEST/QUARANTINE pipeline are separate capabilities.
