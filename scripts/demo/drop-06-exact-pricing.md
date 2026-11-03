# Drop 6: exact jewelry pricing commits and reopens

Run `bash scripts/demo/drop-06-exact-pricing.sh` from clean, fetched main with
Rust 1.98, Bun and frozen `tools/norn-ts` dependencies (`--backend copyfile`).
This canonical entrypoint replaces the earlier misnumbered
`drop-01-exact-pricing.sh`; it is the same pricing capability, not another
delivered Drop 1.

Five TypeScript scenarios check rate metadata, compiler refusals, exact
callback bounds and fresh extraction of the retained pricing fixture. Two
native SQLite scenarios check the actual admitted action: 20 fine g at
15 USD/g commits 300 USD, and 20 fine g at 9 USD/g commits 180 USD. Retry
and reopen retain the committed result. EUR, an mg denominator, zero mass
and an inexact cent result under Refuse are covered without partial writes.

This is price derivation and durable action acceptance. It does not value
inventory layers, balance GL, record assays or execute the complete jewelry
cycle. That work belongs to T1-C1. No new Kani or application proof is
claimed. Main rehearsal evidence pins the engine source separately from a
wrapper PR if the entrypoint has not yet landed.
