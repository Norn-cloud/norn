# Generic atomic partition commit

One proposal consists of ordinary business effects and an ordered list of
partition quantity postings, under one retry identity and one guarded cell
transaction. The primitive contains no allocation, costing or document rules.

- POST-ATOMIC-PARTITIONS: any later-partition refusal preserves all earlier
  partitions, business facts, history, outbox, timers and receipts.
- POST-PARTITION-FENCES: retain complete fact/link and balance read sets,
  including absent objects, empty memberships and revision-zero balances.
- POST-GROUP-IDENTITY: the entire ordered input and every result are hash-bound
  to one receipt. Exact retries replay; changed, omitted or reordered entries
  conflict. Duplicate ledger entries refuse.
- POST-GROUP-REPLAY: reopen/export/restore reproduce every request and result.
  Missing, surplus or altered transitions/results refuse. Singleton receipts
  retain their original canonical scalar bytes.
- POST-GROUP-AUTHORITY: proposals grant no permission. Existing kernel admission
  and current authorization govern all writes; unsupported hosts refuse.

The specification is enforced and tested, not a formal implementation proof.
Application rules and laws must separately establish ownership, conserved
measures, provenance and financial interpretation. No domain algorithm or
second posting path is introduced.
