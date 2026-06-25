#!/usr/bin/env python3
"""Aggregate logbench overnight-run JSON into a statistically-grounded report.

The overnight harness (`scripts/overnight.sh`) runs every logging strategy in
its own process, many independent times ("trials"), and drops one JSON file per
trial under:

    <run-dir>/results/<strategy>/trial_<NN>.json

Each JSON file is an array of per-case results (one object per swept workload
cell). This script treats **each trial as one independent observation** of every
(strategy, workload-cell) pair, then computes summary statistics across trials —
mean, sample standard deviation, and a 95% confidence interval (Student's t) —
so the report can say not just "A was faster than B" but "A was faster than B by
a margin that is / isn't larger than the run-to-run noise".

It writes:
  * REPORT.md            — the human-readable report
  * summary_stats.csv    — every (strategy, cell, metric) aggregate row

No third-party dependencies — standard library only.
"""

import csv
import glob
import json
import math
import os
import statistics
import sys
from collections import defaultdict
from datetime import datetime, timezone

# --- statistics helpers -----------------------------------------------------

# Two-sided 95% Student-t critical values by degrees of freedom (n-1). For
# df >= 30 the value is close enough to the normal 1.96 that we asymptote to it.
_T95 = {
    1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365,
    8: 2.306, 9: 2.262, 10: 2.228, 11: 2.201, 12: 2.179, 13: 2.160, 14: 2.145,
    15: 2.131, 16: 2.120, 17: 2.110, 18: 2.101, 19: 2.093, 20: 2.086,
    21: 2.080, 22: 2.074, 23: 2.069, 24: 2.064, 25: 2.060, 26: 2.056,
    27: 2.052, 28: 2.048, 29: 2.045,
}


def t95(df):
    if df <= 0:
        return float("nan")
    if df in _T95:
        return _T95[df]
    return 1.96  # df >= 30


class Agg:
    """Summary statistics for one metric across N trial observations."""

    def __init__(self, values):
        self.values = [v for v in values if v is not None]
        self.n = len(self.values)
        if self.n == 0:
            self.mean = self.sd = self.sem = self.ci = float("nan")
            self.lo = self.hi = self.median = self.vmin = self.vmax = float("nan")
            self.cv = float("nan")
            return
        self.mean = statistics.fmean(self.values)
        self.sd = statistics.stdev(self.values) if self.n > 1 else 0.0
        self.sem = self.sd / math.sqrt(self.n) if self.n > 1 else 0.0
        self.ci = t95(self.n - 1) * self.sem if self.n > 1 else 0.0
        self.lo = self.mean - self.ci
        self.hi = self.mean + self.ci
        self.median = statistics.median(self.values)
        self.vmin = min(self.values)
        self.vmax = max(self.values)
        self.cv = (self.sd / self.mean * 100.0) if self.mean else float("nan")


def overlaps(a, b):
    """True if the 95% CIs of two Agg metrics overlap (≈ not distinguishable)."""
    if a.n < 2 or b.n < 2:
        return True
    return not (a.hi < b.lo or b.hi < a.lo)


# --- formatting helpers -----------------------------------------------------

def fmt_ns(ns):
    if ns != ns:  # NaN
        return "n/a"
    if ns < 1_000:
        return f"{ns:.0f} ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.2f} µs"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    return f"{ns / 1_000_000_000:.2f} s"


def fmt_rate(r):
    if r != r:
        return "n/a"
    if r >= 1e6:
        return f"{r / 1e6:.2f} M/s"
    if r >= 1e3:
        return f"{r / 1e3:.1f} k/s"
    return f"{r:.0f} /s"


def fmt_ci_ns(agg):
    if agg.n < 2:
        return fmt_ns(agg.mean)
    return f"{fmt_ns(agg.mean)} ±{fmt_ns(agg.ci)}"


def fmt_ci_rate(agg):
    if agg.n < 2:
        return fmt_rate(agg.mean)
    return f"{fmt_rate(agg.mean)} ±{fmt_rate(agg.ci)}"


