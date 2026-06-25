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
#
# Benchmarking ANOTHER device, fully automated (LOGBENCH_REMOTE). Point
# LOGBENCH_REMOTE at an SSH target and this script does the whole round-trip for
# you: it builds the binary HERE, copies it to the device (chmod +x), runs every
# trial ON the device over SSH, copies each trial's JSON result back, and then
# aggregates the report HERE. Nothing to do by hand.
#
#   # same architecture as this host (e.g. another x86-64 box):
#   LOGBENCH_REMOTE=user@device.local scripts/overnight.sh
#
#   # different architecture — cross-compile for the device (needs the Rust
#   # target + a linker installed here):
#   LOGBENCH_REMOTE=pi@pi.local LOGBENCH_TARGET=aarch64-unknown-linux-gnu \
#     scripts/overnight.sh
#
# The device needs nothing but an SSH server and the ability to run the binary —
# no Rust toolchain, no Python, no repo checkout. The captured run_meta.json
# records the *device's* CPU/kernel/memory (that is what was benchmarked) and
# notes this host as the build host. Remote knobs:
#
#   LOGBENCH_REMOTE      user@host of the device. Unset => ordinary local run.
#   LOGBENCH_TARGET      Rust target triple to cross-compile for (e.g.
#                        aarch64-unknown-linux-gnu). Unset => build for this host
#                        (fine when the device shares this host's architecture).
#   LOGBENCH_REMOTE_DIR  staging dir on the device. Default: /tmp/logbench-overnight.
#   LOGBENCH_SSH         ssh command. Default: "ssh". e.g. "ssh -p 2222 -i ~/key".
#   LOGBENCH_SCP         scp command. Default: "scp". e.g. "scp -P 2222 -i ~/key".
#   LOGBENCH_KEEP_REMOTE set to 1 to leave the staged binary/results on the device.
#
# Other binary knobs (also useful without LOGBENCH_REMOTE):
#   LOGBENCH_BIN   path to a prebuilt logbench binary to use instead of building.
#                  Default: target/release/logbench (or target/<triple>/release
#                  when LOGBENCH_TARGET is set).
#   SKIP_BUILD     set to 1 to skip `cargo build` and use LOGBENCH_BIN as-is.

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

# Remote-device orchestration (see header). When REMOTE is empty everything runs
# locally and these are ignored.
REMOTE="${LOGBENCH_REMOTE:-}"
TARGET="${LOGBENCH_TARGET:-}"
REMOTE_DIR="${LOGBENCH_REMOTE_DIR:-/tmp/logbench-overnight}"
SSH="${LOGBENCH_SSH:-ssh}"
SCP="${LOGBENCH_SCP:-scp}"
KEEP_REMOTE="${LOGBENCH_KEEP_REMOTE:-0}"
REMOTE_BIN_DIR="$REMOTE_DIR/bin"             # the executable lives in, and runs from, here
REMOTE_BIN="$REMOTE_BIN_DIR/logbench"        # where the binary lands on the device

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

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Locate the binary we will run (locally) or stage onto the device. An explicit
# LOGBENCH_BIN always wins; otherwise it is the in-tree release binary, under the
# target-specific path when cross-compiling.
if [[ -n "${LOGBENCH_BIN:-}" ]]; then
    BIN="$LOGBENCH_BIN"
elif [[ -n "$TARGET" ]]; then
    BIN="$ROOT/target/$TARGET/release/logbench"
else
    BIN="$ROOT/target/release/logbench"
fi
RESULTS_DIR="$OUT_DIR/results"
LOGS_DIR="$OUT_DIR/logs"        # transient log files the strategies produce
RUN_LOG="$OUT_DIR/overnight.log"

mkdir -p "$RESULTS_DIR" "$LOGS_DIR"

log() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$RUN_LOG"; }

# --------------------------------------------------------------------------
# Build (always release — benchmarking a debug build is meaningless). Skipped
# with SKIP_BUILD=1 (reuse a prebuilt LOGBENCH_BIN). With LOGBENCH_TARGET set we
# cross-compile so the binary can run on a different-architecture device.
# --------------------------------------------------------------------------
if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
    if [[ ! -x "$BIN" ]]; then
        log "ERROR: SKIP_BUILD=1 but no executable binary at '$BIN' (set LOGBENCH_BIN)."
        exit 1
    fi
    log "Skipping build (SKIP_BUILD=1); using prebuilt binary: $BIN"
