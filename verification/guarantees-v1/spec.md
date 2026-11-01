# Guarantees document v1

This is public evidence data for inclusion in an authorized server document.
The host owns authorization and document delivery; this unit adds no endpoint.
Law order is core primitives, accounting/inventory, jewelry, then workflow.
Every coverage law is included, using statements from obligation registries or
the demo catalog. A missing statement fails generation. Kernel/verification laws
use the core primitives layer; demo laws declare their layer. Coverage claims join
only by exact ID. Core statements come from the core obligation registry.

- `GUAR-IDENTITY`: duplicate law or coverage IDs refuse generation.
- `GUAR-SEPARATION`: model, execution and parity evidence are independent.
  Only a recorded `reference_proved` or `implementation_proved` proof with
  evidence earns `proved on the model`. Bounded checks remain recorded coverage.
  Execution requires an explicit `execution` claim with status `enforced`;
  parity requires an explicit `parity` claim with status `tested`. Each requires
  artifact, command and selectors. Ordinary runtime tests earn neither label.
- `GUAR-UNKNOWN`: missing claims, missing evidence and partial proofs stay
  `unknown`; an issue, planned law, source file or reference is not evidence.
- `GUAR-SCOPE`: preserve raw coverage, residuals and subsystem limitations.
  No installed definition or inherited receipt is implied by static coverage.
- `GUAR-DRIFT`: UTF-8 JSON with sorted keys, two-space indentation and a final
  newline regenerates byte-identically. No timestamps or checkout paths.

The conformance suite exercises these properties, including evidence removal,
misleading runtime tests, duplicate IDs and committed-document byte drift.
The catalog includes planned demo laws so absent enrollment is visible. Their
statements specify the target, not a claim that execution already enforces it.