# Which family each strategy belongs to, for the narrative grouping.
FAMILY = {
    "direct": "Transport baselines (raw bytes)",
    "crossbeam": "Transport baselines (raw bytes)",
    "flume": "Transport baselines (raw bytes)",
    "tracing-appender": "Transport baselines (raw bytes)",
    "env_logger": "`log` facade backends",
    "fern": "`log` facade backends",
    "log4rs": "`log` facade backends",
    "flexi_logger": "`log` facade backends",
    "slog-async": "Structured logging",
    "tracing-fmt": "Instrumentation (tracing)",
    "tracing-nb": "Instrumentation (tracing)",
    "tracing-span": "Instrumentation (tracing)",
    "ftlog": "High-throughput async",
}
FAMILY_ORDER = [
    "Transport baselines (raw bytes)",
    "`log` facade backends",
    "Structured logging",
    "Instrumentation (tracing)",
    "High-throughput async",
]


# --- data loading -----------------------------------------------------------

def load(run_dir):
    """Return (records, meta).

    records: list of dicts, each a per-case result tagged with its trial index.
    meta:    contents of run_meta.json if present, else {}.
    """
    meta = {}
    meta_path = os.path.join(run_dir, "run_meta.json")
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            meta = json.load(f)

    records = []
    pattern = os.path.join(run_dir, "results", "*", "trial_*.json")
    files = sorted(glob.glob(pattern))
    for path in files:
        trial = os.path.basename(path).replace("trial_", "").replace(".json", "")
        try:
            with open(path) as f:
                rows = json.load(f)
        except (json.JSONDecodeError, OSError) as e:
            print(f"warning: skipping {path}: {e}", file=sys.stderr)
            continue
        for row in rows:
            row["_trial"] = trial
            records.append(row)
    return records, meta, files


def cell_key(row):
    rate = row.get("target_rate_per_producer")
    rate = "max" if rate in (None, 0, 0.0) else f"{rate:g}"
    return (row["msg_size"], row["capacity"], row["producers"], rate,
            row["full_policy"])


def cell_label(key):
    size, cap, prod, rate, policy = key
    cap_s = "∞" if cap == 0 else str(cap)
    return (f"{size} B payload · {prod} producer(s) · buffer {cap_s} · "
            f"rate {rate} · {policy}")


def cores_from_meta(meta):
    """Best-effort integer core count from run_meta.json (None if unknown)."""
    try:
        return int(str(meta.get("cpu_count")).strip())
    except (TypeError, ValueError):
        return None


def oversub_note(producers, cores):
    """Markdown warning for a cell that runs more producers than cores, else None.

    When `producers` exceeds the available cores the producer threads can't all
    execute at once: the OS time-slices them, so each measured `log()` call also
    pays context-switch / run-queue-wait time that has nothing to do with the
    logger. On core-limited (e.g. embedded) hardware that thread-switching
    overhead dominates the number, so the reader needs to know the cell is
    measuring oversubscribed contention rather than the logger's intrinsic cost.
    """
    if cores is None or producers <= cores:
        return None
    return (
        f"> ⚠️ **Oversubscribed: {producers} producer threads on {cores} core(s).** "
        f"More producers than cores means they can't run simultaneously — the OS "
        f"time-slices them, so the hot-path latency below includes "
        f"context-switch and scheduler-wait time, not just the logger's own cost. "
        f"On core-limited (embedded) hardware this thread-switching overhead "
        f"dominates; read these as oversubscribed-contention figures, not the "
        f"logger's intrinsic latency."
    )


# --- aggregation ------------------------------------------------------------

# Metrics we summarise across trials. Lower-is-better unless noted.
LATENCY_METRICS = [
    ("p50_ns", "p50", "lower"),
    ("p99_ns", "p99", "lower"),
    ("p999_ns", "p99.9", "lower"),
    ("max_ns", "max", "lower"),
    ("mean_ns", "mean", "lower"),
]


def build_aggregates(records):
    """cell -> strategy -> metric -> Agg, plus per-cell strategy list."""
    # cell -> strategy -> list of per-trial rows
    grouped = defaultdict(lambda: defaultdict(list))
    for row in records:
        grouped[cell_key(row)][row["strategy"]].append(row)

    agg = {}
    for cell, by_strat in grouped.items():
        agg[cell] = {}
        for strat, rows in by_strat.items():
            metrics = {}
            for field, _, _ in LATENCY_METRICS:
                metrics[field] = Agg([r["latency"][field] for r in rows])
            metrics["throughput"] = Agg([r["end_to_end_throughput"] for r in rows])
            metrics["mb_per_sec"] = Agg([r["mb_per_sec"] for r in rows])
            metrics["dropped"] = Agg([r["dropped"] for r in rows])
            metrics["_n_trials"] = len(rows)
            agg[cell][strat] = metrics
    return agg


