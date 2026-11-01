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
- **DIFF-REVIEW:** each differing request/account or request/line/warehouse group binds case, method, step,
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
  unsupported cases/checkpoints and open bugs (unique issue URLs).
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

## CP2 serial lifecycle

`lifecycle.py` emits the shared trace format, moving average first, then FIFO:
receipt 10 @ 2 → receipt 10 @ 4 → delivery 3 → transfer 2 a/b → return 1 against the delivery →
cancel return → cancel delivery → cancel transfer → cancel second/first receipt. The checker
asserts all 20 outcomes, Q/V at every checkpoint, balanced books, warehouse
transfer deltas and all original documents cancelled at completion.

- **CP2-native-reference:** returns read the original Delivery Note line and
  carry `return_against`/`dn_detail`, negative quantity, original warehouse and
  sale rate; valuation remains entirely native. No original-cost override.
- **CP2-native-cancel:** cancel through the public document method against the
  original native ID. Cancellation attempts retain their own request ID but do
  not alias the original document to the cancel command. Native cancellation
  rows/flags and GL reversals remain observable under the original request.
- **CP2-transfer:** Material Transfer carries both warehouse aliases on one
  stock line, with no invented receipt or delivery; both warehouse accounts
  must show opposite quantity/value deltas.
- **CP2-order:** reverse live dependents before originals. Cancel command times
  are evidence, not a rewritten posting timestamp on the original document.

Run generated traces through `run.py`, then `python3 lifecycle.py results.json`
on the disposable fork. Moving-average rounding cases, refusal reason
normalization, duplicate/race replay and integration with the real Norn driver
remain later units; this witness alone never claims full moving-average parity.

- **CP2-acceptance-selection:** `boat.py --acceptance lifecycle` selects the
  20-checkpoint checker; the default remains the two-step smoke checker. Both
  require native accepted effects; selecting a checker never waives failures.
- **CP2-cold-bootstrap:** a fresh fork observed native ERPNext installation at
  39% when the controller's 600-second execution limit expired. Timeout is
  incomplete evidence, never a pass. A bounded continuation on the same owned
  VM must first confirm native ERPNext installation before replay. Bootstrap
  budget is extended for unattended lifecycle batches.

- **CP2-fresh-transfer-master:** provision Material Transfer through the public
  Stock Entry Type resource; a fresh site otherwise refuses transfer with
  LinkValidationError. No native document/schema export is used.
- **CP2-repost-service:** enable and start the native scheduler as well as its
  queue worker. Pin scheduler tick to 10 seconds on this disposable
  site and wait at most 120 seconds per checkpoint. The first live cancel
  exposed queued valuations that a worker alone could not dispatch. Native
  repost computation is never replaced by a harness callback. See the official
  [background jobs documentation](https://docs.frappe.io/framework/user/en/api/background_jobs).
- **CP2-budget:** lifecycle controller execution is bounded to 1800 seconds,
  with a 1860-second CLI subprocess bound; smoke remains 600/660 seconds.
  Creation remains bounded to 300 seconds; all paths retain owned-VM cleanup.

- **CP2-repost-cadence:** REST observations showed native `repost_entries` uses
  Hourly Maintenance, so starting a scheduler alone cannot meet a short bound.
  Pin native valuation jobs to once-per-minute Cron through Scheduled Job Type
  REST updates, as supported by the background-jobs documentation. Keep the
  native computation unchanged and allow one minute plus scheduler/worker
  latency within the 120-second checkpoint deadline.

- **CP2-snapshot-timeout-receipt:** retain the attempted command before reading
  its checkpoint. A read/repost timeout leaves `status: "unsupported"` on that
  checkpoint and preserves its native ID and outcome, even when accepted.
  Empty stock/GL arrays there are placeholders, never observed empty business
  state; the comparator marks the checkpoint unsupported and fails.

Checkpoint limitation: the development fork still did not settle valuation
reposts after the scheduler configuration corrections. The full 20-checkpoint
lifecycle witness is not qualified. Positive receipt/delivery/transfer/return
observations do not establish cancellation or cross-engine parity. The next
unit must qualify a fresh fork with the final configuration and lifecycle budget.

- **CP2-failed-batch-artifact:** retrieve an available normalized dump even when
  native replay or acceptance exits nonzero. Preserve the failure exit; download
  never makes an unsupported case pass. Download failure on an otherwise
  successful run fails too, and owned-VM deletion runs in either case.
  Retrieval is bounded to 60 seconds and skipped when creation never reached
  execution, so an allocated creation stall still starts cleanup immediately.
  `test_failed_acceptance_keeps_dump_before_deletion` is its correspondence.
