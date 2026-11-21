#!/usr/bin/env bash
# Lake engine bake-off driver (#1533), run on one Boat VM from the source
# root: `tools/lake-bakeoff/run.sh build|data|bench|sizes`. Every result is
# one `RESULT {json}` line appended to $OUT/results.jsonl.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="${BAKEOFF_WORK:-/home/user/bakeoff}"
OUT="$WORK/out"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/home/user/norn-target}"
mkdir -p "$WORK" "$OUT"
BIN="$CARGO_TARGET_DIR/release/bakeoff"
record() { grep '^RESULT ' | sed 's/^RESULT //' | tee -a "$OUT/results.jsonl"; }

case "${1:-}" in
build)
  cargo build --release --manifest-path "$ROOT/tools/lake-bakeoff/Cargo.toml" --bin bakeoff
  (cd "$ROOT" && cargo test --release --locked -p norn-cell --features duckdb --test bench_manifest --no-run)
  ;;
data)
  python3 "$ROOT/scripts/bench_bpi.py" fetch "$WORK/BPI_Challenge_2019.xes"
  [ -f "$WORK/bpi19.csv" ] || python3 "$ROOT/scripts/bench_bpi.py" csv "$WORK/BPI_Challenge_2019.xes" "$WORK/bpi19.csv"
  ;;
bench)
  CSV="$WORK/bpi19.csv"
  for run in 1 2 3; do
    # Alternate which engine lands first.
    if [ $((run % 2)) -eq 1 ]; then order="duckdb df df-builder"; else order="df-builder df duckdb"; fi
    for engine in $order; do
      case $engine in
        duckdb) "$BIN" land-duckdb "$CSV" "$WORK/land-duckdb-$run" | record ;;
        df) "$BIN" land-df "$CSV" "$WORK/land-df-$run" | record ;;
        df-builder) "$BIN" land-df-builder "$CSV" "$WORK/land-dft-$run" | record ;;
      esac
    done
    (cd "$ROOT" && NORN_BENCH_BPI_CSV="$CSV" NORN_BENCH_THREADS=4 NORN_BENCH_MEMORY=5GB NORN_BENCH_PER_ROW=0 \
      /usr/bin/time -f 'NORN_MAXRSS_KB %M' cargo test --release --locked -p norn-cell --features duckdb --test bench_manifest -- --ignored --nocapture 2>&1) \
      | grep -E '^BENCH-MANIFEST|NORN_MAXRSS_KB' | tee -a "$OUT/norn-harness.txt"
  done
  LAKE="$WORK/lake"
  mkdir -p "$LAKE"
  cp "$WORK/land-duckdb-1/data.parquet" "$LAKE/data.parquet"
  "$BIN" prep "$LAKE" | record
  for run in 1 2 3; do
    if [ $((run % 2)) -eq 1 ]; then a=duckdb; b=df; else a=df; b=duckdb; fi
    "$BIN" "queries-$a" "$LAKE" | record
    "$BIN" "queries-$b" "$LAKE" | record
  done
  for run in 1 2 3; do
    "$BIN" plan-duckdb "$LAKE" | record
    "$BIN" plan-df "$LAKE" | record
  done
  "$BIN" encrypt "$LAKE" | record
  "$BIN" substrait "$LAKE" | record
  ;;
sizes)
  M="$ROOT/tools/lake-bakeoff/Cargo.toml"
  cargo build --profile size --manifest-path "$M" --no-default-features --features duckdb-engine --bin size-duckdb
  cargo build --profile size --manifest-path "$M" --no-default-features --features df-engine --bin size-datafusion
  (cd "$ROOT" && cargo build --release --locked -p norn-cell-do --features duckdb --bin norn-olapd)
  cp "$CARGO_TARGET_DIR/release/norn-olapd" "$WORK/norn-olapd.stripped" && strip "$WORK/norn-olapd.stripped"
  python3 - "$CARGO_TARGET_DIR" "$WORK" <<'PY' | tee -a "$OUT/results.jsonl"
import json, os, sys
t, w = sys.argv[1], sys.argv[2]
size = lambda p: os.path.getsize(p) if os.path.exists(p) else None
print(json.dumps({"bench": "sizes",
  "sizeDuckdbStripped": size(f"{t}/size/size-duckdb"),
  "sizeDatafusionStripped": size(f"{t}/size/size-datafusion"),
  "olapdRelease": size(f"{t}/release/norn-olapd"),
  "olapdStripped": size(f"{w}/norn-olapd.stripped")}))
PY
  ;;
*) echo "usage: $0 build|data|bench|sizes" >&2; exit 2 ;;
esac
