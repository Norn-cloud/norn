# Generic atomic partition commit

One guarded transaction binds business effects, ordered partition postings and
one retry identity. The primitive has no application or valuation semantics.

- POST-ATOMIC-PARTITIONS: a later refusal preserves every partition, business
  fact, history, outbox, timer and receipt.
- POST-PARTITION-FENCES: retain complete fact/link and balance read sets,
  including absent objects, empty memberships and revision-zero balances.
- POST-GROUP-IDENTITY: hash-bind every ordered input/result to one receipt;
  exact retries replay; changed/reordered inputs or duplicate ledgers refuse.
- POST-GROUP-REPLAY: reopen/export/restore reproduce every request and result;
  missing/extra/altered transitions or results refuse. Singleton bytes persist.
- POST-GROUP-AUTHORITY: proposals grant no permission. Existing kernel
  admission/current authorization govern writes; unsupported hosts refuse.

Evidence is execution/conformance, not a formal implementation proof.

POST-GROUP-SNAPSHOT: prepare all plans from the same verified pre-write state.
Multiple first definitions must not inspect another plan's unreceipted writes.

The core proposal is a `CellCommit`, an ordered slice of `QuantityRequest`
partition postings and one retry identity. `CellHost::commit_action_quantity`
and native `commit_with_partition_postings` use the existing guarded commit.
The primitive interprets no application fields, account purpose or valuation.
RuleIR-derived authority/effect composition remains the separate A4/A5 bridge;
unsupported owned-row/quantity combinations still refuse.
