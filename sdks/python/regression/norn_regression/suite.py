"""The applied-regression assumption suite, as a pure function of a dataframe.

`assumption_suite(frame, target, features, order=None)` fits ordinary least
squares (statsmodels) of `target` on `features` with an intercept and returns
one `Check` per test: the statistic, the p-value when the test has one, the
threshold the status was decided by and where that threshold comes from, and
a short remediation hint. Nothing here reads or writes a cell, so the same
frame always gives the same checks (rows are sorted by `id` first).

Statuses: `pass`, `warn`, `fail`, and `not_applicable` when a test cannot run
on this data (for example Durbin-Watson without an order field).
"""

from __future__ import annotations

import math
import warnings
from dataclasses import asdict, dataclass

import numpy as np
import pandas as pd
import scipy
import statsmodels
import statsmodels.api as sm
from scipy import stats as sstats
from statsmodels.stats.diagnostic import het_breuschpagan, het_white, linear_reset
from statsmodels.stats.outliers_influence import OLSInfluence, variance_inflation_factor
from statsmodels.stats.stattools import durbin_watson, jarque_bera
from statsmodels.tools.sm_exceptions import SingularMatrixWarning

SUITE_VERSION = "1"
# Shapiro-Wilk's p-value is accurate up to n = 5000 (scipy's documented limit).
SHAPIRO_MAX_N = 5000


@dataclass(frozen=True)
class Check:
    assumption: str  # missingness | sanity | linearity | normality | homoscedasticity | independence | multicollinearity | influence
    test: str
    subject: str  # a field name, or "model"
    status: str  # pass | warn | fail | not_applicable
    statistic: str
    p_value: str | None
    threshold: str
    threshold_source: str
    remediation: str

    def as_dict(self) -> dict:
        return asdict(self)


def number(value: float) -> str:
    """A statistic as stable text: six significant digits, `inf` and `nan` spelled out."""
    if value is None:
        return "n/a"
    value = float(value)
    if math.isnan(value):
        return "nan"
    if math.isinf(value):
        return "inf" if value > 0 else "-inf"
    return f"{value:.6g}"


def by_p(p: float, fail: float, warn: float) -> str:
    return "fail" if p < fail else "warn" if p < warn else "pass"


def environment() -> str:
    return f"OLS with intercept; statsmodels {statsmodels.__version__}, scipy {scipy.__version__}, numpy {np.__version__}; suite {SUITE_VERSION}"


def _numeric(frame: pd.DataFrame, column: str) -> pd.Series:
    return pd.to_numeric(frame[column], errors="coerce")


def missingness(frame: pd.DataFrame, fields: list[str]) -> list[Check]:
    checks = []
    for field in fields:
        share = float(_numeric(frame, field).isna().mean()) if len(frame) else 1.0
        status = "fail" if share > 0.20 else "warn" if share > 0.05 else "pass"
        checks.append(Check(
            "missingness", "share of missing values", field, status, number(share), None,
            "warn > 0.05, fail > 0.20",
            "Schafer (1999): above 5% missing, listwise deletion can bias estimates",
            "Rows with a missing value are dropped before fitting; impute (multiple imputation) or model the missingness instead.",
        ))
    return checks


def sanity(frame: pd.DataFrame, target: str, features: list[str]) -> list[Check]:
    checks = []
    y = frame[target]
    for feature in features:
        x = frame[feature]
        values = x.value_counts(normalize=True)
        if x.nunique() <= 1:
            checks.append(Check("sanity", "constant feature", feature, "fail", "1 distinct value", None,
                                "fail when the feature has one value", "a constant column is collinear with the intercept",
                                "Drop the feature: it carries no information and makes the design singular."))
        elif float(values.iloc[0]) > 0.99:
            checks.append(Check("sanity", "near-constant feature", feature, "warn", number(values.iloc[0]), None,
                                "warn when one value covers > 99% of rows", "rule of thumb (near-zero variance; Kuhn & Johnson 2013)",
                                "Consider dropping the feature or pooling rare values."))
        else:
            checks.append(Check("sanity", "constant feature", feature, "pass", f"{x.nunique()} distinct values", None,
                                "fail when the feature has one value", "a constant column is collinear with the intercept", "None needed."))
        same = bool(np.allclose(x.to_numpy(), y.to_numpy(), rtol=0, atol=0))
        corr = float(np.corrcoef(x, y)[0, 1]) if x.std() > 0 and y.std() > 0 else 0.0
        status = "fail" if same or abs(corr) >= 0.99 else "warn" if abs(corr) >= 0.95 else "pass"
        checks.append(Check("sanity", "target leakage (|correlation| with the target)", feature, status,
                            "identical to the target" if same else number(corr), None,
                            "warn >= 0.95, fail >= 0.99 or identical", "rule of thumb for leakage screening",
                            "A feature this close to the target is usually the target in disguise (computed from it or recorded after it): remove it." if status != "pass" else "None needed."))
    return checks


