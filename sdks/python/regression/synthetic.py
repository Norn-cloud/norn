"""A seeded dataset with known violations, for the tests and the demo.

`y = 3 + 2 x1 - x2 + e`, where the error's spread grows with `x1`
(heteroscedastic), the errors follow an AR(1) process in `t` (serially
correlated), and `x3 = x1 + x2 + small noise` (collinear with both).
"""

from __future__ import annotations

import numpy as np
import pandas as pd


def dataset(n: int = 2000, seed: int = 20261115) -> pd.DataFrame:
    rng = np.random.default_rng(seed)
    t = np.arange(n)
    x1 = rng.uniform(0, 10, n)
    x2 = rng.normal(5, 2, n)
    x3 = x1 + x2 + rng.normal(0, 0.05, n)
    shocks = rng.normal(0, 1, n) * (0.2 + 0.4 * x1)
    errors = np.empty(n)
    errors[0] = shocks[0]
    for i in range(1, n):
        errors[i] = 0.8 * errors[i - 1] + shocks[i]
    y = 3 + 2 * x1 - x2 + errors
    frame = pd.DataFrame({"sample": [f"s{i:05d}" for i in t], "t": t, "x1": x1, "x2": x2, "x3": x3, "y": y})
    # Four-decimal values, as the cell stores them (Norn.decimal(4)).
    for column in ("x1", "x2", "x3", "y"):
        frame[column] = frame[column].round(4)
    frame["id"] = frame["sample"]
    return frame
