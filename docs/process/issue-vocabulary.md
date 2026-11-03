# Issue vocabulary

Issues describe outcomes in Norn's own terms. The authority is the
[thesis](../architecture/norn-thesis.md) and [Core 01–08](../spec/core/README.md),
not a label or a passing PR. The build DAG is tracked in issue #566.

## Kind, track and checkpoint

Use one `kind:*`: `task` (bounded implementation), `finding` (observed gap,
ambiguity or contradiction), `bug` (incorrect implemented behaviour), `epic`
(child outcomes), or `vision` (proposed direction with a falsifiable target).
Existing `dag:task`, `spec-finding`, `bug`/`type:bug` and `epic` remain useful
compatibility labels; do not create synonyms for them.

Choose one of the existing nine `track:*` labels: `cell-network`, `ci-infra`,
`data-analysis`, `demo-ui`, `dx`, `engine-core`, `hosts`, `laws-proofs`,
`mappings-integrations`. C1 is engine + demo MVP, C2 is ES submission, C3 is
after ES. The matching milestone is authoritative; a new idea does not
silently expand C1. Record checkpoint changes and new dependencies on #566.
Issue-form answers describe classification; triage applies labels/milestones.

## Spec and Core references

Cite stable invariant IDs verbatim, for example `VAL-EXACT-1`, `TXN-ATOMIC-1`,
`PROOF-BIND-1`, and named acceptance scenarios where relevant. Find IDs in
`docs/spec` and `docs/architecture` with
`grep -rohE '\b[A-Z]{2,}(-[A-Z0-9]+)*-[0-9]+\b' docs/spec docs/architecture`.
Discard incidental matches such as SHA-256 and UTF-8. A scenario name is not
necessarily a numbered invariant. Use `new` for a proposed contract and name
the governing document; do not invent an existing spec citation.

Apply all governing `core:*` areas: 01 values/identity; 02 concept graph;
03 query/compute; 04 transactions/effects; 05 laws/authority;
06 composition/configuration; 07 world boundary; 08 worlds/projections.
Area means contract ownership, not every dependency mentioned in an issue.

## Primary concept

Use exactly one `concept:*` from this closed list. Classify by the outcome's
shared mechanism; REA groups cover issues whose outcome is the business kind.

| Label suffix | Meaning |
|---|---|
| rea-party-resource | REA Agent and Resource; ownership and custody |
| rea-event-process | EconomicEvent, Process, Conversion and Exchange |
| rea-obligation-ledger | Commitment, Claim, Hold and LedgerEffect |
| law | Governing requirements and protected law propagation |
| writer-action | Checked writers, transitions and atomic effects |
| mapping | Source, term and checked law-carrying interpretations |
| question-metric | Typed questions, grain, measures and analysis |
| fold | Deterministic event reduction and incremental maintenance |
| world-projection | Forks, history, subscriptions and rendered views |
| cell-host | Durable cell identity, placement, hosts and transport |
| function | Sole code boundary, manifests and recorded execution |
| evidence-proof | Generated obligations, receipts and evidence levels |
| authorization | Principals, scope, delegation and access decisions |
| identity-value | Exact values, units, stable IDs and keys |
| connector | External feeds, ingestion and governed write-back |

Claim here is the REA right to resources; a verification claim belongs under
`evidence-proof`. Functions stay behind perform. Mapping source evidence is
not proof of law preservation. Imported violations and governed refusals are
different outcomes even when they cite the same law.

## Relations and triage

`Relates-to: #N` groups related goals without asserting dependency or identity.
A duplicate has the same outcome AND acceptance; shared concepts/IDs alone
are insufficient. Keep epics and their implementation children distinct.
`Supersedes: #N` on the newer issue must explain how every older acceptance
item is covered; link back with `Superseded-by: #N` before closing the older
issue. Proposed closures go to the coordinator; triage does not close issues.
Quote both demands and identify their common contract when reporting a
contradiction. Preserve owner text: append a `Spec IDs:` line only when absent
and clearly implied. Uncertain scope or citations belong in the hub report.
