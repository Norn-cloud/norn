"""Seed `lab.Sample` with the synthetic dataset, through the governed action route."""
import sys

from norn_regression import Cell
from synthetic import dataset

cell = Cell.from_project(sys.argv[1])
n = int(sys.argv[2]) if len(sys.argv) > 2 else 2000
for row in dataset(n).itertuples():
    cell.call("lab.sample.add", {"sample": row.sample, "t": int(row.t), "x1": f"{row.x1:.4f}", "x2": f"{row.x2:.4f}",
                                  "x3": f"{row.x3:.4f}", "y": f"{row.y:.4f}"}, f"seed-{row.sample}")
print(f"seeded {n} lab.Sample objects")