else
    build_args=(build --release)
    if [[ -n "$TARGET" ]]; then
        log "Building release binary (cross-compiling for $TARGET)..."
        build_args+=(--target "$TARGET")
    else
        log "Building release binary..."
    fi
    if ! (cd "$ROOT" && cargo "${build_args[@]}" >>"$RUN_LOG" 2>&1); then
        log "ERROR: cargo build failed; see $RUN_LOG"
        exit 1
    fi
fi

# --------------------------------------------------------------------------
# Remote staging: copy the freshly built binary to the device, make it
# executable, and verify it actually runs there (catches an architecture
# mismatch up front, before we burn a night of trials).
# --------------------------------------------------------------------------
# Tidy up the device on exit (registered before staging so a failed setup that
# already created the staging dir still gets cleaned up).
remote_cleanup() {
    [[ -n "$REMOTE" && "$KEEP_REMOTE" != "1" ]] || return 0
    # shellcheck disable=SC2086
    $SSH "$REMOTE" "rm -rf '$REMOTE_DIR'" >/dev/null 2>&1 || true
}
trap remote_cleanup EXIT

# shellcheck disable=SC2086  # $SSH/$SCP are intentionally word-split (may carry flags).
if [[ -n "$REMOTE" ]]; then
    log "Remote device: $REMOTE — staging $BIN at $REMOTE:$REMOTE_BIN"
    if ! $SSH "$REMOTE" "mkdir -p '$REMOTE_BIN_DIR'" >>"$RUN_LOG" 2>&1; then
        log "ERROR: cannot SSH to $REMOTE (or mkdir '$REMOTE_BIN_DIR' failed); see $RUN_LOG"
        exit 1
    fi
    if ! $SCP -q "$BIN" "$REMOTE:$REMOTE_BIN" >>"$RUN_LOG" 2>&1; then
        log "ERROR: scp of '$BIN' to $REMOTE failed; see $RUN_LOG"
        exit 1
    fi
    $SSH "$REMOTE" "chmod +x '$REMOTE_BIN'" >>"$RUN_LOG" 2>&1 || true
    # Run from the bin directory (cd in, invoke by relative name) so the staged
    # executable is exercised exactly as the trials will invoke it below.
    if ! $SSH "$REMOTE" "cd '$REMOTE_BIN_DIR' && ./logbench --help" >>"$RUN_LOG" 2>&1; then
        log "ERROR: '$REMOTE_BIN' will not execute on $REMOTE."
        log "       Likely an architecture mismatch — set LOGBENCH_TARGET to the device's"
        log "       Rust target triple (e.g. aarch64-unknown-linux-gnu) to cross-compile."
        exit 1
    fi
fi

IFS=',' read -r -a STRAT_ARR <<<"$STRATEGIES"
log "Strategies (${#STRAT_ARR[@]}): $STRATEGIES"
log "Trials=$TRIALS messages=$MESSAGES warmup=$WARMUP sizes=$MSG_SIZES producers=$PRODUCERS"
log "Output: $OUT_DIR  (max ${MAX_HOURS}h)"

# --------------------------------------------------------------------------
# Capture run metadata for the report header. The hardware/OS fields must
# describe the machine that is *benchmarked* — the device under test in remote
# mode, otherwise this host. Build provenance (git commit, rustc, build host)
# always describes where the binary was compiled (this host).
# --------------------------------------------------------------------------
git_commit="$(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
rustc_ver="$(rustc --version 2>/dev/null || echo unknown)"
build_host="$(hostname 2>/dev/null || echo unknown)"
started="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

# Self-contained probe that prints `field<TAB>value` lines. Quoted heredoc => no
# expansion here; it is fed verbatim to the benchmarked machine's shell, so the
# awk/sed quoting needs no SSH-escaping gymnastics.
read -r -d '' META_PROBE <<'PROBE' || true
cpu_model=$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //')
cpu_count=$(nproc 2>/dev/null)
mem_total=$(grep -m1 MemTotal /proc/meminfo 2>/dev/null | awk '{printf "%.1f GiB", $2/1048576}')
kernel=$(uname -sr 2>/dev/null)
host=$(hostname 2>/dev/null)
printf 'cpu_model\t%s\n' "${cpu_model:-unknown}"
printf 'cpu_count\t%s\n' "${cpu_count:-unknown}"
printf 'mem_total\t%s\n' "${mem_total:-unknown}"
printf 'kernel\t%s\n' "${kernel:-unknown}"
printf 'host\t%s\n' "${host:-unknown}"
PROBE

