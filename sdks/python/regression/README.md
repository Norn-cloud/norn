# One-click regression assumptions

`norn_regression.check_regression(type, target, features, cell=..., order=None)`
reads an object type from a cell through the generated client (only the
objects the caller may read), runs the applied-regression assumption suite
with statsmodels and scipy (pinned in `requirements.txt`), and records the
results in the cell through the governed actions in
`definitions/regression.ts`: one `stats.ModelSpec` (type, target, features,
order field, dataset revision, rows used, library versions) and one
`stats.AssumptionCheck` per check. Each check has a status, the statistic, the
p-value when the test has one, the threshold and its source, and a remediation
hint.

| Assumption | Tests |
|---|---|
| missing values | share missing per field |
| sanity | constant and near-constant features; leakage (a feature identical or nearly identical to the target) |
| linearity | Ramsey RESET |
| normality of residuals | Jarque-Bera; Shapiro-Wilk (n ≤ 5000); Q-Q points in `result.plots` |
| homoscedasticity | Breusch-Pagan; White |
| independence | Durbin-Watson, when an order field is named (`order=`); otherwise `not_applicable` |
| multicollinearity | VIF per feature; condition number of the scaled design |
| influence | Cook's distance; leverage; externally studentized residuals |

**Reproducible.** The dataset revision is a hash of every row's id and
version. The spec id hashes the type, target, features, order field, suite
version and revision, and every action call's idempotency key derives from it.
Rerunning on the same revision replays the same calls and gets the same
receipts; a write to the dataset records a new spec beside the old one.

**Hosted: one click in Studio.** Studio's button calls
`stats.checkrequest.submit` and creates a `stats.CheckRequest` with status
`queued`. The worker (`python -m norn_regression.worker`, image from
`Dockerfile`) runs next to the hosted cell. It reaches the cell through
Studio's proxy (`NORN_URL=https://<studio>/api/host`) with a Studio API key
(`NORN_TOKEN_FILE`, sent as `x-api-key`), polls every 5 s, runs the queued
requests, and reports `running`, then `done` with the spec id or `failed` with
the reason, through `stats.checkrequest.finish`. The queue is the cell; every
step has a receipt. Run one worker per cell. `stats.regression.access` grants
role `member` the four stats actions.

The same function stays callable from Python. It works on any object type
whose fields hold numbers or numeric text, such as a dataset uploaded as CSV.

**Files**
- `check_regression.py`: the notebook cell (`# %%`).
- `synthetic.py`: a seeded dataset with known violations: heteroscedastic, AR(1) errors in `t`, `x3` collinear with `x1` and `x2`.
- `test_suite.py`: the suite against known violations, without a cell.
- `run-demo.sh WORKDIR`: generates the client, runs the tests, serves `definitions/` with `norn dev`, seeds 2,000 samples, and runs the notebook and `e2e_check.py`. The end-to-end check includes the worker running one queued request and failing one with its reason.
- `norn_regression/worker.py` and `Dockerfile`: the hosted job runner.

**Limits.**
- The model is ordinary least squares with an intercept. Rows with a missing value are dropped.
- Fitted values and residuals are returned for plotting, not stored in the cell.
- Absent values are recorded as "": `order_field` when no order field is named, `p_value` for checks without one. The profile has no nullable action inputs.