def _vif(design: pd.DataFrame, features: list[str]) -> list[Check]:
    checks = []
    exog = design.to_numpy()
    for index, feature in enumerate(features, start=1):  # column 0 is the intercept
        with np.errstate(divide="ignore", invalid="ignore"):
            vif = float(variance_inflation_factor(exog, index))
        if math.isnan(vif):
            vif = math.inf
        status = "fail" if vif > 10 else "warn" if vif > 5 else "pass"
        checks.append(Check("multicollinearity", "variance inflation factor", feature, status, number(vif), None,
                            "warn > 5, fail > 10", "Kutner, Nachtsheim & Neter (2004), Applied Linear Regression Models",
                            "Drop or combine collinear features (sum, ratio, principal component), or use ridge regression." if status != "pass" else "None needed."))
    return checks


def _condition(frame: pd.DataFrame, features: list[str]) -> Check:
    # Belsley-Kuh-Welsch: columns scaled to unit length, intercept included.
    design = sm.add_constant(frame[features].to_numpy(dtype=float), has_constant="add")
    scaled = design / np.linalg.norm(design, axis=0)
    singular = np.linalg.svd(scaled, compute_uv=False)
    kappa = float(singular[0] / singular[-1]) if singular[-1] > 0 else math.inf
    status = "fail" if kappa > 30 else "warn" if kappa > 10 else "pass"
    return Check("multicollinearity", "condition number (scaled design)", "model", status, number(kappa), None,
                 "warn > 10, fail > 30", "Belsley, Kuh & Welsch (1980), Regression Diagnostics",
                 "Near-dependencies among features: drop or combine features, or centre them." if status != "pass" else "None needed.")


def _influence(fit, n: int, p: int) -> list[Check]:
    influence = OLSInfluence(fit)
    cooks = np.asarray(influence.cooks_distance[0])
    leverage = np.asarray(influence.hat_matrix_diag)
    student = np.asarray(influence.resid_studentized_external)
    over_cook = int((cooks > 4 / n).sum())
    max_cook = float(cooks.max())
    cook_status = "fail" if max_cook > 1 else "warn" if over_cook > 0.05 * n else "pass"
    high_leverage = int((leverage > 2 * p / n).sum())
    leverage_status = "warn" if high_leverage > 0.05 * n else "pass"
    outliers = int((np.abs(student) > 3).sum())
    outlier_status = "fail" if np.abs(student).max() > 5 else "warn" if outliers > 0.01 * n else "pass"
    return [
        Check("influence", "Cook's distance", "model", cook_status, f"max {number(max_cook)}; {over_cook} rows > 4/n", None,
              "fail if any > 1; warn if more than 5% of rows > 4/n", "Cook & Weisberg (1982); 4/n cut-off, Bollen & Jackman (1985)",
              "Inspect the influential rows: data errors, or a subgroup the model misses; refit without them to see how much they move the coefficients." if cook_status != "pass" else "None needed."),
        Check("influence", "leverage (hat values)", "model", leverage_status, f"{high_leverage} rows > 2p/n", None,
              "warn if more than 5% of rows have leverage > 2p/n", "Hoaglin & Welsch (1978)",
              "High-leverage rows sit far from the bulk of the features: check them, or transform skewed features." if leverage_status != "pass" else "None needed."),
        Check("influence", "externally studentized residuals", "model", outlier_status,
              f"max |r| {number(np.abs(student).max())}; {outliers} rows |r| > 3", None,
              "fail if any |r| > 5; warn if more than 1% of rows |r| > 3", "about 0.3% of normal residuals exceed 3 in magnitude",
              "Outliers in the target: check the rows, or use a robust regression (Huber, M-estimation)." if outlier_status != "pass" else "None needed."),
    ]


def assumption_suite(frame: pd.DataFrame, target: str, features: list[str], order: str | None = None) -> tuple[list[Check], dict]:
    """The checks, and the fitted model's data for plots (fitted, residuals, Q-Q points).
    A rank-deficient or ill-conditioned design is reported by the multicollinearity
    checks, so statsmodels' warnings about it are not repeated."""
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", SingularMatrixWarning)
        warnings.filterwarnings("ignore", message="The design matrix is poorly conditioned")
        return _suite(frame, target, features, order)


