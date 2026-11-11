# Norn metrics

Every performance and quality number Norn has measured, one JSON record per line in
[`metrics/`](metrics), appended and never rewritten. Schema and tools:
`scripts/metrics/` on `main`. This page and [`index.html`](index.html) are regenerated
on every append.

1,922 records across 139 metrics (aliases folded, re-reports of one commit's value shown once). Each series runs in the order of the
measured commits' dates, then measurement time; the trend shows the last 40. *est.* marks an
estimate; runner `boat` series are the only ones the nightly ratchet gates.

## Boat usage

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `boat.libclang_step_s` | 58 | s (single) | boat | 1 | 58 / 58 | ▁ | `959458e889` | [link](https://github.com/Norn-cloud/norn/pull/1250) |
| `boat.starts.used_24h` | 514 | count (single) | local | 4 | 410 / 514 | ▁▁▇█ | none named | [link](https://github.com/Norn-cloud/norn/blob/main/scripts/metrics/boat_nightly.py) |

## Checker test suite

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `checker.test.cut2_four_serial_s` | 14.6 | s (total) | boat | 2 | 14.6 / 29 | █▁ | `7a58029d8e` | [link](https://github.com/Norn-cloud/norn/pull/1231) |
| `checker.test.cut_delta_s` | -30 *est.* | s (single) | boat | 2 | -30 / -5 | █▁ | `7a58029d8e` | [link](https://github.com/Norn-cloud/norn/pull/1231) |
| `checker.test.nextest_exec_s` | 62.5 | s (single) | boat | 3 | 61 / 67.4 | █▁▃ | `20e8feb4b0` | [link](https://github.com/Norn-cloud/norn/pull/1216) |

## Cell forks

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `fork.cow.create.f100.p50` | 0.3249 | ms (p50) | boat | 3 | 0.32 / 0.35 | ▁█▂ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.create.f100.p99` | 0.4791 | ms (p99) | boat | 3 | 0.4791 / 0.66 | █▄▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.create.f1000.p50` | 0.3116 | ms (p50) | boat | 2 | 0.31 / 0.3116 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.create.f1000.p99` | 0.4699 | ms (p99) | boat | 2 | 0.4699 / 2.1 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.create.f20.p50` | 0.332 | ms (p50) | boat | 3 | 0.332 / 0.41 | ▄█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.create.f20.p99` | 0.6948 | ms (p99) | boat | 3 | 0.6948 / 12.5 | █▅▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.collect_live.f100` | 10.63 | ms (single) | boat | 3 | 10.63 / 20 | ▄█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.collect_live.f1000` | 129.4 | ms (single) | boat | 2 | 129.4 / 134 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.collect_live.f20` | 2.635 | ms (single) | boat | 2 | 2.635 / 3.7 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.discard_collect.f100` | 2.695 | ms (single) | boat | 3 | 2.695 / 3.2 | ▄█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.discard_collect.f1000` | 12.24 | ms (single) | boat | 2 | 10.6 / 12.24 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.discard_collect.f20` | 1.834 | ms (single) | boat | 2 | 1.834 / 2 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.pages_freed.f100` | 931 | count (single) | boat | 3 | 931 / 931 | ▁▁▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.pages_freed.f1000` | 9,031 | count (single) | boat | 2 | 9,031 / 9,031 | ▁▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.gc.pages_freed.f20` | 211 | count (single) | boat | 2 | 211 / 211 | ▁▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.pages_per_fork.f100` | 9.31 | count (mean) | boat | 3 | 9.3 / 9.31 | ▁▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.pages_per_fork.f1000` | 9.031 | count (mean) | boat | 2 | 9 / 9.031 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.pages_per_fork.f20` | 10.55 | count (mean) | boat | 3 | 10.55 / 10.6 | ██▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f100.p50` | 10.44 | us (p50) | boat | 3 | 9.7 / 10.44 | ▁▄█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f100.p99` | 20.92 | us (p99) | boat | 2 | 20.92 / 26 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f1000.p50` | 10.37 | us (p50) | boat | 2 | 9.7 / 10.37 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f1000.p99` | 24.61 | us (p99) | boat | 2 | 23.7 / 24.61 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f20.p50` | 10.32 | us (p50) | boat | 3 | 9.7 / 10.32 | ▁▃█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.fork.f20.p99` | 20.36 | us (p99) | boat | 2 | 20.36 / 23.7 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f100.p50` | 9.86 | us (p50) | boat | 3 | 9.86 / 10.2 | ▂█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f100.p99` | 18.46 | us (p99) | boat | 2 | 18.46 / 27.7 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f1000.p50` | 10.26 | us (p50) | boat | 2 | 9.5 / 10.26 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f1000.p99` | 22.32 | us (p99) | boat | 2 | 16.5 / 22.32 | ▁█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f20.p50` | 9.89 | us (p50) | boat | 3 | 9.7 / 12.2 | ▁█▂ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.read.main.f20.p99` | 18.35 | us (p99) | boat | 2 | 18.35 / 24.8 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.storage_vs_full.f100` | 0.1934 | percent (single) | boat | 3 | 0.19 / 0.1934 | ▁▇█ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.storage_vs_full.f1000` | 0.1876 | percent (single) | boat | 2 | 0.1876 / 0.188 | █▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.cow.storage_vs_full.f20` | 0.2192 | percent (single) | boat | 3 | 0.219 / 0.22 | █▁▂ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.full_copy.create.f100.p50` | 4.971 | ms (p50) | boat | 3 | 4.971 / 14.2 | ▅█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.full_copy.create.f100.p99` | 15.01 | ms (p99) | boat | 3 | 15.01 / 1,630 | █▆▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.full_copy.create.f20.p50` | 3.145 | ms (p50) | boat | 3 | 3.145 / 4.6 | ▃█▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.full_copy.create.f20.p99` | 4.181 | ms (p99) | boat | 3 | 4.181 / 11.9 | █▄▁ | `8aca98c592` | [link](https://github.com/Norn-cloud/norn/pull/1226#issuecomment-6799825849) |
| `fork.proto.create.durable.f100.p50` | 45 | ms (p50) | boat | 1 | 45 / 45 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f100.p99` | 107 | ms (p99) | boat | 1 | 107 / 107 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f1000.p50` | 61 | ms (p50) | boat | 1 | 61 / 61 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f1000.p99` | 162 | ms (p99) | boat | 1 | 162 / 162 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f1000.total` | 3.9 | s (total) | boat | 1 | 3.9 / 3.9 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f20.p50` | 125 | ms (p50) | boat | 1 | 125 / 125 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.durable.f20.p99` | 126 | ms (p99) | boat | 1 | 126 / 126 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f100.p50` | 181 | ns (p50) | boat | 1 | 181 / 181 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f100.p99` | 1.9 | us (p99) | boat | 1 | 1.9 / 1.9 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f100.total` | 0.8 | ms (total) | boat | 1 | 0.8 / 0.8 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f1000.p50` | 872 | ns (p50) | boat | 1 | 872 / 872 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f1000.p99` | 92 | us (p99) | boat | 1 | 92 / 92 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f1000.total` | 0.89 | ms (total) | boat | 1 | 0.89 / 0.89 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f20.p50` | 180 | ns (p50) | boat | 1 | 180 / 180 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f20.p99` | 1.7 | us (p99) | boat | 1 | 1.7 / 1.7 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.create.mem.f20.total` | 0.41 | ms (total) | boat | 1 | 0.41 / 0.41 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.gc.discard_collect.f1000` | 43.7 | ms (single) | boat | 1 | 43.7 / 43.7 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.pages_per_fork.f100` | 28 | count (mean) | boat | 1 | 28 / 28 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.pages_per_fork.f1000` | 25.1 | count (mean) | boat | 1 | 25.1 / 25.1 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.pages_per_fork.f20` | 27 | count (mean) | boat | 1 | 27 / 27 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f100.p50` | 5 | us (p50) | boat | 1 | 5 / 5 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f100.p99` | 11.9 | us (p99) | boat | 1 | 11.9 / 11.9 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f1000.p50` | 4.5 | us (p50) | boat | 1 | 4.5 / 4.5 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f1000.p99` | 165 | us (p99) | boat | 1 | 165 / 165 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f20.p50` | 4.8 | us (p50) | boat | 1 | 4.8 / 4.8 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.fork.f20.p99` | 13.9 | us (p99) | boat | 1 | 13.9 / 13.9 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f100.p50` | 4.3 | us (p50) | boat | 1 | 4.3 / 4.3 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f100.p99` | 10 | us (p99) | boat | 1 | 10 / 10 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f1000.p50` | 3.8 | us (p50) | boat | 1 | 3.8 / 3.8 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f1000.p99` | 7.5 | us (p99) | boat | 1 | 7.5 / 7.5 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f20.p50` | 4.8 | us (p50) | boat | 1 | 4.8 / 4.8 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.read.main.f20.p99` | 8.5 | us (p99) | boat | 1 | 8.5 / 8.5 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.storage_vs_full.durable.f1000` | 0.036 | percent (single) | boat | 1 | 0.036 / 0.036 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.storage_vs_full.f100` | 0.135 | percent (single) | boat | 1 | 0.135 / 0.135 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.storage_vs_full.f1000` | 0.136 | percent (single) | boat | 1 | 0.136 / 0.136 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |
| `fork.proto.storage_vs_full.f20` | 0.136 | percent (single) | boat | 1 | 0.136 / 0.136 | ▁ | `05c8c6dbbb` | [link](https://github.com/Norn-cloud/norn-platform/issues/3148#issuecomment-6794396872) |

## Proof gates

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `proof.gate.checker-certificates-correspondence.s` | 177 | s (single) | boat | 1 | 177 / 177 | ▁ | `12bf2239d4` | [link](https://github.com/Norn-cloud/norn/pull/1232) |
| `proof.gate.checker_certificates.s` | 210 | s (single) | boat | 2 | 210 / 345 | █▁ | `bf2905c1cd` | [link](https://github.com/Norn-cloud/norn/pull/1234) |
| `proof.gate.exact_arith.s` | 1,920 | s (single) | boat | 5 | 647 / 1,920 | ▁▃▃▆█ | `ded6070871` | [link](https://github.com/Norn-cloud/norn/pull/1221) |
| `proof.gate.kernel_binding.s` | 236 | s (single) | boat | 1 | 236 / 236 | ▁ | `311735a1d0` | [link](https://github.com/Norn-cloud/norn/pull/1228#issuecomment-6800006111) |
| `proof.gate.kernel_commit.s` | 301 | s (single) | boat | 1 | 301 / 301 | ▁ | `311735a1d0` | [link](https://github.com/Norn-cloud/norn/pull/1228#issuecomment-6800006111) |
| `proof.gate.kernel_decider.s` | 214 | s (single) | boat | 1 | 214 / 214 | ▁ | `311735a1d0` | [link](https://github.com/Norn-cloud/norn/pull/1228#issuecomment-6800006111) |
| `proof.gate.kernel_ka.s` | 170 | s (single) | boat | 1 | 170 / 170 | ▁ | `c920cf7c87` | [link](https://github.com/Norn-cloud/norn/pull/1228) |
| `proof.gate.kernel_l1.s` | 9 | s (single) | boat | 1 | 9 / 9 | ▁ | `311735a1d0` | [link](https://github.com/Norn-cloud/norn/pull/1228#issuecomment-6800006111) |
| `proof.gate.law-exceptions-correspondence.s` | 153 | s (single) | boat | 1 | 153 / 153 | ▁ | `12bf2239d4` | [link](https://github.com/Norn-cloud/norn/pull/1232) |
| `proof.gate.norm-precedence-decision-correspondence.s` | 189 | s (single) | boat | 1 | 189 / 189 | ▁ | `12bf2239d4` | [link](https://github.com/Norn-cloud/norn/pull/1232) |
| `proof.gate.norm_precedence.s` | 202 | s (single) | boat | 1 | 202 / 202 | ▁ | `b5d3afdd03` | [link](https://github.com/Norn-cloud/norn/pull/1235) |
| `proof.gate.semantics-v1-lean.s` | 38 | s (single) | boat | 1 | 38 / 38 | ▁ | `12bf2239d4` | [link](https://github.com/Norn-cloud/norn/pull/1232) |
| `proof.gate.semantics_lean.s` | 229 | s (single) | boat | 2 | 229 / 505 | █▁ | none named | [link](https://github.com/Norn-cloud/norn/pull/1232) |
| `proof.verus.kernel_build.s` | 269 | s (single) | boat | 1 | 269 / 269 | ▁ | none named | [link](https://github.com/Norn-cloud/norn/issues/717#issuecomment-6795178248) |
| `proof.verus.kernel_l1_version.s` | 2 | s (single) | boat | 1 | 2 / 2 | ▁ | none named | [link](https://github.com/Norn-cloud/norn/issues/717#issuecomment-6795178248) |

## Deterministic simulation

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `sim.dst.do_glue_s` | 6.8 | s (single) | boat | 1 | 6.8 / 6.8 | ▁ | `73f3b52e1f` | [link](https://github.com/Norn-cloud/norn/pull/1223) |
| `sim.dst.request_layer_s` | 15.9 | s (single) | boat | 1 | 15.9 / 15.9 | ▁ | `73f3b52e1f` | [link](https://github.com/Norn-cloud/norn/pull/1223) |
| `sim.dst.serial_total_s` | 34.7 | s (total) | boat | 3 | 33.4 / 121 | ▁█▁ | `1d665da8fd` | [link](https://github.com/Norn-cloud/norn/pull/1223) |

## Test counts

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `tests.rust.count` | 3,592 | count (single) | boat, local | 16 | 1,990 / 3,592 | ▁▁▂▂▃▄▅▅▅▅▆▆▆▇▇█ | `a23725859a` | [link](https://github.com/Norn-cloud/norn/pull/1253) |
| `tests.rust.crate.norn-api.count` | 118 | count (single) | local | 14 | 9 / 118 | ▁▁▁▁▁▂▃▃▄▄▄▅▇█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-authz.count` | 52 | count (single) | local | 14 | 50 / 52 | ▁▁▁▁▁█████████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-builtins.count` | 14 | count (single) | local | 2 | 14 / 14 | ▁▁ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-canonical.count` | 34 | count (single) | local | 14 | 25 / 34 | ▁▆▆▆▆▆████████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-cell-do.count` | 65 | count (single) | local | 4 | 8 / 65 | ▁▄▅█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-cell.count` | 595 | count (single) | local | 14 | 337 / 595 | ▁▂▂▂▃▅▆▆▇▇▇▇██ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-checker-wasm.count` | 30 | count (single) | local | 14 | 19 / 30 | ▁▁▁▂▂▂▂▂▂▄████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-checker.count` | 1,327 | count (single) | local | 16 | 783 / 1,327 | ▁▂▃▃▄▄▆▆▆▇████▇█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-cli.count` | 13 | count (single) | local | 11 | 1 / 13 | ▁▁▂▂▂▂▆████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-datalog.count` | 13 | count (single) | local | 4 | 12 / 13 | ▁▁▁█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-dataplane.count` | 2 | count (single) | local | 11 | 2 / 2 | ▁▁▁▁▁▁▁▁▁▁▁ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-decisions.count` | 65 | count (single) | local | 14 | 29 / 65 | ▁▁▁▁██████████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-expr.count` | 159 | count (single) | local | 14 | 119 / 159 | ▁▃▃▃▆▆████████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-graph.count` | 25 | count (single) | local | 14 | 25 / 25 | ▁▁▁▁▁▁▁▁▁▁▁▁▁▁ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-ir.count` | 229 | count (single) | local | 14 | 169 / 229 | ▁▁▁▁▂▂▅▆▆▇████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-kernel.count` | 393 | count (single) | local | 14 | 274 / 393 | ▁▁▁▁▂▄▄▅▅▆▇███ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-projections.count` | 57 | count (single) | local | 14 | 54 / 57 | █▁▃▃▃▆████████ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-skeleton-guard.count` | 96 | count (single) | local | 14 | 74 / 111 | ▁▄▅▄▅▅████▅▅▅▅ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.crate.norn-testkit.count` | 15 | count (single) | local | 14 | 15 / 15 | ▁▁▁▁▁▁▁▁▁▁▁▁▁▁ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.norn_checker.count` | 1,283 | count (single) | boat | 2 | 1,283 / 1,318 | █▁ | `20e8feb4b0` | [link](https://github.com/Norn-cloud/norn/pull/1223) |
| `tests.rust.proptest_blocks.count` | 125 | count (single) | local | 14 | 8 / 125 | ▁▁▁▁▃▅▆▆▇▇▇███ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |
| `tests.rust.tokio.count` | 29 | count (single) | local | 14 | 26 / 29 | ▁▁▁▃▃▆▆▆▆▆▆▆▆█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/commit/6cbc5766c342dd26744106920bc3a38a3cacfe58) |

## CI task times (GitHub-hosted, never ratcheted)

| Metric | Latest | Unit (stat) | Runner | n | Min / max | Trend | Commit | Source |
|---|---:|---|---|---:|---|---|---|---|
| `ci.core.run.s` | 3,291 | s (single) | github-hosted | 170 | 35 / 9,710 | ▃▂▅▂▃▂▂▄▃▃▂▂▃▃▂▃▃▃▃▃▃▄▃▂▂▂▃▂▃▂▃▃▃▃▃▃▃▂▃▃ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.browser.s` | 272 | s (single) | github-hosted | 23 | 0 / 447 | ▁▁▁▄▆▆▄▅▅▇▆▇▆▆▆▅▆▅▇█▅▇▅ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.clippy.s` | 77 | s (single) | boat, github-hosted | 72 | 42 / 121 | ▆▁▅▆▄▃▃▃▃▂▂▄▄▃▃▄▃▂▁▃▁▄▃▃▅▂▄▄▄▃▄▂▃▃▅█▆▃▆▄ | `a23725859a` | [link](https://github.com/Norn-cloud/norn/pull/1253) |
| `ci.core.task.phase.doc.s` | 32 | s (single) | github-hosted | 71 | 16 / 73 | ▃▄▁▅▅▃▂▂▂▂▄▁▆▂▃▅▃▂▃▁▃▁▅▅▁▃▃▅▅▃▅▅▂▄▂▄█▃▅▃ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.doctest.s` | 21 | s (single) | github-hosted | 79 | 18 / 53 | █▂▃▃▂▂▁▂▂▁▃▄▂▁▂▃▁▃▂▃▁▂▂▂▁▁▃▁▁▃▃▃▂▄▃▄▂▂▃▂ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.features.s` | 667 | s (single) | github-hosted | 67 | 296 / 1,013 | ▅▄▃▄▆▆▃▇▆▆▆▆▄▆▆▃▃▇▆▄▄▆▇▅▆▄▅▇▅▅█▇▆▇▇█▇▆█▅ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.guards.s` | 590 | s (single) | github-hosted | 81 | 226 / 805 | ▇▄▆▆▆▆▆▅▆▆▄▄▆▆▆▆▄▅▅▅▆▅▆▅▆▅▅▆▅▅▇▆▆▇▇██▇▇▅ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.integration.s` | 642 | s (single) | github-hosted | 81 | 275 / 1,263 | ▃▃▃▂▂▁▃▂▃▂▂▂▃▂▃▃▂▃▂▄▂▂▇▇▃▃▇▅▄▄▇█▅▆▆▇█▄▇▄ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.provenance.s` | 14 | s (single) | github-hosted | 64 | 6 / 34 | ▃▆▄▄▃▄▆▃▄▅▃▄▅▃▅▂▂▃▃▂▂▃▂▃▃▂▃▂▂▃▃▂▁▃▁▂▃▃▃▃ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.test-shards.s` | 14 | s (single) | github-hosted | 63 | 6 / 14 | ▅▅▆▃▅▅▄▅▃▅▅▂▅▅▄▅▆▃▄▅▇▅▄▆▄▄▅▃▅▅▅▇▆▅▇▅▇▅██ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.phase.wasm.s` | 89 | s (single) | boat, github-hosted | 62 | 42 / 150 | ▂▂▂▃▄▃▆█▁▂▃▁▂▆▁▇▃▃▅▃▄▅▂▅▁▆▄▆▄▇▇▄▅▅▅▃▂█▃▄ | `a23725859a` | [link](https://github.com/Norn-cloud/norn/pull/1253) |
| `ci.core.task.test.heavy-1.s` | 90 | s (single) | github-hosted | 72 | 47 / 98 | ▇▁▄▄▇▇▇▄▄▂▂▇▇▇▄▅▇▁▁▄▄▁▄▄▂▇▂▄▇▇▅█▂▂▂▇▇▇▁▇ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.heavy-2.s` | 639 | s (single) | github-hosted | 83 | 303 / 650 | ▅▄▆▆▆▄▅▃▃▇▄▆▆▅▅▆▃▂▅▄▅▂▅▅▂▇▃▅▇▇▆▇▃▄▄███▃█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.heavy-3.s` | 160 | s (single) | github-hosted | 75 | 89 / 176 | ▇▁▅▅▇▇▇▅▅▂▂▇▇▇▅▅▇▁▁▅▅▁▅▅▂▇▁▅▇▆▅▆▂▂▂▇█▇▁▇ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.heavy-4.s` | 99 | s (single) | github-hosted | 73 | 49 / 102 | ▇▁▅▅█▇▇▆▅▂▃█▇█▅▆▇▂▁▅▅▁▅▅▂█▁▅▇▇▇▆▂▂▂███▁█ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.heavy-5.s` | 68 | s (single) | github-hosted | 71 | 32 / 71 | ▇▇▁▄▄██▇▅▄▂▃▇▇▇▄▆▇▁▂▄▁▄▄▂█▂▄▇▇▆▇▂▂▂█▇█▂▇ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.heavy-6.s` | 108 | s (single) | github-hosted | 63 | 35 / 109 | ▅▄▆▁▆▅▃▆▂▅▅▂▅▄▄▆█▃▄▅█▅▃█▃▂▅▂▆▅▅█▆▆▅▅▆▂██ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.rest-1.s` | 1,166 | s (single) | github-hosted | 71 | 453 / 1,166 | ▅▅▄▅▃▆▅▂▅▄▄▇▇▃▅▇▅▇▄▃▇▃▇█▃▃▆▃▅▅▇█▇█▆▅▇▃██ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.rest-2.s` | 607 | s (single) | github-hosted | 64 | 132 / 695 | ▅▄▅▂▅▆▄▅▃▆▆▃▆▅▄▇█▃▅▅█▅▄█▄▃▆▃▆▅▅▇▆▆▅▅▆▃▇▇ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |
| `ci.core.task.test.rest-3.s` | 376 | s (single) | github-hosted | 68 | 97 / 376 | ▄▁▅▅▃▅▃▅▅▃▅▅▄▆▆▃▄▅▇▅▇▃▇▇▃▃▅▃▅▆▆▇▇▇▆▆▇▃██ | `6cbc5766c3` | [link](https://github.com/Norn-cloud/norn/actions/runs/43024711474) |

## Latest caveats

What limits each metric's latest value, in the measurer's words.

- `boat.libclang_step_s`: one run on a default Boat VM from snapshot norn-ci-main-a8c384a14d-v1; includes apt-get update over the network; reused runners skip it (0 s)
- `boat.starts.used_24h`: Account-wide: every project on this Boat account, not only Norn. Rolling 24-hour window from `boat limits --json`, read before this run's own start. Not tied to a commit.
- `checker.test.cut2_four_serial_s`: The four tests cut 2 changed most, run serially on one VM. measured_at is when the PR body stated it. The head sha is #1231's head; the body does not restate it.
- `checker.test.cut_delta_s`: Author's net estimate: the serial four-test delta (14.4 s) plus two tests from a parallel run that was about 16% slower. Whole-suite wall time is unusable for this: untouched tests varied by up to 41 s (incremental 68 s, then 109 s).
- `checker.test.nextest_exec_s`: Noisy: about half the crate's test time is two long property tests (incremental about 52–55 s, three_systems about 9–11 s) that the cut does not touch. 4 tests fail on base and head alike (they need CI's prepare_inputs). measured_at is when the PR body stated it.
- `fork.cow.create.f100.p50`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.create.f100.p99`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.create.f1000.p50`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.create.f1000.p99`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.create.f20.p50`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.create.f20.p99`: In-memory tier; one run. Fork latency is opening/configuring the fork's SQLite connection and cloning store caches; the page-store root copy is O(1). p99 is a CPU tail on 4 vCPU.
- `fork.cow.gc.collect_live.f100`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.collect_live.f1000`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.collect_live.f20`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.discard_collect.f100`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.discard_collect.f1000`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.discard_collect.f20`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.pages_freed.f100`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.pages_freed.f1000`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.gc.pages_freed.f20`: In-memory mark-and-sweep over live roots; one run; marking skips shared subtrees, so cost scales with distinct pages.
- `fork.cow.pages_per_fork.f100`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.cow.pages_per_fork.f1000`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.cow.pages_per_fork.f20`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.cow.read.fork.f100.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.fork.f100.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.fork.f1000.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.fork.f1000.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.fork.f20.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.fork.f20.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f100.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f100.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f1000.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f1000.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f20.p50`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.read.main.f20.p99`: In-memory tier, all pages resident: CPU-bound point loads (2,000 per sample set); one run. Cold reads from object storage not measured.
- `fork.cow.storage_vs_full.f100`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.cow.storage_vs_full.f1000`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.cow.storage_vs_full.f20`: In-memory tier; one run. ~1.8-2.1 pages per commit (fact table, index, ledger, receipt, header) plus ~1 page per fork for its pin record; forks write distinct values so dedupe cannot hide growth.
- `fork.full_copy.create.f100.p50`: Copying reference engine (SQLite online backup of an 18.8 MiB in-memory cell); one run; full-copy p99 is sensitive to memory pressure.
- `fork.full_copy.create.f100.p99`: Copying reference engine (SQLite online backup of an 18.8 MiB in-memory cell); one run; full-copy p99 is sensitive to memory pressure.
- `fork.full_copy.create.f20.p50`: Copying reference engine (SQLite online backup of an 18.8 MiB in-memory cell); one run; full-copy p99 is sensitive to memory pressure.
- `fork.full_copy.create.f20.p99`: Copying reference engine (SQLite online backup of an 18.8 MiB in-memory cell); one run; full-copy p99 is sensitive to memory pressure.
- `fork.proto.create.durable.f100.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f100.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f1000.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f1000.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f1000.total`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f20.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.durable.f20.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by this VM disk's fdatasync (36–126 ms p50 by probe): 'Durable fork latency on this VM is the fsync floor, not the design.' One sync covers about 16 concurrent forks.
- `fork.proto.create.mem.f100.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f100.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f100.total`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only. Total time to create all forks.
- `fork.proto.create.mem.f1000.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f1000.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f1000.total`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only. Total time to create all forks.
- `fork.proto.create.mem.f20.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f20.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only.
- `fork.proto.create.mem.f20.total`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Limited by one registry RwLock, 16 threads on 4 cores (sharding removes it). In-memory fork metadata only. Total time to create all forks.
- `fork.proto.gc.discard_collect.f1000`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. Mark-and-sweep by reachability, O(sum of root sizes); freed 238 MiB. No per-count GC values were given for 20 or 100 forks (only a 13–46 ms range).
- `fork.proto.pages_per_fork.f100`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Max 30 pages.
- `fork.proto.pages_per_fork.f1000`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Max 28 pages.
- `fork.proto.pages_per_fork.f20`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Max 27 pages.
- `fork.proto.read.fork.f100.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.fork.f100.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.fork.f1000.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured. The p99 at 1,000 forks is a CPU tail from 1,000 open connections on 4 vCPUs.
- `fork.proto.read.fork.f1000.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured. The p99 at 1,000 forks is a CPU tail from 1,000 open connections on 4 vCPUs.
- `fork.proto.read.fork.f20.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.fork.f20.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.main.f100.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.main.f100.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.main.f1000.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured. The p99 at 1,000 forks is a CPU tail from 1,000 open connections on 4 vCPUs.
- `fork.proto.read.main.f1000.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured. The p99 at 1,000 forks is a CPU tail from 1,000 open connections on 4 vCPUs.
- `fork.proto.read.main.f20.p50`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.read.main.f20.p99`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. 2 MiB SQLite cache, every page resident: the CPU path only; cold reads from object storage not measured.
- `fork.proto.storage_vs_full.durable.f1000`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Root log grew 0.44 MiB (about 450 B per fork).
- `fork.proto.storage_vs_full.f100`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Total store growth against N full copies.
- `fork.proto.storage_vs_full.f1000`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Total store growth against N full copies.
- `fork.proto.storage_vs_full.f20`: D81 prototype on a synthetic cell-shaped database, not the real CellStore. measured_at is when the result was posted; the run time was not stated. The 4 KiB page is the unit: a small change costs one page per B-tree touched (table, index, ledger, plus page 1), about 1.3–2 pages per commit after sharing. Total store growth against N full copies.
- `proof.gate.checker-certificates-correspondence.s`: one run, wall clock incl. Mathlib/Aeneas olean import and every mutant re-extraction; same VM ran the four gates sequentially; Lean import is IO-bound on 8 GB (about 40-100 s per process)
- `proof.gate.checker_certificates.s`: With 17 native_decide refutation vectors added, yet faster than the 345 s run; no explanation given. measured_at is when the PR body stated it. Cold or warm tool caches not stated. One run.
- `proof.gate.exact_arith.s`: single sample; gate script alone (timed in-wrapper); mutants run serially at ~70 s each (0-160 s), ~250 s fixed setup+extraction+proofs, so wall time is linear in mutant count; same host class as the 1596 s record
- `proof.gate.kernel_binding.s`: one sample; warm snapshot (Aeneas lake build cached); gates run sequentially on one VM sharing page cache; mutant loop dominates
- `proof.gate.kernel_commit.s`: one sample; warm snapshot (Aeneas lake build cached); gates run sequentially on one VM sharing page cache; mutant loop dominates
- `proof.gate.kernel_decider.s`: one sample; warm snapshot (Aeneas lake build cached); gates run sequentially on one VM sharing page cache; mutant loop dominates
- `proof.gate.kernel_ka.s`: K-A gate on the tree of this head. measured_at is the head's commit time; the run time was not stated. No durations were stated for the K-B and K-C gates. Cold or warm tool caches not stated. One run.
- `proof.gate.kernel_l1.s`: one sample; warm snapshot (Aeneas lake build cached); gates run sequentially on one VM sharing page cache; mutant loop dominates
- `proof.gate.law-exceptions-correspondence.s`: one run, wall clock incl. Mathlib/Aeneas olean import and every mutant re-extraction; same VM ran the four gates sequentially; Lean import is IO-bound on 8 GB (about 40-100 s per process)
- `proof.gate.norm-precedence-decision-correspondence.s`: one run, wall clock incl. Mathlib/Aeneas olean import and every mutant re-extraction; same VM ran the four gates sequentially; Lean import is IO-bound on 8 GB (about 40-100 s per process)
- `proof.gate.norm_precedence.s`: The source states 3m22s. measured_at is when the PR body stated it. Cold or warm tool caches not stated. One run.
- `proof.gate.semantics-v1-lean.s`: one run, wall clock incl. Mathlib/Aeneas olean import and every mutant re-extraction; same VM ran the four gates sequentially; Lean import is IO-bound on 8 GB (about 40-100 s per process)
- `proof.gate.semantics_lean.s`: U0 NornSem Lean gate. No commit stated for this run; measured_at is when the PR body stated it. Cold or warm tool caches not stated. One run.
- `proof.verus.kernel_build.s`: Build only: the crate verifies nothing until it opts in. The source states 4 min 29 s. No commit stated; measured_at is when the comment was posted. One run.
- `proof.verus.kernel_l1_version.s`: One small file (1 verified item). No commit stated; measured_at is when the comment was posted. One run.
- `sim.dst.do_glue_s`: The plain DO glue law test (64 cases), from the serial run of the set. One run.
- `sim.dst.request_layer_s`: The DO request-layer simulator test, from the serial run of the set. One run.
- `sim.dst.serial_total_s`: Same serial set on the combined frozen head, which includes 23 newer main commits (one changes norn-cell-do/src/server.rs). VM type not restated for this run. One run; measured_at is when the PR body stated it.
- `tests.rust.count`: core.sh test all on the D82 train tree; 9 skipped not counted
- `tests.rust.crate.norn-api.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-authz.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-builtins.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-canonical.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-cell-do.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-cell.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-checker-wasm.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-checker.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-cli.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-datalog.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-dataplane.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-decisions.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-expr.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-graph.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-ir.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-kernel.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-projections.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-skeleton-guard.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.crate.norn-testkit.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.norn_checker.count`: same method as at 5327e2d084
- `tests.rust.proptest_blocks.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
- `tests.rust.tokio.count`: Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] and cfg-gated tests are not told apart. Machine-independent.
