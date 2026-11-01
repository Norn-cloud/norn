# ERPNext oracle checkpoint 1

Input is `norn.stock-trace/1` from #215 / #221, with its exact decimal text,
warehouse aliases and request references. This checkpoint exercises one fresh
serial receipt followed by one delivery; other operations are unsupported.
Output is `norn.stock-dump/1`, as documented on #215, including an empty initial
checkpoint, every attempted command, native identities, raw GL legs and status.

## Named invariants and correspondence

- **CP1-isolation:** fork the prebuilt ERPNext v16 Boat snapshot; create a fresh
  site and company, USD currency, Unit UOM, one FIFO item and warehouse accounts
  a/b. Perpetual inventory is enabled and negative stock disabled. Credentials
  never leave the fork. `boat.py` deletes its owned VM on success and failure
  and requires an explicit not-found response to verify deletion.
- **CP1-source:** the fork checks the uploaded committed archive's SHA-256 before
  extracting regular files/directories only. No ERPNext code/schema is imported.
- **CP1-rest-number-exact:** decimal text becomes exact JSON numbers at the REST
  boundary; response numbers are parsed as Decimal. Neither binary floating
  point conversion nor decimal strings may replace JSON numeric tokens.
- **CP1-stock-fold:** receipt 10 at rate 2 produces Q=10,V=20; delivery 3 then
  produces Q=7,V=14, rate=2, deltas (+10,+20),(-3,-6), FIFO queue [[7,2]].
- **CP1-books:** receipt debits stock-a 20 / credits SRBNB 20; delivery debits
  COGS 6 / credits stock-a 6. Every checkpoint balances; both documents submit.
- **CP1-evidence:** `check_smoke.py` corresponds to number-exact, stock-fold and
  books using live REST output. Evidence is sampled behaviour on the pinned
  runtime, not a proof of equivalence. Incomplete execution is unsupported.

## Discovered setup and boundary scenarios

- **fresh-site-language:** System Settings needs `language=en` before saving.
- **fresh-site-transit:** company creation needs Warehouse Type Transit.
- **leaf-customer-group:** Customer refuses a group-type Customer Group.
- **delivery-price-list:** Delivery Note needs selling price list, currency and
  conversion rate even when every line already has a price.
- **decimal-submit:** stringifying numeric document fields can make native
  submission refuse with TypeError; preserve numeric tokens through submission.
- **runtime-readiness:** the prebuilt image cache contains MariaDB 10.11 and
  Redis 7 image tags; version output is an array of app/version/commit records.
  App commits are unavailable in this image; version plus image digest pin it.

- **socket-only-bootstrap:** database ping may accept a temporary socket-only
  server; site creation waits for an authenticated TCP query instead.
- **creation-stream-stall:** a stalled controller creation stream must be killed
  within 300 seconds so allocated VM cleanup runs; other CLI calls are bounded.

Remaining contract scenarios and 10b reviewed dispositions are the next unit.
No result in this checkpoint claims cross-engine parity or race correctness.

## 10b exact comparison contract

`python3 tools/stock-parity/compare.py --norn norn.json --erpnext erpnext.json
--dispositions reviewed.json --output report` writes sanitized Markdown and JSON.
The Norn dump may be a stub for conformance; that does not establish live parity.

- **DIFF-EXACT:** compare initial and every consecutive checkpoint, cumulative
  stock row Q/V and deltas in integer millionths, GL gross debits and credits
  grouped by request/account/currency/cancellation in cents. Excess scale fails;
  no epsilon, floating point, final-only netting or cross-request cancellation.
  Native identities are excluded across engines; raw GL legs remain separately
  in JSON. Refusal preserves stock/GL contents and native identity sets locally.
- **DIFF-REVIEW:** each differing field has a SHA-256 binding case, method, step,
  field and both observed values. Dispositions are an array keyed by fingerprint;
  duplicates, stale entries or missing evidence fail. Each entry needs `reviewer`
  (GitHub login), `review` (public review URL), and exactly one `bucket`:
  `norn-bug` or `erpnext-bug` requires public `issue`, `reproduction`, `receipt`
  URLs; ERPNext bugs also require a named `invariant`. `intentional-difference`
  requires `decision` and public `decisionRecord`. Review evidence is supplied by
  the reviewer, not inferred by this offline comparator. Open bugs fail the gate.
- **DIFF-DECISIONS:** pre-registered approved decision IDs are
  `original-cost-returns`, `no-negative-stock`, `backdating-refused`, and
  `delivery-stock-cogs-only` (#214 design review). Registration never silently
  accepts a diff; a reviewed, value-bound disposition must reference the decision.
- **DIFF-COVERAGE:** missing/incomplete cases or checkpoints are unsupported and
  fail; unsupported is never a disposition. Empty corpora fail. Untriaged diffs
  fail. Counts distinguish matched checkpoints, intentional differing fields,
  unsupported cases/checkpoints and open bugs (differing fields).
- **DIFF-PUBLIC:** case/request/account/currency and observed string labels become
  stable opaque aliases. Native IDs, native messages, versions and input paths
  are omitted. Evidence accepts only public Norn issue/PR URLs. JSON retains
  exact integer observations and individually aliased raw legs; Markdown gives
  the checkpoint parity table and counts. Only sanitized outputs are publishable.

`test_compare.py` supplies the stub Norn/ERPNext dumps and correspondence cases:
representation-only equality, one-millionth intermediate drift, net-equal but
wrong gross GL, refusal identity churn, missing coverage, reviewed dispositions,
stale/duplicate evidence and public-label omission. Run on Boat:
`python3 -m unittest discover -s tools/stock-parity -p 'test_*.py'`.