def _suite(frame: pd.DataFrame, target: str, features: list[str], order: str | None) -> tuple[list[Check], dict]:
    if not features:
        raise ValueError("at least one feature is needed")
    if target in features:
        raise ValueError(f"{target} is both the target and a feature")
    frame = frame.sort_values("id", kind="mergesort").reset_index(drop=True) if "id" in frame else frame
    fields = [target, *features]
    checks = missingness(frame, fields)
    data = pd.DataFrame({field: _numeric(frame, field) for field in fields})
    if order is not None:
        # Numeric when it parses (a CSV upload stores text), otherwise its own order (timestamps sort as text).
        numeric = _numeric(frame, order)
        data[order] = numeric if numeric.notna().all() else frame[order]
    data = data.dropna(subset=fields).reset_index(drop=True)
    n, p = len(data), len(features) + 1
    if n <= p + 1:
        raise ValueError(f"{n} complete rows cannot fit {len(features)} features")
    checks += sanity(data, target, features)
    usable = [f for f in features if data[f].nunique() > 1]
    design = sm.add_constant(data[usable].astype(float), has_constant="add")
    fit = sm.OLS(data[target].astype(float), design).fit()
    residuals = np.asarray(fit.resid)

    # Linearity: Ramsey's RESET (powers 2 and 3 of the fitted values).
    reset = linear_reset(fit, power=3, use_f=True)
    checks.append(Check("linearity", "Ramsey RESET (powers 2-3 of fitted, F)", "model", by_p(float(reset.pvalue), 0.01, 0.05),
                        number(float(reset.fvalue)), number(float(reset.pvalue)), "fail p < 0.01, warn p < 0.05", "Ramsey (1969)",
                        "The mean is not linear in the features: add a transform (log, polynomial or spline term) or an interaction." if reset.pvalue < 0.05 else "None needed."))

    # Normality of residuals.
    jb, jb_p, skew, kurtosis = jarque_bera(residuals)
    checks.append(Check("normality", "Jarque-Bera", "residuals", by_p(float(jb_p), 0.001, 0.05), number(jb), number(jb_p),
                        "fail p < 0.001, warn p < 0.05", "Jarque & Bera (1987)",
                        f"Residuals are not normal (skew {number(skew)}, kurtosis {number(kurtosis)}): inference on coefficients relies on large-sample approximations; transform the target (log, Box-Cox) or bootstrap the intervals." if jb_p < 0.05 else "None needed."))
    if n <= SHAPIRO_MAX_N:
        sw, sw_p = sstats.shapiro(residuals)
        checks.append(Check("normality", "Shapiro-Wilk", "residuals", by_p(float(sw_p), 0.001, 0.05), number(sw), number(sw_p),
                            "fail p < 0.001, warn p < 0.05", "Shapiro & Wilk (1965)",
                            "As for Jarque-Bera: transform the target or bootstrap." if sw_p < 0.05 else "None needed."))
    else:
        checks.append(Check("normality", "Shapiro-Wilk", "residuals", "not_applicable", "n/a", None,
                            f"runs for n <= {SHAPIRO_MAX_N}", "scipy.stats.shapiro: p-values are approximate above n = 5000",
                            "Jarque-Bera covers large samples."))

    # Homoscedasticity.
    bp, bp_p, _, _ = het_breuschpagan(residuals, design)
    checks.append(Check("homoscedasticity", "Breusch-Pagan (LM)", "model", by_p(float(bp_p), 0.01, 0.05), number(bp), number(bp_p),
                        "fail p < 0.01, warn p < 0.05", "Breusch & Pagan (1979)",
                        "The error variance depends on the features: use heteroscedasticity-robust standard errors (HC3), weighted least squares, or a log target." if bp_p < 0.05 else "None needed."))
    if len(usable) <= 10:
        white, white_p, _, _ = het_white(residuals, design)
        checks.append(Check("homoscedasticity", "White (LM)", "model", by_p(float(white_p), 0.01, 0.05), number(white), number(white_p),
                            "fail p < 0.01, warn p < 0.05", "White (1980)",
                            "As for Breusch-Pagan: robust (HC3) standard errors or WLS." if white_p < 0.05 else "None needed."))

    # Independence: Durbin-Watson needs the order the errors could be correlated in.
    if order is None:
        checks.append(Check("independence", "Durbin-Watson", "residuals", "not_applicable", "n/a", None,
                            "needs a time or order field", "Durbin & Watson (1950): defined for ordered observations",
                            "Name the time or order field (order=...) to test serial correlation."))
    else:
        ordered = residuals[np.argsort(data[order].to_numpy(), kind="mergesort")]
        dw = float(durbin_watson(ordered))
        status = "fail" if dw < 1.0 or dw > 3.0 else "warn" if dw < 1.5 or dw > 2.5 else "pass"
        checks.append(Check("independence", f"Durbin-Watson (ordered by {order})", "residuals", status, number(dw), None,
                            "pass 1.5-2.5, fail < 1 or > 3", "Durbin & Watson (1950); common rule of thumb",
                            "Residuals are serially correlated: use Newey-West (HAC) standard errors, add lagged terms, or model the time structure." if status != "pass" else "None needed."))

    # Multicollinearity.
    checks += _vif(design, usable)
    checks.append(_condition(data, usable))

    # Influence and outliers.
    checks += _influence(fit, n, len(usable) + 1)

    fitted = np.asarray(fit.fittedvalues)
    theoretical = sstats.norm.ppf((np.arange(1, n + 1) - 0.5) / n)
    plots = {"fitted": fitted, "residuals": residuals, "qq": {"theoretical": theoretical, "sample": np.sort(residuals)},
             "n": n, "coefficients": {name: float(value) for name, value in fit.params.items()}}
    return checks, plots


def summary(checks: list[Check]) -> str:
    statuses = {check.status for check in checks}
    return "fail" if "fail" in statuses else "warn" if "warn" in statuses else "pass"
