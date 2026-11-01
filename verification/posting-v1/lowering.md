# Receipt/delivery lowering correspondence

Part (d) of #214, building on #258's POST model, #260's templates and #257's
FIFO API. `lower_stock_posting` is pure preparation, not posting authority.
Its caller must supply authorized manifest-ordered lines, replay history in
commit order and admitted master bindings, then seal all corresponding reads
(including empty partitions) into `commit_owned_rows` with its output.

- POST-VALUE: one partition, quantity/value scale 6, GL scale 2. Receipt
  acquisition is half-up(q × unit cost / 10^6); only `allocate_fifo` chooses
  delivery allocations. Stock GL is round2(sum after) − round2(sum before).
  Never sum individually rounded line GL. Zero-cost movements remain legal.
- POST-ORDER: apply lines in manifest order; historical movements replay through
  `replay_fifo`. Bound history and candidate cardinality before traversal.
- POST-AUTHORITY: masters fix exact account scope/book/graph/version, currency,
  layer and template identities. Reject mixed account pins or collapsed roles.
  This structural check does not establish master admission or rule authority.
- POST-ATOMIC: pure refusal preserves inputs, including a partially processed
  multi-line candidate. Storage rollback remains the guarded commit's duty.
- POST-IDENTITY: identical lowering inputs reproduce the same movement/leg IDs
  and output. Storage replay/conflicting retry remain action receipt duties.

CP1-FIFO checks receive 2 @ 10 + 3 @ 20, deliver 4, quantity 1/value 20,
receipt Dr stock 80 / Cr SRBNB 80, delivery Dr COGS 60 / Cr stock 60.
Additional witnesses: aggregate half cents, zero cost, partial-candidate
oversell, mixed master pins, invalid quantities, overflow and duplicate lines.
Evidence is execution-tested pure correspondence, not alpha acceptance.
Follow-up: checked Post bindings, authoritative reads, protected outputs,
native atomic/retry tests and alpha invoke/replay. No installed action is claimed.