# --- report generation ------------------------------------------------------

def strategy_order(strats):
    """Order strategies by family then name for stable presentation."""
    return sorted(strats, key=lambda s: (FAMILY_ORDER.index(FAMILY.get(s, FAMILY_ORDER[0])), s))


def md_table(headers, rows):
    out = ["| " + " | ".join(headers) + " |",
           "| " + " | ".join("---" for _ in headers) + " |"]
    for r in rows:
        out.append("| " + " | ".join(str(c) for c in r) + " |")
    return "\n".join(out)


def report(run_dir, agg, meta, files):
    lines = []
    A = lines.append
    cores = cores_from_meta(meta)

    A("# Rust logging crates — benchmark report\n")
    generated = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    A(f"_Generated {generated} by `scripts/aggregate.py`._\n")

    # --- run metadata
    A("## Run environment\n")
    if meta:
        env_rows = []
        for k in ("host", "kernel", "cpu_model", "cpu_count", "memory",
                  "rustc", "git_commit", "started", "finished",
                  "trials_requested", "messages", "warmup", "writer_buf",
                  "full_policy", "msg_sizes", "producers", "rates", "buffers"):
            if k in meta and meta[k] not in (None, ""):
                env_rows.append((f"`{k}`", meta[k]))
        A(md_table(["field", "value"], env_rows))
    else:
        A("_No `run_meta.json` found; this report was built from result files only._")
    n_trials_seen = len({(r) for r in files})
    A(f"\nResult files parsed: **{len(files)}**.\n")

    # collect global strategy set and cells
    all_strats = set()
    for cell in agg:
        all_strats.update(agg[cell].keys())
    cells = sorted(agg.keys())

    # --- methodology
    A("## Methodology\n")
    A(
        "Every strategy was run **in its own process**, repeated for many "
        "independent **trials**. Each trial spins up the logger fresh, warms it "
        "up, then times a single `log()` call on each of several concurrent "
        "producer threads while they emit a fixed number of records. The "
        "headline metric is the **producer-side hot-path latency** of that "
        "`log()` call — the time your application thread is held inside the "
        "logging call — captured as a full HdrHistogram per trial. We also "
        "record end-to-end throughput (including the final drain/flush).\n"
    )
    A(
        "For statistical significance we treat **each trial as one observation** "
        "of each metric and report the **mean across trials ± the 95% confidence "
        "interval** (Student's t). When two strategies' confidence intervals "
        "overlap, the difference between them is within run-to-run noise and we "
        "say they are *not statistically distinguishable* on this machine. The "
        "transport baselines (`direct`, `crossbeam`, `flume`, "
        "`tracing-appender`) write raw bytes and pay **no formatting cost**; the "
        "real-crate strategies pay their genuine timestamp/level/format/sink "
        "cost, which is the whole point of the comparison.\n"
    )
    if cores is not None:
        A(
            f"This machine has **{cores} core(s)**. Workloads whose producer count "
            f"exceeds that are **oversubscribed**: the producer threads can't all "
            f"run at once, so the OS time-slices them and the measured hot-path "
            f"latency includes context-switch / scheduler-wait time on top of the "
            f"logger's own cost. Those workloads are flagged inline below — on a "
            f"core-limited (embedded) device the thread-switching overhead, not the "
            f"logger, is what dominates them.\n"
        )

    # =====================================================================
    # Executive summary — pick a representative "primary" cell.
    # =====================================================================
    A("## Executive summary\n")
    primary = pick_primary_cell(cells, agg)
    if primary is None:
        A("_No data._")
        write_files(run_dir, lines, agg, cells)
        return
    A(f"Headline numbers are for the representative workload: **{cell_label(primary)}** "
      f"(other workloads are tabulated below).\n")

    summary = rank_cell(agg[primary])
    # Best lossless tail latency
    lossless = [s for s in summary if agg[primary][s]["dropped"].mean == 0]
    if lossless:
        best_lat = min(lossless, key=lambda s: agg[primary][s]["p99_ns"].mean)
        a = agg[primary][best_lat]["p99_ns"]
        A(f"- **Lowest p99 hot-path latency (lossless):** `{best_lat}` — "
          f"{fmt_ci_ns(a)} over {agg[primary][best_lat]['_n_trials']} trials.")
    best_thr = max(summary, key=lambda s: agg[primary][s]["throughput"].mean)
    a = agg[primary][best_thr]["throughput"]
    A(f"- **Highest end-to-end throughput:** `{best_thr}` — {fmt_ci_rate(a)}.")

    # Best per family on p99
    A("\n**Best in each family (by mean p99 at the representative workload):**\n")
    fam_rows = []
    for fam in FAMILY_ORDER:
        members = [s for s in summary if FAMILY.get(s) == fam]
        if not members:
            continue
        best = min(members, key=lambda s: agg[primary][s]["p99_ns"].mean)
        a = agg[primary][best]["p99_ns"]
        t = agg[primary][best]["throughput"]
        fam_rows.append((fam, f"`{best}`", fmt_ci_ns(a), fmt_ci_rate(t)))
    A(md_table(["family", "best member", "p99 latency", "throughput"], fam_rows))

    # =====================================================================
    # Leaderboard at the primary cell (all strategies).
    # =====================================================================
    A(f"\n## Leaderboard — {cell_label(primary)}\n")
    note = oversub_note(primary[2], cores)
    if note:
        A(note + "\n")
    A("Sorted by mean p99 hot-path latency (lower is better). `±` is the 95% CI "
      "half-width across trials; `CV` is the coefficient of variation "
      "(run-to-run noise).\n")
    A(leaderboard_table(agg[primary]))

    sig = significance_notes(agg[primary])
    if sig:
        A("\n**Statistical significance notes:**\n")
        for s in sig:
            A(f"- {s}")

    # =====================================================================
    # Per-cell detailed tables.
    # =====================================================================
    A("\n## All workloads\n")
    for cell in cells:
        A(f"### {cell_label(cell)}\n")
        note = oversub_note(cell[2], cores)
        if note:
            A(note + "\n")
        A(leaderboard_table(agg[cell]))
        A("")

    # =====================================================================
    # Crate-difference narrative by family.
    # =====================================================================
    A("## Crate differences, by family\n")
    A(family_narrative(primary, agg))

    # =====================================================================
    # Caveats.
    # =====================================================================
    A("## Caveats & methodology notes\n")
    A(
        "- **Machine-specific.** These numbers reflect *this* host's CPU, "
        "allocator and disk. On a faster/slower disk or with a slow/bursty sink "
        "the ranking can change — re-run on your target hardware.\n"
        "- **One global logger per process.** The `log` facade and `tracing`'s "
        "global default subscriber can each only be installed once per process, "
        "so every global crate (`env_logger`, `fern`, `log4rs`, `flexi_logger`, "
        "all `tracing-*`, `ftlog`) is benchmarked in its own process. `slog` is "
        "the exception — a `slog::Logger` is a plain value.\n"
        "- **`tracing-nb` drain.** `tracing-appender`'s non-blocking worker has "
        "no public mid-life flush, so its per-case *drain* time is not separately "
        "captured; its hot-path latency (the headline metric) is measured "
        "exactly like everyone else's.\n"
        "- **Dropped counts.** `slog`, `tracing-*` and the synchronous `log` "
        "backends don't surface a programmatic dropped-record count, so `drop` "
        "reads 0 for them; run lossless (`--full-policy block`) for an "
        "apples-to-apples comparison (the default).\n"
        "- **Formatting is included on purpose.** Real-crate latencies include "
        "timestamp formatting and level filtering; the raw transport baselines "
        "do not. That gap *is* the cost of structured, human-readable logs.\n"
        "- **More producers than cores = thread-switching bound.** Any workload "
        "whose producer count exceeds this host's core count is oversubscribed "
        "(flagged inline above): the producers can't run simultaneously, so the "
        "measured hot-path latency is inflated by OS context-switch and "
        "run-queue-wait time rather than reflecting the logger itself. This is "
        "especially pronounced on core-limited embedded targets — to measure a "
        "logger's intrinsic cost there, keep producers ≤ cores.\n"
    )

    write_files(run_dir, lines, agg, cells)


