# `norn-rostering-solver` (`norn.solver.rostering@0.1.0`)

Deterministic Google OR-Tools CP-SAT shift-rostering solver worker for the Norn Governed HR vertical (`registry/packs/hr/rostering.ts`, `registry/packs/loc-eg/egypt.ts`).

## Architecture & Guarantees

1. **Production OR-Tools CP-SAT Backend (`ortools.sat.python.cp_model`):**
   - Single-worker (`num_search_workers = 1`), pinned `random_seed = 0`, canonical sorted ordering of employees, shifts, variables, and constraints.
   - Repeated solves and permuted input arrays produce byte-identical canonical JSON and identical SHA-256 `solver_receipt_digest` (`sha256:...`).
2. **Zero IEEE `float` Arithmetic (`boundary.py`):**
   - Strict JSON parser rejects bare JSON floats, `NaN`, and `Infinity` at ingress with `NORN-SOLVER-FLOAT-FORBIDDEN`.
   - All shift durations, rest gaps, daily/weekly caps, and costs use exact integer minutes and exact integer minor / sub-minor currency units.
3. **Fail-Closed PII Boundary (`boundary.py`):**
   - Recursively refuses any payload containing marked PII fields (`national_id`, `phone`, `email`, `base_salary`, `hourly_overtime_rate`, `medical_note`, `gender`, `dob`, `bank_account`, etc.) with `NORN-SOLVER-PII-FORBIDDEN`.
4. **Hard Laws (`HR-SH-01..04` + statutory baselines):**
   - Exact shift coverage (`required_count`), role/skill and branch/unit eligibility, non-overlapping half-open UTC intervals `[starts_at, ends_at)`, minimum rest gap (`min_rest_minutes`, e.g. Egypt 11h = 660 min), maximum weekly hours (`max_weekly_minutes`, e.g. Egypt 48h = 2880 min), maximum daily hours (e.g. Egypt 10h = 600 min), maximum consecutive working days (e.g. 6), approved leave exclusion, and weekly overtime budget cap.
5. **Independent Hard-Law Post-Verifier (`verifier.py`):**
   - Independently verifies every feasible schedule emitted by CP-SAT against all hard laws without trusting solver internals; fails closed (`NORN-SOLVER-POSTVERIFY-FAILED`) on any violation.
6. **Multi-Repair Slack-Variable UNSAT Diagnostics (`solver.py`):**
   - When the primary hard-law CP-SAT model is `INFEASIBLE`, runs a deterministic secondary minimum-slack CP-SAT repair model with bounded integer slack variables on `relax_coverage`, `approve_overtime`, and `allow_transfer` (cross-branch and cross-skill) while keeping statutory rest, daily, consecutive-day, and approved-leave laws strictly enforced and post-verified.
7. **What-If Profile Comparisons (`compare_what_if`):**
   - Deterministically compares two `PreferenceProfile` runs on identical staffing inputs (changed assignments, overtime minutes/cost delta, weekend fairness spread delta, preference hits delta, objective score delta).
8. **Norn `perform` / `FunctionManifest` HTTP Worker (`worker.py`):**
   - Stdlib `ThreadingHTTPServer` exposing:
     - `GET /health`
     - `GET /manifest` (canonical `FunctionManifest` for `norn.solver.rostering@0.1.0`)
     - `POST /v1/functions/norn.solver.rostering/invoke`

## Running Locally

```bash
cd tools/norn-rostering-solver
uv sync
uv run pytest
uv run mypy --strict norn_rostering_solver tests
uv run ruff check norn_rostering_solver tests

# Run the HTTP worker
uv run norn-rostering-worker --host 127.0.0.1 --port 8091
```