# Run the probe on the benchmarked machine (device if remote, else local).
host_name=unknown; kernel=unknown; cpu_model=unknown; cpu_count=unknown; mem_total=unknown
while IFS=$'\t' read -r _k _v; do
    case "$_k" in
        host)      host_name="$_v" ;;
        kernel)    kernel="$_v" ;;
        cpu_model) cpu_model="$_v" ;;
        cpu_count) cpu_count="$_v" ;;
        mem_total) mem_total="$_v" ;;
    esac
done < <(
    if [[ -n "$REMOTE" ]]; then
        # shellcheck disable=SC2086
        printf '%s' "$META_PROBE" | $SSH "$REMOTE" bash -s 2>/dev/null
    else
        printf '%s' "$META_PROBE" | bash -s 2>/dev/null
    fi
)

json_escape() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

write_meta() {
    local finished="$1"
    cat >"$OUT_DIR/run_meta.json" <<EOF
{
  "host": $(json_escape "$host_name"),
  "kernel": $(json_escape "$kernel"),
  "cpu_model": $(json_escape "$cpu_model"),
  "cpu_count": $(json_escape "$cpu_count"),
  "memory": $(json_escape "$mem_total"),
  "rustc": $(json_escape "$rustc_ver"),
  "build_host": $(json_escape "$build_host"),
  "remote": $(json_escape "${REMOTE:-(local)}"),
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

# Run one (strategy, trial): execute the binary for one strategy and land its
# JSON+CSV at the given host paths. Locally it runs the binary directly; in
# remote mode it runs the staged binary on the device over SSH (writing to a
# device-local scratch dir) and copies the two result files back. One strategy
# per process either way — each process owns at most one global logger.
run_case() {
    local strat="$1" json_out="$2" csv_out="$3"
    local common_args=(
        --strategies "$strat"
        --msg-sizes "$MSG_SIZES"
        --buffers "$BUFFERS"
        --producers "$PRODUCERS"
        --rates "$RATES"
        --messages "$MESSAGES"
        --warmup "$WARMUP"
        --writer-buf "$WRITER_BUF"
        --full-policy "$FULL_POLICY"
    )

    if [[ -z "$REMOTE" ]]; then
        timeout "$PER_RUN_TIMEOUT" "$BIN" "${common_args[@]}" \
            --out-dir "$LOGS_DIR/$strat" \
            --json "$json_out" \
            --csv "$csv_out" \
            >>"$RUN_LOG" 2>&1
        local rc=$?
        rm -rf "${LOGS_DIR:?}/$strat" 2>/dev/null || true
        return $rc
    fi

    # Remote: build a quoted remote command (printf %q is safe for these simple
    # values), run it from the bin directory on the device, then scp the
    # JSON+CSV back. Output paths are absolute, so the cd doesn't affect them.
    local r_logs="$REMOTE_DIR/run/logs" r_json="$REMOTE_DIR/run/out.json" r_csv="$REMOTE_DIR/run/out.csv"
    local qa="" a
    for a in "${common_args[@]}" --out-dir "$r_logs" --json "$r_json" --csv "$r_csv"; do
        qa+=" $(printf '%q' "$a")"
    done
    local rcmd="mkdir -p $(printf '%q' "$r_logs") && cd $(printf '%q' "$REMOTE_BIN_DIR") && ./logbench$qa; rc=\$?; rm -rf $(printf '%q' "$r_logs"); exit \$rc"
    # shellcheck disable=SC2086  # $SSH/$SCP are intentionally word-split.
    timeout "$PER_RUN_TIMEOUT" $SSH "$REMOTE" "$rcmd" >>"$RUN_LOG" 2>&1 || return $?
    # shellcheck disable=SC2086
    $SCP -q "$REMOTE:$r_json" "$json_out" >>"$RUN_LOG" 2>&1 || return $?
    # shellcheck disable=SC2086
    $SCP -q "$REMOTE:$r_csv" "$csv_out" >>"$RUN_LOG" 2>&1 || return $?
    return 0
}

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

        run_case "$strat" "$json_out" "$csv_out"
        rc=$?
        if (( rc == 0 )); then
            log "[$run_no/$total_runs] trial $trial_tag  $strat  ok"
        else
            log "[$run_no/$total_runs] trial $trial_tag  $strat  FAILED (rc=$rc)"
        fi
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