def pick_primary_cell(cells, agg):
    """Prefer a mid-size payload, moderate producer count, max rate, block."""
    if not cells:
        return None
    def score(cell):
        size, cap, prod, rate, policy = cell
        return (
            0 if policy == "block" else 1,
            0 if rate == "max" else 1,
            abs(size - 512),
            abs(prod - 4),
        )
    return sorted(cells, key=score)[0]


def rank_cell(cell_aggs):
    return strategy_order(cell_aggs.keys())


def leaderboard_table(cell_aggs):
    strats = sorted(cell_aggs.keys(), key=lambda s: cell_aggs[s]["p99_ns"].mean)
    headers = ["#", "strategy", "family", "trials", "p50", "p99", "p99.9",
               "throughput", "p99 CV%"]
    rows = []
    for i, s in enumerate(strats, 1):
        m = cell_aggs[s]
        rows.append((
            i, f"`{s}`", FAMILY.get(s, "?"), m["_n_trials"],
            fmt_ci_ns(m["p50_ns"]),
            fmt_ci_ns(m["p99_ns"]),
            fmt_ci_ns(m["p999_ns"]),
            fmt_ci_rate(m["throughput"]),
            f"{m['p99_ns'].cv:.1f}" if m["p99_ns"].cv == m["p99_ns"].cv else "n/a",
        ))
    return md_table(headers, rows)


