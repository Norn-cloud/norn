# ERPNext oracle checkpoint 1

Input is `norn.stock-trace/1` from #215 / #221, with its exact decimal text,
warehouse aliases and request references. Serial fresh receipt/delivery traces produce `norn.stock-dump/1`, including
initial state, every command attempt, stock, raw GL and document statuses.
Unsupported/incomplete execution never passes. No Norn parity is claimed.

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

- **CP1-FIFO-seven:** `fifo.json` retains cross-layer 2 @ 10 + 3 @ 20, deliver 4
  (Q=1/V=20), and smoke 10 @ 2, deliver 3 (Q=7/V=14). Seven checkpoints
  include both initials. Rows come only from REST, never expected trace values.
- **CP1-timeout-attempt:** accepted attempts survive an unavailable snapshot;
  its explicit unsupported status cannot masquerade as an empty observation.
