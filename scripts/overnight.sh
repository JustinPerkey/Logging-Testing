#!/usr/bin/env bash
#
# overnight.sh — run the full logging-crate comparison with enough repetitions
# for statistical significance, then build a report.
#
# WHY A SCRIPT (and not one `logbench` invocation): the real logging crates each
# install a *process-global* logger (the `log` facade's global logger, or
# `tracing`'s global default subscriber), and only one can exist per process. So
# this harness runs **each strategy in its own process**, and repeats the whole
# thing for many independent **trials**. The Python aggregator
# (`scripts/aggregate.py`) then treats each trial as one observation and reports
# means with 95% confidence intervals — that is where the statistical
# significance comes from.
#
# Strategy order is reshuffled every trial so that slow thermal drift or a
# transient background load can't systematically favour whichever strategy
# always ran first.
#
# Usage:
#   scripts/overnight.sh                 # overnight-sized defaults (~hours)
#   SMOKE=1 scripts/overnight.sh         # tiny, ~1 minute, to validate the pipeline
#   TRIALS=50 MESSAGES=1000000 scripts/overnight.sh
#
# Everything is configurable via environment variables (see DEFAULTS below).
# The run is safe to Ctrl-C: it aggregates whatever trials completed so far.

set -uo pipefail

# --------------------------------------------------------------------------
# Defaults (override via env). These are sized so the matrix below, at the
# default trial count, fills a night on a typical multi-core box. Trim TRIALS
# or the sweep axes for a shorter run.
# --------------------------------------------------------------------------
TRIALS="${TRIALS:-40}"                       # independent repetitions per strategy
MESSAGES="${MESSAGES:-500000}"               # measured records per producer per case
WARMUP="${WARMUP:-20000}"                    # untimed warmup records per producer
MSG_SIZES="${MSG_SIZES:-64,512,4096}"        # payload sizes (bytes)
PRODUCERS="${PRODUCERS:-1,4,8}"              # concurrent producer-thread counts
BUFFERS="${BUFFERS:-8192}"                   # channel/queue capacity (records)
RATES="${RATES:-0}"                          # 0 = max throughput; or rec/s per producer
WRITER_BUF="${WRITER_BUF:-65536}"            # background BufWriter size (bytes)
FULL_POLICY="${FULL_POLICY:-block}"          # block (lossless) or drop (lossy)
STRATEGIES="${STRATEGIES:-direct,crossbeam,flume,tracing-appender,env_logger,fern,log4rs,flexi_logger,slog-async,tracing-fmt,tracing-nb,tracing-span,ftlog}"
# OUT_DIR is defaulted *after* the SMOKE block so smoke runs land in smoke-out
# unless the user set OUT_DIR explicitly.
MAX_HOURS="${MAX_HOURS:-10}"                 # stop launching new trials after this
PER_RUN_TIMEOUT="${PER_RUN_TIMEOUT:-1800}"   # seconds; kill a wedged single run

# SMOKE mode: a tiny, fast end-to-end validation of the whole pipeline.
if [[ "${SMOKE:-0}" == "1" ]]; then
    TRIALS="${TRIALS_SMOKE:-6}"
    MESSAGES="${MESSAGES_SMOKE:-20000}"
    WARMUP="${WARMUP_SMOKE:-2000}"
    MSG_SIZES="${MSG_SIZES_SMOKE:-64,1024}"
    PRODUCERS="${PRODUCERS_SMOKE:-1,4}"
    MAX_HOURS="${MAX_HOURS_SMOKE:-1}"
    OUT_DIR="${OUT_DIR:-smoke-out}"
fi

# Default output directory for the normal (non-smoke) run.
OUT_DIR="${OUT_DIR:-overnight-out}"

