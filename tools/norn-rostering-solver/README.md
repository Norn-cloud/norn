# `norn-rostering-solver` (`norn.solver.rostering@0.1.0`)

Deterministic Google OR-Tools CP-SAT shift-rostering solver worker for the Norn Governed HR vertical (`registry/packs/hr/rostering.ts`, `registry/packs/loc-eg/egypt.ts`).

## Execution Contract (Unmarked Slice Boundary)

In `registry/packs/hr/rostering.ts`, `SolveRosterFn` declares the cell-level handle `{ rosterId, unitId, preferenceProfileId }`. This worker implements the **unmarked slice execution contract**: the cell host adapter resolves those handles, strips all marked PII / compensation fields under `V2Markings` / `AccessIR`, and invokes `POST /v1/functions/norn.solver.rostering/invoke` with an unmarked `SolveRequest` JSON payload containing **only opaque identifiers** (`roster_id`, `unit_id`, `profile_id`, `employee_id`, `shift_id`, `role_code`), pinned `iana_timezone`, and exact integer / exact-scale decimal quantities. Zero free-text labels (`display_label`, `label`, etc.) cross the solver boundary.

## Architecture & Guarantees

1. **Production OR-Tools CP-SAT Backend (`ortools.sat.python.cp_model`):**
   - Single-worker (`num_search_workers = 1`), pinned `random_seed = 0`, canonical sorted ordering of employees, shifts, variables, and constraints.
   - Repeated solves and permuted input arrays produce byte-identical canonical JSON and identical full-payload SHA-256 `solver_receipt_digest` (`sha256:...`).
   - Honest status & evidence mapping:
     - `OPTIMAL` → `"optimal"` (`evidence_label: "exhaustive-finite"`)
     - `FEASIBLE` → `"feasible"` (`evidence_label: "bounded"`)
     - `INFEASIBLE` → `"infeasible"` (`evidence_label: "exhaustive-finite"`, only status that triggers UNSAT repair)
     - `UNKNOWN` → `"unknown"` (`evidence_label: "unknown"`, never falsely claims infeasibility or runs repair)
     - `MODEL_INVALID` → fail-closed `NORN-SOLVER-MODEL-INVALID` error
2. **Exact Integer Minutes & Currency Units — Zero IEEE `float`, Zero Silent Rounding (`boundary.py`, `models.py`):**
   - Duplicate-key-proof strict UTF-8 JSON parser (`object_pairs_hook=reject_duplicate_keys`) refuses duplicate JSON object keys (`NORN-SOLVER-DUPLICATE-KEY`), shadowed PII subtrees (`NORN-SOLVER-PII-FORBIDDEN`), bare JSON floats / `NaN` / `Infinity` (`NORN-SOLVER-FLOAT-FORBIDDEN`), and malformed UTF-8 (`NORN-SOLVER-JSON`).
   - Integer minute fields are preserved directly without rounding through 2-decimal hours; over-scale currency strings (> 2 decimal places) and invalid decimals fail closed with `NORN-SOLVER-DECIMAL` instead of silently quantizing.
3. **Fail-Closed PII & Closed-Schema Boundary (`boundary.py`):**
   - Recursively refuses any payload containing marked PII fields (`name`, `full_name_ar`, `national_id`, `iqama`, `phone`, `mobile_no`, `email`, `base_salary`, `hourly_overtime_rate`, `medical_note`, `gender`, `dob`, `bank_account`, `bank_iban`, `emergency_contact`, `home_address`, etc.) with `NORN-SOLVER-PII-FORBIDDEN`.
   - Enforces a strict closed-schema allowlist at every object level containing only opaque IDs and typed operational quantities; any unallowlisted key (including `display_label` or `label`) fails closed with `NORN-SOLVER-UNKNOWN-FIELD` and its exact dotted JSON path.
4. **Exact Overnight & ISO-Week Timezone Accounting + Hard Laws (`HR-SH-01..04` + statutory baselines):**
   - Every shift interval `[starts_at, ends_at)` is split across local calendar days and ISO weeks using `zoneinfo.ZoneInfo(request.iana_timezone)` (validated IANA timezone, defaulting to `"UTC"`).
   - An overnight shift crossing local midnight counts **both** calendar days toward consecutive-day streaks and splits its exact minutes across each local calendar day and ISO week in both `solver.py` and the independent `verifier.py`.
   - Diagnostic codes align with `registry/packs/frappe-hr/ORACLE-CHECKLIST.md`: `HR-SH-01` (overlapping/duplicate shifts), `HR-SH-02` (approved leave exclusion), `HR-SH-03` (minimum rest gap), `HR-SH-04` (typed UNSAT repairs with zero silent statutory relaxation), plus `HR-SH-COV`, `HR-SH-ELIG`, `HR-SH-STAT`, and `HR-SH-BUDGET`.
5. **Independent Hard-Law Post-Verifier (`verifier.py`):**
   - Independently verifies every feasible schedule emitted by CP-SAT against all hard laws without trusting solver internals; fails closed (`NORN-SOLVER-POSTVERIFY-FAILED`) on any violation.
6. **Honest Multi-Repair Slack-Variable UNSAT Diagnostics (`solver.py`):**
   - Runs only on proven `INFEASIBLE` primary models. Adds bounded integer slack variables on `relax_coverage`, `approve_overtime` (strictly capped at statutory `profile.max_weekly_minutes` — never violating the statutory weekly ceiling!), and `allow_transfer` (cross-branch and cross-skill) while keeping all statutory laws strictly enforced and post-verified.
   - Labels repairs `"minimum_slack"` (`is_minimum_slack: true`) only when the repair solver proves `OPTIMAL`; bounded `FEASIBLE` repairs are labeled `"feasible_slack"` (`is_minimum_slack: false`).
7. **Full-Payload Receipt Integrity (`receipts.py`):**
   - `solver_receipt_digest` hashes the complete canonical response payload (`assignments`, `metrics`, `unsat_summary`, `status`, `evidence_label`, `repair_optimality`, `repair_suggestions`, `alternative_repairs`, `verification`, `input_digest`, etc.); `verify_receipt_digest` fails closed on any tamper.
8. **Strict `FunctionManifest` & Bounded HTTP Worker (`worker.py`):**
   - Stdlib `ThreadingHTTPServer` with socket read timeouts, bounded concurrency (`BoundedSemaphore`), pre-model cardinality bounds (`MAX_EMPLOYEES = 250`, `MAX_SHIFTS = 1000`, `MAX_HORIZON_DAYS = 62`, `MAX_LEAVE_BLOCKS = 2000`), and `MAX_RESPONSE_BYTES` output cap.
   - Exposes:
     - `GET /health` (liveness + `source_digest`, `manifest_digest`, `endpoints`)
     - `GET /manifest` (strict 16-field `FunctionManifest` matching `crates/norn-ir/src/function.rs` `#[serde(deny_unknown_fields)]`, `implementation.format = "host"` pinned by `source_digest`, `X-Norn-Manifest-Digest` response header)
     - `POST /v1/functions/norn.solver.rostering/invoke`

## Running Locally

```bash
cd tools/norn-rostering-solver
uv sync --frozen
uv run --frozen pytest -v
uv run --frozen mypy --strict --no-incremental norn_rostering_solver tests
uv run --frozen ruff check norn_rostering_solver tests

# Run the HTTP worker
uv run --frozen norn-rostering-worker --host 127.0.0.1 --port 8091
```
