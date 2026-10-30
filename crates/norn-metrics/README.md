# Analytical tables

`TemporalPlan` resolves numeric fields from the effective ontology and admits a
bounded point-in-time snapshot. It refuses future feature knowledge, stale
features, unavailable outcomes, duplicate object/time grain and invalid values.
Chronological holdouts require training outcomes known before the boundary.

These checks cover supplied evidence only. The exporting host must authorize
reads and authenticate source receipts and ledger heads. No live ingestion,
incremental maintenance, statistical publication or formal proof is claimed.

Tests compile a synthetic typed declaration with the repository's Bun authoring
toolchain, then pass its generated artifacts to the Rust catalog.