# --------------------------------------------------------------------------
# Embedded / core-limited safety: never let a single benchmark case spawn more
# concurrent producer threads than the machine has cores.
#
# Each case runs `producers` threads that emit simultaneously (see
# runner::run_case). On a beefy multi-core box, sweeping producers=1,4,8 measures
# real lock/queue contention. On a core-limited embedded device the same 8-way
# case oversubscribes the cores: the producers spend their time fighting the OS
# scheduler instead of the logger, which distorts the hot-path latency we care
# about and can wedge a tiny board. So clamp the producer sweep to the available
# core count — on a single-core device this collapses to producers=1 (fully
# serial), exactly what you want when running on constrained hardware.
#
# Override the ceiling with MAX_PRODUCERS (set it high, e.g. 9999, to opt out).
# --------------------------------------------------------------------------
NPROC="$(nproc 2>/dev/null || echo 1)"
MAX_PRODUCERS="${MAX_PRODUCERS:-$NPROC}"

clamp_producers() {
    local kept=() p
    IFS=',' read -r -a _producers <<<"$PRODUCERS"
    for p in "${_producers[@]}"; do
        if (( p <= MAX_PRODUCERS )); then
            kept+=("$p")
        fi
    done
    # Always keep at least one case; if everything exceeded the ceiling, fall
    # back to a single case capped at the ceiling itself.
    if (( ${#kept[@]} == 0 )); then
        kept=("$MAX_PRODUCERS")
    fi
    local clamped
    clamped="$(IFS=,; echo "${kept[*]}")"
    if [[ "$clamped" != "$PRODUCERS" ]]; then
        PRODUCERS_CLAMP_NOTE="producers clamped from [$PRODUCERS] to [$clamped] (cores=$NPROC, MAX_PRODUCERS=$MAX_PRODUCERS)"
    fi
    PRODUCERS="$clamped"
}
clamp_producers

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/logbench"
RESULTS_DIR="$OUT_DIR/results"
LOGS_DIR="$OUT_DIR/logs"        # transient log files the strategies produce
RUN_LOG="$OUT_DIR/overnight.log"

mkdir -p "$RESULTS_DIR" "$LOGS_DIR"

log() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$RUN_LOG"; }

# --------------------------------------------------------------------------
# Build (always release — benchmarking a debug build is meaningless).
# --------------------------------------------------------------------------
log "Building release binary..."
if ! (cd "$ROOT" && cargo build --release >>"$RUN_LOG" 2>&1); then
    log "ERROR: cargo build failed; see $RUN_LOG"
    exit 1
fi

IFS=',' read -r -a STRAT_ARR <<<"$STRATEGIES"
log "Strategies (${#STRAT_ARR[@]}): $STRATEGIES"
log "Trials=$TRIALS messages=$MESSAGES warmup=$WARMUP sizes=$MSG_SIZES producers=$PRODUCERS"
[[ -n "${PRODUCERS_CLAMP_NOTE:-}" ]] && log "$PRODUCERS_CLAMP_NOTE"
log "Output: $OUT_DIR  (max ${MAX_HOURS}h)"

# --------------------------------------------------------------------------
# Capture run metadata for the report header.
# --------------------------------------------------------------------------
git_commit="$(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
cpu_model="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //' || echo unknown)"
cpu_count="$(nproc 2>/dev/null || echo unknown)"
mem_total="$(grep -m1 MemTotal /proc/meminfo 2>/dev/null | awk '{printf "%.1f GiB", $2/1048576}' || echo unknown)"
rustc_ver="$(rustc --version 2>/dev/null || echo unknown)"
started="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

json_escape() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

write_meta() {
    local finished="$1"
    cat >"$OUT_DIR/run_meta.json" <<EOF
{
  "host": $(json_escape "$(hostname 2>/dev/null || echo unknown)"),
  "kernel": $(json_escape "$(uname -sr 2>/dev/null || echo unknown)"),
  "cpu_model": $(json_escape "$cpu_model"),
  "cpu_count": $(json_escape "$cpu_count"),
  "memory": $(json_escape "$mem_total"),
  "rustc": $(json_escape "$rustc_ver"),
  "git_commit": $(json_escape "$git_commit"),
  "started": $(json_escape "$started"),
  "finished": $(json_escape "$finished"),
  "trials_requested": $(json_escape "$TRIALS"),
  "messages": $(json_escape "$MESSAGES"),
  "warmup": $(json_escape "$WARMUP"),
  "writer_buf": $(json_escape "$WRITER_BUF"),
  "full_policy": $(json_escape "$FULL_POLICY"),
  "msg_sizes": $(json_escape "$MSG_SIZES"),
  "producers": $(json_escape "$PRODUCERS"),
  "rates": $(json_escape "$RATES"),
  "buffers": $(json_escape "$BUFFERS")
}
EOF
}
write_meta "(in progress)"

# Reshuffle the strategy order each trial (Fisher-Yates via `shuf` when present).
shuffle() {
    if command -v shuf >/dev/null 2>&1; then
        printf '%s\n' "${STRAT_ARR[@]}" | shuf
    else
        printf '%s\n' "${STRAT_ARR[@]}"
    fi
}

# Graceful stop on Ctrl-C: break out, then still aggregate.
STOP=0
trap 'STOP=1; log "Interrupt received — finishing current run then aggregating."' INT TERM

start_epoch=$(date +%s)
deadline=$(( start_epoch + $(python3 -c "print(int(${MAX_HOURS}*3600))") ))

total_runs=$(( TRIALS * ${#STRAT_ARR[@]} ))
run_no=0

for (( trial=1; trial<=TRIALS; trial++ )); do
    (( STOP )) && break
    if (( $(date +%s) >= deadline )); then
        log "Time budget (${MAX_HOURS}h) reached at trial $trial; stopping."
        break
    fi
    trial_tag=$(printf '%02d' "$trial")
    while read -r strat; do
        (( STOP )) && break
        run_no=$(( run_no + 1 ))
        mkdir -p "$RESULTS_DIR/$strat"
        json_out="$RESULTS_DIR/$strat/trial_${trial_tag}.json"
        csv_out="$RESULTS_DIR/$strat/trial_${trial_tag}.csv"

        # One strategy, one process. Each process owns at most one global logger.
        timeout "$PER_RUN_TIMEOUT" "$BIN" \
            --strategies "$strat" \
            --msg-sizes "$MSG_SIZES" \
            --buffers "$BUFFERS" \
            --producers "$PRODUCERS" \
            --rates "$RATES" \
            --messages "$MESSAGES" \
            --warmup "$WARMUP" \
            --writer-buf "$WRITER_BUF" \
            --full-policy "$FULL_POLICY" \
            --out-dir "$LOGS_DIR/$strat" \
            --json "$json_out" \
            --csv "$csv_out" \
            >>"$RUN_LOG" 2>&1
        rc=$?
        if (( rc == 0 )); then
            log "[$run_no/$total_runs] trial $trial_tag  $strat  ok"
        else
            log "[$run_no/$total_runs] trial $trial_tag  $strat  FAILED (rc=$rc)"
        fi
        # Reclaim the strategies' raw log files; we only keep the JSON metrics.
        rm -rf "${LOGS_DIR:?}/$strat" 2>/dev/null || true
    done < <(shuffle)
done

finished="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
write_meta "$finished"

elapsed=$(( $(date +%s) - start_epoch ))
log "Completed $run_no runs in $(( elapsed/3600 ))h $(( (elapsed%3600)/60 ))m."

# --------------------------------------------------------------------------
# Aggregate into REPORT.md + summary_stats.csv.
# --------------------------------------------------------------------------
log "Aggregating results into $OUT_DIR/REPORT.md ..."
if python3 "$ROOT/scripts/aggregate.py" "$OUT_DIR" >>"$RUN_LOG" 2>&1; then
    log "Report ready: $OUT_DIR/REPORT.md"
else
    log "ERROR: aggregation failed; see $RUN_LOG"
    exit 1
fi
