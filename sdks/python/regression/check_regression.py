# %% [markdown]
# # One-click regression assumptions
#
# Pick a dataset in the cell (an object type), name the target and the
# features, and Norn runs the applied-regression assumption suite: missing
# values, leakage and constant features, linearity (Ramsey RESET), normality
# of residuals (Jarque-Bera, Shapiro-Wilk), homoscedasticity (Breusch-Pagan,
# White), independence (Durbin-Watson, when there is an order field),
# multicollinearity (VIF, condition number) and influence (Cook's distance,
# leverage, studentized residuals). Each check says pass, warn or fail, with
# its statistic, the threshold and where the threshold comes from, and what to
# do about it.
#
# The results are written back into the cell, through governed actions, as a
# `stats.ModelSpec` and its `stats.AssumptionCheck` rows: they have receipts,
# Studio shows them, and rerunning on the same dataset revision gives the same
# results and the same receipts.
#
#     python check_regression.py /path/to/project

# %%
import sys

import pandas as pd

from norn_regression import Cell, check_regression

project = sys.argv[1] if len(sys.argv) > 1 else "."
cell = Cell.from_project(project)

# %% [markdown]
# ## Run the suite on `lab.Sample`: target `y`, features `x1 x2 x3`, ordered by `t`

# %%
result = check_regression("lab.Sample", target="y", features=["x1", "x2", "x3"], order="t", cell=cell)
print(f"{result.spec['objectType']} at revision {result.spec['datasetRevision'][:12]}: {result.status} "
      f"({result.spec['rowsUsed']} of {result.spec['rows']} rows used; spec {result.spec['spec']})")
pd.set_option("display.width", 200)
pd.set_option("display.max_colwidth", 60)
table = result.frame()[["assumption", "test", "subject", "status", "statistic", "p_value", "threshold"]]
print(table.to_string(index=False))

# %% [markdown]
# ## What to do about the failures

# %%
for check in result.checks:
    if check.status in ("fail", "warn"):
        print(f"[{check.status}] {check.test} ({check.subject}): {check.remediation}  [threshold: {check.threshold}; {check.threshold_source}]")

# %% [markdown]
# ## Residuals for plotting
#
# `result.plots` holds the fitted values, residuals and Q-Q points (they are
# not written to the cell). With matplotlib:
#
#     plt.scatter(result.plots["fitted"], result.plots["residuals"], s=4)
#     plt.scatter(result.plots["qq"]["theoretical"], result.plots["qq"]["sample"], s=4)
