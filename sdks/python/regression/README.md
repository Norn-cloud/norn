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

**Files**
- `check_regression.py`: the notebook cell (`# %%`).
- `synthetic.py`: a seeded dataset with known violations: heteroscedastic, AR(1) errors in `t`, `x3` collinear with `x1` and `x2`.
- `test_suite.py`: the suite against known violations, without a cell.
- `run-demo.sh WORKDIR`: generates the client, runs the tests, serves `definitions/` with `norn dev`, seeds 2,000 samples, runs the notebook and `e2e_check.py`.

**Limits.**
- The model is ordinary least squares with an intercept. Rows with a missing value are dropped.
- Fitted values and residuals are returned for plotting, not stored in the cell.
- Absent values are recorded as "": `orderField` when no order field is named, `pValue` for checks without one. The profile has no nullable action inputs.