def significance_notes(cell_aggs):
    """Compare adjacent strategies (sorted by p99) and flag overlaps."""
    strats = sorted(cell_aggs.keys(), key=lambda s: cell_aggs[s]["p99_ns"].mean)
    notes = []
    for a, b in zip(strats, strats[1:]):
        am, bm = cell_aggs[a]["p99_ns"], cell_aggs[b]["p99_ns"]
        if am.n < 2 or bm.n < 2:
            continue
        if overlaps(am, bm):
            notes.append(
                f"`{a}` and `{b}` have overlapping 95% CIs on p99 "
                f"({fmt_ci_ns(am)} vs {fmt_ci_ns(bm)}) — **not statistically "
                f"distinguishable** at this workload.")
    if not notes:
        notes.append("Every adjacent pair in the leaderboard is separated by "
                     "more than its 95% CI — the ordering is statistically "
                     "robust on this machine.")
    return notes


def family_narrative(primary, agg):
    cell_aggs = agg[primary]
    out = []
    for fam in FAMILY_ORDER:
        members = [s for s in cell_aggs if FAMILY.get(s) == fam]
        if not members:
            continue
        members.sort(key=lambda s: cell_aggs[s]["p99_ns"].mean)
        out.append(f"### {fam}\n")
        rows = []
        for s in members:
            m = cell_aggs[s]
            rows.append((f"`{s}`", fmt_ci_ns(m["p50_ns"]), fmt_ci_ns(m["p99_ns"]),
                         fmt_ci_rate(m["throughput"])))
        out.append(md_table(["strategy", "p50", "p99", "throughput"], rows))
        out.append("")
    return "\n".join(out)


def write_files(run_dir, lines, agg, cells):
    report_path = os.path.join(run_dir, "REPORT.md")
    with open(report_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {report_path}")

    csv_path = os.path.join(run_dir, "summary_stats.csv")
    with open(csv_path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["msg_size", "capacity", "producers", "rate", "full_policy",
                    "strategy", "metric", "n_trials", "mean", "ci95_halfwidth",
                    "stdev", "cv_percent", "min", "median", "max"])
        for cell in cells:
            size, cap, prod, rate, policy = cell
            for strat, metrics in agg[cell].items():
                for name, m in metrics.items():
                    if name in ("_n_trials",):
                        continue
                    w.writerow([size, cap, prod, rate, policy, strat, name,
                                m.n, f"{m.mean:.4f}", f"{m.ci:.4f}",
                                f"{m.sd:.4f}", f"{m.cv:.2f}", f"{m.vmin:.4f}",
                                f"{m.median:.4f}", f"{m.vmax:.4f}"])
    print(f"wrote {csv_path}")


def main():
    run_dir = sys.argv[1] if len(sys.argv) > 1 else "overnight-out"
    if not os.path.isdir(run_dir):
        print(f"error: run directory '{run_dir}' not found", file=sys.stderr)
        sys.exit(1)
    records, meta, files = load(run_dir)
    if not records:
        print(f"error: no trial results found under {run_dir}/results/",
              file=sys.stderr)
        sys.exit(1)
    agg = build_aggregates(records)
    report(run_dir, agg, meta, files)


if __name__ == "__main__":
    main()
