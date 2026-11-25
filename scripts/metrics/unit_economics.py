#!/usr/bin/env python3
"""Translate measured Norn performance metrics into transparent unit-economics estimates.

Reads metric records (from `--jsonl` files or a git revision such as `origin/metrics`
or `origin/metrics:metrics/2026-11.jsonl`) alongside explicit, dated infrastructure
prices (`pricing.json`), and emits machine-readable JSON, CSV (ready for macro.com
FP&A import), or Markdown.

Missing inputs are never fabricated: any derived metric whose required measurements
are absent is emitted with `value = null` and an explicit `missing_inputs` list.
Retired host series (`host.celld.*`, `host.orange.*`) never populate live Rivet rows;
if present, `host.celld.pss_per_cell.bytes` is emitted only in a separate, explicitly
labeled `historical_proxy` reference section so FP&A models cannot confuse it with live
Rivet capacity.

    python3 scripts/metrics/unit_economics.py --format md
    python3 scripts/metrics/unit_economics.py --format csv --out unit-economics.csv
    python3 scripts/metrics/unit_economics.py --jsonl metrics.jsonl --format json

Standard library only.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import math
import sys
from pathlib import Path


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


HERE = Path(__file__).resolve().parent
DEFAULT_PRICING = HERE / "pricing.json"
EVIDENCE_LABEL = "derived-estimate"
HISTORICAL_PROXY_EVIDENCE = "historical-proxy-retired-celld"
BYTES_PER_GIB = 1024 ** 3
BYTES_PER_GB = 1_000_000_000
ONE_MILLION = 1_000_000.0


def load_pricing(path: Path = DEFAULT_PRICING) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def load_records(*, jsonl_paths: list[str] | None = None, repo: Path | None = None,
                 rev: str = "origin/metrics") -> tuple[list[dict], str]:
    store = _sibling("record")
    if jsonl_paths:
        records: list[dict] = []
        for item in jsonl_paths:
            records.extend(store.read_jsonl(item))
        return records, ", ".join(jsonl_paths)
    repo_path = repo or HERE.parents[1]
    if ":" in rev:
        blob = store.git(repo_path, "cat-file", "blob", rev).stdout
        return store.parse_shard(rev, blob), rev
    files = store.read_store(repo_path, rev)
    return store.all_records(files), rev


def latest_by_metric(records: list[dict]) -> dict[str, dict]:
    """Fold aliases and supersessions, returning the latest record per canonical metric id."""
    store = _sibling("record")
    latest: dict[str, dict] = {}
    for row in sorted(store.fold(records), key=store.series_key):
        latest[row["metric"]] = row
    return latest


def _is_finite_number(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _require_positive(value, label: str) -> float:
    if not _is_finite_number(value) or float(value) <= 0.0:
        raise ValueError(f"pricing parameter `{label}` must be a finite number > 0, got {value!r}")
    return float(value)


def _require_non_negative(value, label: str) -> float:
    if not _is_finite_number(value) or float(value) < 0.0:
        raise ValueError(f"pricing parameter `{label}` must be a finite number >= 0, got {value!r}")
    return float(value)


def _validate_pricing(pricing: dict) -> dict[str, float]:
    cpx = pricing["compute"]["hetzner_cpx62"]
    return {
        "hours_per_month": _require_positive(pricing["hours_per_month"], "hours_per_month"),
        "cpx_vcpu": _require_positive(cpx["vcpu"], "compute.hetzner_cpx62.vcpu"),
        "cpx_ram_gib": _require_positive(cpx["ram_gib"], "compute.hetzner_cpx62.ram_gib"),
        "cpx_nvme_gb": _require_positive(cpx["nvme_gb"], "compute.hetzner_cpx62.nvme_gb"),
        "cpx_monthly_usd": _require_non_negative(cpx["monthly_usd"], "compute.hetzner_cpx62.monthly_usd"),
        "vol_usd_per_gb_mo": _require_non_negative(
            pricing["storage"]["hetzner_volume_usd_per_gb_month"]["value"],
            "storage.hetzner_volume_usd_per_gb_month",
        ),
        "r2_usd_per_gb_mo": _require_non_negative(
            pricing["storage"]["cloudflare_r2_storage_usd_per_gb_month"]["value"],
            "storage.cloudflare_r2_storage_usd_per_gb_month",
        ),
        "cdn_usd_per_gb": _require_non_negative(
            pricing["bandwidth"]["cdn_egress_usd_per_gb"]["value"],
            "bandwidth.cdn_egress_usd_per_gb",
        ),
        "boat_usd_per_hr": _require_non_negative(
            pricing["ci"]["boat_vm_usd_per_hour"]["value"],
            "ci.boat_vm_usd_per_hour",
        ),
        "ci_parallel_vms": _require_positive(
            pricing["ci"]["effective_parallel_vms"]["value"],
            "ci.effective_parallel_vms",
        ),
        "cpu_target": _require_positive(
            pricing["utilization"]["cpu_target"]["value"],
            "utilization.cpu_target",
        ),
        "ram_ceiling": _require_positive(
            pricing["utilization"]["ram_ceiling"]["value"],
            "utilization.ram_ceiling",
        ),
        "disk_ceiling": _require_positive(
            pricing["utilization"]["disk_ceiling"]["value"],
            "utilization.disk_ceiling",
        ),
        "ha_factor": _require_positive(
            pricing["utilization"]["ha_replication_factor"]["value"],
            "utilization.ha_replication_factor",
        ),
        "tenant_commits_mo": _require_positive(
            pricing["workload_assumptions"]["active_tenant_commits_per_month"]["value"],
            "workload_assumptions.active_tenant_commits_per_month",
        ),
        "peak_to_avg": _require_positive(
            pricing["workload_assumptions"]["peak_to_average_cpu_ratio"]["value"],
            "workload_assumptions.peak_to_average_cpu_ratio",
        ),
        "retention_months": _require_positive(
            pricing["workload_assumptions"]["hot_cell_retention_months"]["value"],
            "workload_assumptions.hot_cell_retention_months",
        ),
    }


def _as_of_for_key(pricing: dict, key: str) -> str:
    """Resolve the `as_of` date of a dotted pricing key (or its enclosing dated object)."""
    node = pricing
    last_date: str | None = None
    for part in key.split("."):
        if isinstance(node, dict) and isinstance(node.get("as_of"), str):
            last_date = node["as_of"]
        if isinstance(node, dict) and part in node:
            node = node[part]
        else:
            break
    if isinstance(node, dict) and isinstance(node.get("as_of"), str):
        last_date = node["as_of"]
    return last_date or "unknown"


def _row_pricing_dates(pricing: dict, pricing_keys: list[str]) -> tuple[str, dict[str, str]]:
    by_key = {key: _as_of_for_key(pricing, key) for key in pricing_keys}
    unique = sorted(set(by_key.values()))
    summary = ", ".join(unique) if unique else "unknown"
    return summary, by_key


def _input_ref(row: dict) -> dict:
    return {
        "metric": row["metric"],
        "value": row["value"],
        "unit": row["unit"],
        "statistic": row["statistic"],
        "measured_at": row["measured_at"],
        "commit": row["commit"]["sha"],
        "runner": row["env"]["runner"],
    }


def _first_present(latest: dict[str, dict], candidates: tuple[str, ...], *,
                   allow_zero: bool = False) -> dict | None:
    for name in candidates:
        row = latest.get(name)
        if row is None:
            continue
        val = row.get("value")
        if not _is_finite_number(val):
            continue
        if (val >= 0.0) if allow_zero else (val > 0.0):
            return row
    return None


def _positive_workload_field(row: dict, key: str) -> float | None:
    val = row.get("workload", {}).get(key)
    if _is_finite_number(val) and float(val) > 0.0:
        return float(val)
    return None


def _round_sig(value: float, digits: int = 6) -> float:
    if value == 0:
        return 0.0
    return float(f"{value:.{digits}g}")


def _row(pricing: dict, metric_id: str, category: str, value, unit: str, formula: str,
         inputs: list[dict], missing_inputs: list[str], pricing_keys: list[str],
         notes: str = "", evidence: str = EVIDENCE_LABEL) -> dict:
    as_of_summary, as_of_by_key = _row_pricing_dates(pricing, pricing_keys)
    return {
        "id": metric_id,
        "category": category,
        "value": _round_sig(value) if isinstance(value, float) else value,
        "unit": unit,
        "evidence": evidence,
        "formula": formula,
        "inputs": inputs,
        "missing_inputs": missing_inputs,
        "pricing_assumptions": pricing_keys,
        "pricing_as_of": as_of_summary,
        "pricing_as_of_by_key": as_of_by_key,
        "notes": notes,
    }


def _resolve_lake_rows_per_s(latest: dict[str, dict]) -> tuple[float | None, float | None, dict | None, str, list[str]]:
    """Resolve lake ingest throughput and positive thread count without fabricating missing workload metadata."""
    direct = _first_present(latest, ("lake.ingest.throughput.rows_s",))
    if direct is not None:
        threads = _positive_workload_field(direct, "threads")
        if threads is not None:
            return float(direct["value"]), threads, direct, "lake.ingest.throughput.rows_s", []
        return None, None, None, "lake.ingest.throughput.rows_s", ["lake.ingest.throughput.rows_s(workload.threads>0)"]
    for metric_id in ("analytics.duckdb.load_parquet_1m.s", "analytics.duckdb.load_csv_1m.s"):
        duck = _first_present(latest, (metric_id,))
        if duck is None:
            continue
        rows = _positive_workload_field(duck, "rows")
        threads = _positive_workload_field(duck, "threads")
        if rows is not None and threads is not None:
            return rows / float(duck["value"]), threads, duck, f"{int(rows):,} / {duck['metric']}", []
    return None, None, None, "lake.ingest.throughput.rows_s", ["lake.ingest.throughput.rows_s"]


def compute_unit_economics(records: list[dict], pricing: dict, *, source: str = "records") -> dict:
    """Compute all derived unit-economics estimates from `records` and `pricing`."""
    store = _sibling("record")
    p = _validate_pricing(pricing)
    latest = latest_by_metric(records)

    seconds_per_month = p["hours_per_month"] * 3600.0
    cpx_vcpu = p["cpx_vcpu"]
    cpx_ram_gib = p["cpx_ram_gib"]
    cpx_nvme_gb = p["cpx_nvme_gb"]
    cpx_monthly_usd = p["cpx_monthly_usd"]
    cpx_usd_per_vcpu_s = cpx_monthly_usd / (cpx_vcpu * seconds_per_month)

    vol_usd_per_gb_mo = p["vol_usd_per_gb_mo"]
    r2_usd_per_gb_mo = p["r2_usd_per_gb_mo"]
    cdn_usd_per_gb = p["cdn_usd_per_gb"]
    boat_usd_per_hr = p["boat_usd_per_hr"]
    ci_parallel_vms = p["ci_parallel_vms"]

    cpu_target = p["cpu_target"]
    ram_ceiling = p["ram_ceiling"]
    disk_ceiling = p["disk_ceiling"]
    ha_factor = p["ha_factor"]

    tenant_commits_mo = p["tenant_commits_mo"]
    peak_to_avg = p["peak_to_avg"]
    retention_months = p["retention_months"]

    usable_ram_bytes = cpx_ram_gib * BYTES_PER_GIB * ram_ceiling
    usable_vcpu = cpx_vcpu * cpu_target
    usable_nvme_bytes = cpx_nvme_gb * BYTES_PER_GB * disk_ceiling
    tenant_peak_cmd_s = (tenant_commits_mo / seconds_per_month) * peak_to_avg

    rows: list[dict] = []

    # 1. Compute & tenant density (live Rivet / core engine series only — never retired celld/orange)
    mem_row = _first_present(latest, ("host.rivet.pss_per_cell.bytes", "cell.memory.rss_per_cell.bytes"))
    if mem_row is not None:
        tenants_ram = math.floor(usable_ram_bytes / float(mem_row["value"]))
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_ram", "compute", tenants_ram, "tenants",
            f"floor(({cpx_ram_gib} GiB * {ram_ceiling}) / {mem_row['metric']})",
            [_input_ref(mem_row)], [],
            ["compute.hetzner_cpx62.ram_gib", "utilization.ram_ceiling"],
            notes="Resident memory bound per cpx62 node before HA replication.",
        ))
    else:
        tenants_ram = None
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_ram", "compute", None, "tenants",
            f"floor(({cpx_ram_gib} GiB * {ram_ceiling}) / cell.memory.rss_per_cell.bytes)",
            [], ["cell.memory.rss_per_cell.bytes"],
            ["compute.hetzner_cpx62.ram_gib", "utilization.ram_ceiling"],
        ))

    commit_row = _first_present(latest, ("host.rivet.commit.throughput.cmd_s", "cell.commit.throughput.cmd_s"))
    if commit_row is not None:
        commit_cmd_s = float(commit_row["value"])
        node_cmd_capacity = usable_vcpu * commit_cmd_s
        tenants_cpu = math.floor(node_cmd_capacity / tenant_peak_cmd_s)
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_cpu", "compute", tenants_cpu, "tenants",
            f"floor(({cpx_vcpu} vCPU * {cpu_target} * {commit_row['metric']}) / "
            f"({int(tenant_commits_mo):,}/mo / {int(seconds_per_month):,}s * {peak_to_avg}x peak))",
            [_input_ref(commit_row)], [],
            [
                "compute.hetzner_cpx62.vcpu", "utilization.cpu_target",
                "workload_assumptions.active_tenant_commits_per_month",
                "workload_assumptions.peak_to_average_cpu_ratio",
            ],
            notes="Assumes single-cell throughput scales linearly across effective vCPUs for independent tenant cells.",
        ))
    else:
        commit_cmd_s = None
        tenants_cpu = None
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_cpu", "compute", None, "tenants",
            f"floor(({cpx_vcpu} vCPU * {cpu_target} * cell.commit.throughput.cmd_s) / tenant_peak_cmd_s)",
            [], ["cell.commit.throughput.cmd_s"],
            [
                "compute.hetzner_cpx62.vcpu", "utilization.cpu_target",
                "workload_assumptions.active_tenant_commits_per_month",
                "workload_assumptions.peak_to_average_cpu_ratio",
            ],
        ))

    commit_size_row = _first_present(latest, ("cell.commit.size.bytes",))
    if commit_size_row is not None:
        tenant_hot_bytes = float(commit_size_row["value"]) * tenant_commits_mo * retention_months
        tenants_disk = math.floor(usable_nvme_bytes / tenant_hot_bytes)
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_disk", "compute", tenants_disk, "tenants",
            f"floor(({cpx_nvme_gb} GB * {disk_ceiling}) / "
            f"(cell.commit.size.bytes * {int(tenant_commits_mo):,} * {retention_months} mo))",
            [_input_ref(commit_size_row)], [],
            [
                "compute.hetzner_cpx62.nvme_gb", "utilization.disk_ceiling",
                "workload_assumptions.active_tenant_commits_per_month",
                "workload_assumptions.hot_cell_retention_months",
            ],
            notes="Local NVMe bound per cpx62 node assuming hot_cell_retention_months of commits retained hot.",
        ))
    else:
        tenants_disk = None
        rows.append(_row(
            pricing, "tenants_per_cpx62_by_disk", "compute", None, "tenants",
            f"floor(({cpx_nvme_gb} GB * {disk_ceiling}) / "
            f"(cell.commit.size.bytes * {int(tenant_commits_mo):,} * {retention_months} mo))",
            [], ["cell.commit.size.bytes"],
            [
                "compute.hetzner_cpx62.nvme_gb", "utilization.disk_ceiling",
                "workload_assumptions.active_tenant_commits_per_month",
                "workload_assumptions.hot_cell_retention_months",
            ],
        ))

    binding_missing: list[str] = []
    binding_inputs: list[dict] = []
    if tenants_ram is None:
        binding_missing.append("cell.memory.rss_per_cell.bytes")
    elif mem_row is not None:
        binding_inputs.append(_input_ref(mem_row))
    if tenants_cpu is None:
        binding_missing.append("cell.commit.throughput.cmd_s")
    elif commit_row is not None:
        binding_inputs.append(_input_ref(commit_row))
    if tenants_disk is None:
        binding_missing.append("cell.commit.size.bytes")
    elif commit_size_row is not None:
        binding_inputs.append(_input_ref(commit_size_row))

    binding_pricing_keys = [
        "compute.hetzner_cpx62",
        "utilization.ram_ceiling",
        "utilization.cpu_target",
        "utilization.disk_ceiling",
        "workload_assumptions.active_tenant_commits_per_month",
        "workload_assumptions.peak_to_average_cpu_ratio",
        "workload_assumptions.hot_cell_retention_months",
    ]
    if not binding_missing and tenants_ram is not None and tenants_cpu is not None and tenants_disk is not None:
        binding = min(("ram", tenants_ram), ("cpu", tenants_cpu), ("disk", tenants_disk), key=lambda pair: pair[1])[0]
        rows.append(_row(
            pricing, "binding_constraint", "compute", binding, "axis",
            "argmin(tenants_per_cpx62_by_ram, tenants_per_cpx62_by_cpu, tenants_per_cpx62_by_disk)",
            binding_inputs, [], binding_pricing_keys,
        ))
    else:
        rows.append(_row(
            pricing, "binding_constraint", "compute", None, "axis",
            "argmin(tenants_per_cpx62_by_ram, tenants_per_cpx62_by_cpu, tenants_per_cpx62_by_disk)",
            binding_inputs, binding_missing, binding_pricing_keys,
            notes="Requires RAM, CPU, and disk density inputs to identify the true binding resource without guessing.",
        ))

    compute_missing: list[str] = []
    compute_inputs: list[dict] = []
    if tenants_ram is None:
        compute_missing.append("cell.memory.rss_per_cell.bytes")
    elif mem_row is not None:
        compute_inputs.append(_input_ref(mem_row))
    if tenants_cpu is None:
        compute_missing.append("cell.commit.throughput.cmd_s")
    elif commit_row is not None:
        compute_inputs.append(_input_ref(commit_row))

    compute_tenant_pricing_keys = [
        "compute.hetzner_cpx62.monthly_usd",
        "utilization.ha_replication_factor",
        "utilization.ram_ceiling",
        "utilization.cpu_target",
        "workload_assumptions.active_tenant_commits_per_month",
        "workload_assumptions.peak_to_average_cpu_ratio",
    ]
    if not compute_missing and tenants_ram is not None and tenants_cpu is not None:
        tenants_compute = min(tenants_ram, tenants_cpu)
        if tenants_compute > 0:
            usd_tenant_compute = (cpx_monthly_usd * ha_factor) / tenants_compute
            rows.append(_row(
                pricing, "usd_per_active_tenant_month_compute", "compute", usd_tenant_compute, "USD/tenant-month",
                f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA) / min(tenants_per_cpx62_by_ram, tenants_per_cpx62_by_cpu)",
                compute_inputs, [], compute_tenant_pricing_keys,
                notes=f"Compute-bound tenant density = {tenants_compute:,} tenants per cpx62 node (before disk check).",
            ))
        else:
            rows.append(_row(
                pricing, "usd_per_active_tenant_month_compute", "compute", None, "USD/tenant-month",
                f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA) / min(tenants_per_cpx62_by_ram, tenants_per_cpx62_by_cpu)",
                compute_inputs, [], compute_tenant_pricing_keys,
                notes="Single tenant exceeds one cpx62 node's compute/RAM ceiling (density = 0 tenants/node).",
            ))
    else:
        rows.append(_row(
            pricing, "usd_per_active_tenant_month_compute", "compute", None, "USD/tenant-month",
            f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA) / min(tenants_per_cpx62_by_ram, tenants_per_cpx62_by_cpu)",
            compute_inputs, compute_missing, compute_tenant_pricing_keys,
        ))

    if commit_cmd_s is not None and commit_row is not None:
        effective_node_cmd_s = usable_vcpu * commit_cmd_s
        usd_per_s_ha_node = (cpx_monthly_usd * ha_factor) / seconds_per_month
        usd_1m_commits_compute = (ONE_MILLION / effective_node_cmd_s) * usd_per_s_ha_node
        rows.append(_row(
            pricing, "usd_per_1m_commits_compute", "compute", usd_1m_commits_compute, "USD/1M-commits",
            f"(1e6 / ({cpx_vcpu} vCPU * {cpu_target} * {commit_row['metric']})) * "
            f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA / {int(seconds_per_month):,}s)",
            [_input_ref(commit_row)], [],
            ["compute.hetzner_cpx62", "utilization.cpu_target", "utilization.ha_replication_factor"],
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_1m_commits_compute", "compute", None, "USD/1M-commits",
            f"(1e6 / ({cpx_vcpu} vCPU * {cpu_target} * cell.commit.throughput.cmd_s)) * "
            f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA / {int(seconds_per_month):,}s)",
            [], ["cell.commit.throughput.cmd_s"],
            ["compute.hetzner_cpx62", "utilization.cpu_target", "utilization.ha_replication_factor"],
        ))

    decision_row = _first_present(latest, ("decision.eval.throughput.ops_s",))
    if decision_row is not None:
        effective_dec_s = usable_vcpu * float(decision_row["value"])
        usd_per_s_ha_node = (cpx_monthly_usd * ha_factor) / seconds_per_month
        usd_1m_decisions = (ONE_MILLION / effective_dec_s) * usd_per_s_ha_node
        rows.append(_row(
            pricing, "usd_per_1m_decisions_compute", "compute", usd_1m_decisions, "USD/1M-decisions",
            f"(1e6 / ({cpx_vcpu} vCPU * {cpu_target} * decision.eval.throughput.ops_s)) * "
            f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA / {int(seconds_per_month):,}s)",
            [_input_ref(decision_row)], [],
            ["compute.hetzner_cpx62", "utilization.cpu_target", "utilization.ha_replication_factor"],
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_1m_decisions_compute", "compute", None, "USD/1M-decisions",
            f"(1e6 / ({cpx_vcpu} vCPU * {cpu_target} * decision.eval.throughput.ops_s)) * "
            f"(${cpx_monthly_usd}/mo * {ha_factor:g}x HA / {int(seconds_per_month):,}s)",
            [], ["decision.eval.throughput.ops_s"],
            ["compute.hetzner_cpx62", "utilization.cpu_target", "utilization.ha_replication_factor"],
        ))

    # 2. Storage (hot cell vs cold lake)
    if commit_size_row is not None:
        gb_per_1m_hot = (float(commit_size_row["value"]) * ONE_MILLION) / BYTES_PER_GB
        usd_1m_hot_mo = gb_per_1m_hot * ha_factor * vol_usd_per_gb_mo
        rows.append(_row(
            pricing, "usd_per_1m_commits_hot_cell_month", "storage", usd_1m_hot_mo, "USD/1M-commits-month",
            f"(cell.commit.size.bytes * 1e6 / 1e9) * {ha_factor:g}x HA * ${vol_usd_per_gb_mo}/GB-mo",
            [_input_ref(commit_size_row)], [],
            ["storage.hetzner_volume_usd_per_gb_month", "utilization.ha_replication_factor"],
        ))
    else:
        usd_1m_hot_mo = None
        rows.append(_row(
            pricing, "usd_per_1m_commits_hot_cell_month", "storage", None, "USD/1M-commits-month",
            f"(cell.commit.size.bytes * 1e6 / 1e9) * {ha_factor:g}x HA * ${vol_usd_per_gb_mo}/GB-mo",
            [], ["cell.commit.size.bytes"],
            ["storage.hetzner_volume_usd_per_gb_month", "utilization.ha_replication_factor"],
        ))

    parquet_size_row = _first_present(
        latest, ("lake.parquet.commit_size.bytes", "analytics.parquet.commit_size.bytes")
    )
    if parquet_size_row is not None:
        gb_per_1m_cold = (float(parquet_size_row["value"]) * ONE_MILLION) / BYTES_PER_GB
        usd_1m_cold_mo = gb_per_1m_cold * r2_usd_per_gb_mo
        rows.append(_row(
            pricing, "usd_per_1m_commits_cold_lake_month", "storage", usd_1m_cold_mo, "USD/1M-commits-month",
            f"({parquet_size_row['metric']} * 1e6 / 1e9) * ${r2_usd_per_gb_mo}/GB-mo",
            [_input_ref(parquet_size_row)], [],
            ["storage.cloudflare_r2_storage_usd_per_gb_month"],
        ))
    else:
        usd_1m_cold_mo = None
        rows.append(_row(
            pricing, "usd_per_1m_commits_cold_lake_month", "storage", None, "USD/1M-commits-month",
            f"(lake.parquet.commit_size.bytes * 1e6 / 1e9) * ${r2_usd_per_gb_mo}/GB-mo",
            [], ["lake.parquet.commit_size.bytes"],
            ["storage.cloudflare_r2_storage_usd_per_gb_month"],
        ))

    ratio_missing: list[str] = []
    ratio_inputs: list[dict] = []
    if commit_size_row is None:
        ratio_missing.append("cell.commit.size.bytes")
    else:
        ratio_inputs.append(_input_ref(commit_size_row))
    if parquet_size_row is None:
        ratio_missing.append("lake.parquet.commit_size.bytes")
    else:
        ratio_inputs.append(_input_ref(parquet_size_row))

    ratio_pricing_keys = [
        "storage.hetzner_volume_usd_per_gb_month",
        "storage.cloudflare_r2_storage_usd_per_gb_month",
        "utilization.ha_replication_factor",
    ]
    if usd_1m_hot_mo is not None and usd_1m_cold_mo is not None and usd_1m_cold_mo > 0:
        rows.append(_row(
            pricing, "hot_to_cold_cost_ratio", "storage", usd_1m_hot_mo / usd_1m_cold_mo, "ratio",
            "usd_per_1m_commits_hot_cell_month / usd_per_1m_commits_cold_lake_month",
            ratio_inputs, [], ratio_pricing_keys,
        ))
    elif usd_1m_hot_mo is not None and usd_1m_cold_mo is not None:
        rows.append(_row(
            pricing, "hot_to_cold_cost_ratio", "storage", None, "ratio",
            "usd_per_1m_commits_hot_cell_month / usd_per_1m_commits_cold_lake_month",
            ratio_inputs, [], ratio_pricing_keys,
            notes="cold storage rate is $0; ratio undefined",
        ))
    else:
        rows.append(_row(
            pricing, "hot_to_cold_cost_ratio", "storage", None, "ratio",
            "usd_per_1m_commits_hot_cell_month / usd_per_1m_commits_cold_lake_month",
            ratio_inputs, ratio_missing, ratio_pricing_keys,
        ))

    if usd_1m_hot_mo is not None and commit_size_row is not None:
        usd_100k_storage = usd_1m_hot_mo * (100_000.0 / ONE_MILLION)
        rows.append(_row(
            pricing, "usd_per_100k_cmd_tenant_month_storage", "storage", usd_100k_storage, "USD/tenant-month",
            "usd_per_1m_commits_hot_cell_month * (100,000 / 1,000,000)",
            [_input_ref(commit_size_row)], [],
            ["storage.hetzner_volume_usd_per_gb_month", "utilization.ha_replication_factor"],
            notes=f"Monthly hot-cell storage cost for 100k commits ({ha_factor:g}x HA Hetzner Volume).",
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_100k_cmd_tenant_month_storage", "storage", None, "USD/tenant-month",
            "usd_per_1m_commits_hot_cell_month * (100,000 / 1,000,000)",
            [], ["cell.commit.size.bytes"],
            ["storage.hetzner_volume_usd_per_gb_month", "utilization.ha_replication_factor"],
        ))

    # 3. Analytics / Lake ingest
    rows_per_s, ingest_threads, ingest_row, ingest_expr, ingest_missing = _resolve_lake_rows_per_s(latest)
    if rows_per_s is not None and ingest_threads is not None and ingest_row is not None:
        sec_per_1m_rows = ONE_MILLION / rows_per_s
        usd_1m_rows = sec_per_1m_rows * (ingest_threads / cpu_target) * cpx_usd_per_vcpu_s
        rows.append(_row(
            pricing, "usd_per_1m_rows_landed", "analytics", usd_1m_rows, "USD/1M-rows",
            f"(1e6 / ({ingest_expr})) * ({ingest_threads:g} vCPU / {cpu_target}) * "
            f"(${cpx_monthly_usd}/mo / ({cpx_vcpu} vCPU * {int(seconds_per_month):,}s))",
            [_input_ref(ingest_row)], [],
            ["compute.hetzner_cpx62", "utilization.cpu_target"],
            notes="Compute cost to ingest/land 1M rows at measured DuckDB/DataFusion throughput, scaled by CPU headroom.",
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_1m_rows_landed", "analytics", None, "USD/1M-rows",
            "(1e6 / lake.ingest.throughput.rows_s) * (threads / cpu_target) * cpx62_usd_per_vcpu_s",
            [], ingest_missing,
            ["compute.hetzner_cpx62", "utilization.cpu_target"],
        ))

    # 4. Bandwidth / Frontend (initial cold-load entry assets from dist/index.html; 0 CSS bytes is valid!)
    js_gz = _first_present(latest, ("studio.bundle.initial_js.gzip_bytes",), allow_zero=False)
    css_gz = _first_present(latest, ("studio.bundle.initial_css.gzip_bytes",), allow_zero=True)
    bw_missing: list[str] = []
    bw_inputs: list[dict] = []
    if js_gz is None:
        bw_missing.append("studio.bundle.initial_js.gzip_bytes")
    else:
        bw_inputs.append(_input_ref(js_gz))
    if css_gz is None:
        bw_missing.append("studio.bundle.initial_css.gzip_bytes")
    else:
        bw_inputs.append(_input_ref(css_gz))

    if not bw_missing and js_gz is not None and css_gz is not None:
        transfer_gb = ((float(js_gz["value"]) + float(css_gz["value"])) * 10_000.0) / BYTES_PER_GB
        usd_10k_loads = transfer_gb * cdn_usd_per_gb
        rows.append(_row(
            pricing, "usd_per_10k_studio_loads", "bandwidth", usd_10k_loads, "USD/10k-loads",
            f"((studio.bundle.initial_js.gzip_bytes + studio.bundle.initial_css.gzip_bytes) * 10,000 / 1e9) * ${cdn_usd_per_gb}/GB",
            bw_inputs, [],
            ["bandwidth.cdn_egress_usd_per_gb"],
            notes=(
                f"{transfer_gb:.3f} GB transferred per 10k cold Studio initial loads (dist/index.html entry JS+CSS only); "
                "$0.00 on Cloudflare Pages/R2 zero-egress."
            ),
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_10k_studio_loads", "bandwidth", None, "USD/10k-loads",
            f"((studio.bundle.initial_js.gzip_bytes + studio.bundle.initial_css.gzip_bytes) * 10,000 / 1e9) * ${cdn_usd_per_gb}/GB",
            bw_inputs, bw_missing,
            ["bandwidth.cdn_egress_usd_per_gb"],
            notes="Uses dist/index.html initial entry JS+CSS gzip transfer (never total dist/ with lazy chunks/widgets).",
        ))

    # 5. Engineering / CI
    ci_row = _first_present(latest, ("ci.core.run.s",))
    if ci_row is not None:
        vm_hours = (float(ci_row["value"]) / 3600.0) * ci_parallel_vms
        usd_pr_ci = vm_hours * boat_usd_per_hr
        rows.append(_row(
            pricing, "usd_per_pr_ci_estimate", "ci", usd_pr_ci, "USD/PR",
            f"(ci.core.run.s / 3600) * {ci_parallel_vms:g} VMs * ${boat_usd_per_hr}/VM-hr",
            [_input_ref(ci_row)], [],
            ["ci.boat_vm_usd_per_hour", "ci.effective_parallel_vms"],
            notes=f"Estimated {vm_hours:.2f} VM-hours per full ci.core run using Boat reference rate (#1533).",
        ))
    else:
        rows.append(_row(
            pricing, "usd_per_pr_ci_estimate", "ci", None, "USD/PR",
            f"(ci.core.run.s / 3600) * {ci_parallel_vms:g} VMs * ${boat_usd_per_hr}/VM-hr",
            [], ["ci.core.run.s"],
            ["ci.boat_vm_usd_per_hour", "ci.effective_parallel_vms"],
        ))

    # 6. Historical proxy reference (retired hosts only — strictly separated from live Rivet rows)
    historical_proxies: list[dict] = []
    celld_pss = _first_present(latest, ("host.celld.pss_per_cell.bytes",))
    if celld_pss is not None:
        celld_ram_tenants = math.floor(usable_ram_bytes / float(celld_pss["value"]))
        historical_proxies.append(_row(
            pricing, "retired_celld_reference_tenants_per_cpx62_by_ram", "historical_proxy",
            celld_ram_tenants, "tenants",
            f"floor(({cpx_ram_gib} GiB * {ram_ceiling}) / host.celld.pss_per_cell.bytes)",
            [_input_ref(celld_pss)], [],
            ["compute.hetzner_cpx62.ram_gib", "utilization.ram_ceiling"],
            notes="RETIRED celld host reference only (2026-11-12); do NOT use for live Rivet FP&A capacity.",
            evidence=HISTORICAL_PROXY_EVIDENCE,
        ))

    return {
        "schema": 1,
        "generated_at": store.now_utc(),
        "evidence": EVIDENCE_LABEL,
        "source": source,
        "currency": pricing.get("currency", "USD"),
        "records_considered": len(records),
        "pricing": pricing,
        "metrics": rows,
        "historical_proxies": historical_proxies,
    }


def format_json(report: dict) -> str:
    return json.dumps(report, indent=2, sort_keys=False) + "\n"


def format_csv(report: dict) -> str:
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\n")
    writer.writerow([
        "id",
        "category",
        "value",
        "unit",
        "evidence",
        "formula",
        "input_metrics",
        "input_values",
        "input_measured_at",
        "missing_inputs",
        "pricing_as_of",
        "notes",
    ])
    for row in (*report["metrics"], *report.get("historical_proxies", [])):
        inputs = row["inputs"]
        writer.writerow([
            row["id"],
            row["category"],
            "" if row["value"] is None else row["value"],
            row["unit"],
            row["evidence"],
            row["formula"],
            "; ".join(item["metric"] for item in inputs),
            "; ".join(f"{item['value']} {item['unit']}" for item in inputs),
            "; ".join(item["measured_at"] for item in inputs),
            "; ".join(row["missing_inputs"]),
            row["pricing_as_of"],
            row["notes"],
        ])
    return buf.getvalue()


def _fmt_value(value, unit: str, *, has_missing: bool = True) -> str:
    if value is None:
        return "null (missing inputs)" if has_missing else "null (undefined)"
    if isinstance(value, str):
        return f"`{value}`"
    if isinstance(value, int):
        return f"{value:,}"
    if unit.startswith("USD"):
        if abs(value) >= 0.01:
            return f"${value:,.4f}"
        return f"${value:.6f}"
    return f"{value:,.4g}"


def _md_table_rows(rows: list[dict]) -> list[str]:
    out = []
    for row in rows:
        if row["missing_inputs"]:
            detail = "missing: " + ", ".join(f"`{m}`" for m in row["missing_inputs"])
        else:
            detail = "; ".join(
                f"`{i['metric']}`={i['value']} {i['unit']} ({i['measured_at'][:10]})"
                for i in row["inputs"]
            )
            if row["value"] is None and row["notes"]:
                detail = f"{detail} ({row['notes']})" if detail else row["notes"]
        out.append(
            f"| {row['category']} | `{row['id']}` | "
            f"{_fmt_value(row['value'], row['unit'], has_missing=bool(row['missing_inputs']))} "
            f"| {row['unit']} | {row['pricing_as_of']} | {detail} |"
        )
    return out


def format_markdown(report: dict) -> str:
    lines = [
        "# Norn Unit-Economics Estimate",
        "",
        f"- **Source:** `{report['source']}` ({report['records_considered']:,} records)",
        f"- **Evidence:** `{report['evidence']}` (derived from measured metrics + dated list pricing)",
        f"- **Currency:** `{report['currency']}` (per-row pricing `as_of` dates shown in table)",
        "",
        "## Derived Unit Economics (Live Rivet / Core Engine)",
        "",
        "| Category | Metric | Value | Unit | Pricing As Of | Inputs / Missing |",
        "|---|---|---:|---|---|---|",
        *_md_table_rows(report["metrics"]),
    ]

    proxies = report.get("historical_proxies", [])
    if proxies:
        lines += [
            "",
            "## Historical Proxy Reference (Retired Hosts — Do Not Use for Live Rivet FP&A)",
            "",
            "| Category | Metric | Value | Unit | Pricing As Of | Inputs / Missing |",
            "|---|---|---:|---|---|---|",
            *_md_table_rows(proxies),
        ]

    lines += [
        "",
        "## Formulas & Audit Trail",
        "",
    ]
    for row in (*report["metrics"], *proxies):
        suffix = f" — {row['notes']}" if row["notes"] else ""
        lines.append(
            f"- `{row['id']}` (`{row['evidence']}`, pricing as of `{row['pricing_as_of']}`): "
            f"`{row['formula']}`{suffix}"
        )

    cpx = report["pricing"]["compute"]["hetzner_cpx62"]
    region = f" [{cpx['region']}]" if cpx.get("region") else ""
    lines += [
        "",
        "## Pricing & Capacity Assumptions",
        "",
        "| Parameter | Value | As of | Source |",
        "|---|---:|---|---|",
        f"| Hetzner `cpx62`{region} | ${cpx['monthly_usd']}/mo ({cpx['vcpu']} vCPU, {cpx['ram_gib']} GiB RAM, {cpx['nvme_gb']} GB NVMe) | {cpx['as_of']} | {cpx['source']} |",
    ]
    for section in ("storage", "bandwidth", "ci", "utilization", "workload_assumptions"):
        for key, spec in report["pricing"].get(section, {}).items():
            lines.append(
                f"| `{section}.{key}` | {spec['value']} {spec['unit']} | {spec['as_of']} | {spec['source']} |"
            )
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--jsonl", action="append", help="metric record JSONL path(s); omit to read git --rev")
    parser.add_argument("--repo", default=str(HERE.parents[1]), help="git repository holding the metrics branch")
    parser.add_argument("--rev", default="origin/metrics", help="git rev or rev:path blob (default: origin/metrics)")
    parser.add_argument("--pricing", default=str(DEFAULT_PRICING), help="infrastructure pricing JSON")
    parser.add_argument("--format", choices=("json", "csv", "md"), default="md", help="output format")
    parser.add_argument("--out", help="write output to this path instead of stdout")
    args = parser.parse_args(argv)

    records, source = load_records(jsonl_paths=args.jsonl, repo=Path(args.repo), rev=args.rev)
    pricing = load_pricing(Path(args.pricing))
    report = compute_unit_economics(records, pricing, source=source)

    renderers = {"json": format_json, "csv": format_csv, "md": format_markdown}
    rendered = renderers[args.format](report)
    if args.out:
        Path(args.out).write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    sys.exit(main())
