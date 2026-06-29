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
  * plots.html           — interactive Bokeh charts of every metric vs. payload
                           size (one line per strategy), so you can see at a
                           glance where one crate overtakes another as messages
                           get larger.

No third-party *Python* dependencies — standard library only. The interactive
plots are emitted as a fully self-contained HTML file: BokehJS is inlined from
the bundles vendored under `scripts/vendor/bokehjs/`, so `plots.html` renders
offline (e.g. on an air-gapped device under test) with no CDN fetch and nothing
to `pip install`. If those vendored bundles are missing, the HTML falls back to
loading BokehJS from the Bokeh CDN.
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


def fmt_pct(p):
    if p != p:  # NaN
        return "n/a"
    if p >= 100:
        return f"{p:.0f}%"
    return f"{p:.1f}%"


def fmt_ci_pct(agg):
    if agg.n == 0:
        return "n/a"
    if agg.n < 2:
        return fmt_pct(agg.mean)
    return f"{fmt_pct(agg.mean)} ±{fmt_pct(agg.ci)}"


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
    "slog": "Structured logging",
    "slog-async": "Structured logging",
    "tracing-fmt": "Instrumentation (tracing)",
    "tracing-nb": "Instrumentation (tracing)",
    "tracing-span": "Instrumentation (tracing)",
    "tracing-json": "Instrumentation (tracing)",
    "ftlog": "High-throughput async",
}
FAMILY_ORDER = [
    "Transport baselines (raw bytes)",
    "`log` facade backends",
    "Structured logging",
    "Instrumentation (tracing)",
    "High-throughput async",
]

# Per-strategy reference card used to spell out *exactly what each line in the
# tables is exercising* — the crate behind it, a one-line description of the
# code path it drives, which of the four logging layers it touches (Facade /
# Structured / Formatting / Transport), and whether it installs a process-global
# logger. `layers` is what makes "these are layers, not rival choices" concrete:
# a raw transport touches only Transport; a full production stack touches all
# four. Keep this in sync with config.rs / the README strategy tables.
STRATEGY_INFO = {
    "direct": dict(
        crate="std", global_=False,
        desc="`Mutex<BufWriter<File>>` written on the calling thread — the honest synchronous baseline.",
        layers=("Transport",)),
    "crossbeam": dict(
        crate="crossbeam-channel", global_=False,
        desc="Hands the payload to one background writer thread over a `crossbeam-channel`.",
        layers=("Transport",)),
    "flume": dict(
        crate="flume", global_=False,
        desc="Same as `crossbeam` but over a `flume` channel → one background writer thread.",
        layers=("Transport",)),
    "tracing-appender": dict(
        crate="tracing-appender", global_=False,
        desc="`tracing-appender`'s `NonBlocking` queue carrying raw bytes — the transport with no formatting.",
        layers=("Transport",)),
    "env_logger": dict(
        crate="log + env_logger", global_=True,
        desc="`log` facade → `env_logger`, formatting + writing synchronously on the calling thread.",
        layers=("Facade", "Formatting", "Transport")),
    "fern": dict(
        crate="log + fern", global_=True,
        desc="`log` facade → `fern` dispatch, formatting + writing synchronously.",
        layers=("Facade", "Formatting", "Transport")),
    "log4rs": dict(
        crate="log + log4rs", global_=True,
        desc="`log` facade → `log4rs` appender pipeline, synchronous.",
        layers=("Facade", "Formatting", "Transport")),
    "flexi_logger": dict(
        crate="flexi_logger", global_=True,
        desc="`log` facade → `flexi_logger` buffered writer.",
        layers=("Facade", "Formatting", "Transport")),
    "slog": dict(
        crate="slog", global_=False,
        desc="`slog` structured key/value fields formatted synchronously (a plain value, not a global).",
        layers=("Facade", "Structured", "Formatting", "Transport")),
    "slog-async": dict(
        crate="slog + slog-async", global_=False,
        desc="`slog` structured fields handed to a `slog-async` background drain thread.",
        layers=("Facade", "Structured", "Formatting", "Transport")),
    "tracing-fmt": dict(
        crate="tracing + tracing-subscriber", global_=True,
        desc="`tracing` facade → `fmt` subscriber, formatting + writing synchronously.",
        layers=("Facade", "Formatting", "Transport")),
    "tracing-nb": dict(
        crate="tracing + tracing-appender", global_=True,
        desc="`tracing` facade → `fmt` formatting → `tracing-appender` non-blocking transport.",
        layers=("Facade", "Formatting", "Transport")),
    "tracing-span": dict(
        crate="tracing", global_=True,
        desc="`tracing-fmt` with a span entered/exited around every event (the span-overhead variant).",
        layers=("Facade", "Formatting", "Transport")),
    "tracing-json": dict(
        crate="tracing + JSON + tracing-appender", global_=True,
        desc="The full production stack: structured fields → JSON formatter → non-blocking async transport.",
        layers=("Facade", "Structured", "Formatting", "Transport")),
    "ftlog": dict(
        crate="ftlog", global_=True,
        desc="`log` facade → `ftlog`'s dedicated-thread, batched high-throughput transport.",
        layers=("Facade", "Formatting", "Transport")),
}

# BokehJS version vendored under scripts/vendor/bokehjs/ and inlined into
# plots.html so the report is fully self-contained / offline-viewable. If the
# vendored files are missing we fall back to loading this version from the CDN.
BOKEHJS_VERSION = "3.4.1"
_VENDOR_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           "vendor", "bokehjs")


def bokehjs_script_tags():
    """Return <script> tags for BokehJS: vendored+inlined if present, else CDN.

    Inlining makes plots.html work with no network at view time (air-gapped
    devices, archived reports). The minified bundles contain no literal
    ``</script>`` sequence, but we defensively neutralise any just in case so the
    embedded script can never be terminated early.
    """
    files = [f"bokeh-{BOKEHJS_VERSION}.min.js",
             f"bokeh-api-{BOKEHJS_VERSION}.min.js"]
    paths = [os.path.join(_VENDOR_DIR, f) for f in files]
    if all(os.path.exists(p) for p in paths):
        out = []
        for p in paths:
            with open(p, encoding="utf-8") as fh:
                js = fh.read().replace("</script>", "<\\/script>")
            out.append(f"<script>\n{js}\n</script>")
        return "\n".join(out)
    # Fallback: load from the Bokeh CDN (needs network at view time).
    base = "https://cdn.bokeh.org/bokeh/release"
    return "\n".join(
        f'<script src="{base}/{f}"></script>' for f in files)


# A stable, high-contrast colour per strategy for the interactive plots. Assigned
# by sorted strategy name so a given crate keeps its colour across runs.
_PALETTE = [
    "#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd", "#8c564b",
    "#e377c2", "#7f7f7f", "#bcbd22", "#17becf", "#393b79", "#637939",
    "#8c6d31", "#843c39", "#7b4173", "#3182bd", "#e6550d", "#31a354",
    "#756bb1", "#636363",
]


def strategy_colors(strats):
    """strategy -> hex colour, stable across runs (assigned by sorted name)."""
    return {s: _PALETTE[i % len(_PALETTE)] for i, s in enumerate(sorted(strats))}


# Metrics rendered as "metric vs. payload size" curves in plots.html, plus the
# crossover narrative. `axis` is the y-scale; `better` notes the good direction.
PLOT_METRICS = [
    dict(key="p50_ns", label="p50 hot-path latency", unit="ns", axis="log", better="lower"),
    dict(key="p99_ns", label="p99 hot-path latency", unit="ns", axis="log", better="lower"),
    dict(key="p999_ns", label="p99.9 hot-path latency", unit="ns", axis="log", better="lower"),
    dict(key="throughput", label="end-to-end throughput", unit="rec/s", axis="log", better="higher"),
    dict(key="slowdown_pct", label="program slowdown", unit="%", axis="log", better="lower"),
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
        with open(meta_path, encoding="utf-8") as f:
            meta = json.load(f)

    records = []
    pattern = os.path.join(run_dir, "results", "*", "trial_*.json")
    files = sorted(glob.glob(pattern))
    for path in files:
        trial = os.path.basename(path).replace("trial_", "").replace(".json", "")
        try:
            with open(path, encoding="utf-8") as f:
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
            # Program slowdown from the interleaved-work model. Absent / null in
            # older runs (or when lines_per_log=0); Agg drops the Nones so such
            # cells simply report n/a.
            metrics["slowdown_pct"] = Agg([r.get("slowdown_pct") for r in rows])
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
                  "rustc", "build_host", "remote", "git_commit", "started",
                  "finished", "trials_requested", "messages", "warmup",
                  "writer_buf", "full_policy", "msg_sizes", "producers",
                  "rates", "buffers", "lines_per_log"):
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
    any_slowdown = any(
        agg[c][s]["slowdown_pct"].n > 0 for c in agg for s in agg[c]
    )
    if any_slowdown:
        lpl = meta.get("lines_per_log")
        every = f"every **{lpl}** lines of code" if lpl else "every N lines of code"
        A(
            f"**Program slowdown.** To turn the per-call cost into an applied "
            f"figure, each case also models logging interleaved with real work: "
            f"the producer runs a calibrated chunk of synthetic CPU work "
            f"(standing in for {every}) between consecutive `log()` calls. Every "
            f"case is timed twice — once running only that work (the no-logging "
            f"baseline) and once running the work *and* the `log()` calls — and "
            f"the **slowdown** is how much longer the logged run took as a "
            f"percentage of the baseline (`100 × logging_time / work_time`). It "
            f"answers \"how much does this logger slow my program down?\" rather "
            f"than just \"how long is one `log()` call?\".\n"
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
    # What is being tested — long-form description of strategies & test types.
    # =====================================================================
    A(whats_being_tested_section(all_strats, cells))

    A("> 📈 **Interactive plots:** open [`plots.html`](plots.html) next to this "
      "report for metric-vs-payload-size charts (one line per strategy, log axes, "
      "hover for the 95% CI, click the legend to mute a strategy). That view is "
      "the quickest way to spot a logger that wins on small messages but loses on "
      "large ones.\n")

    # =====================================================================
    # Executive summary — pick a representative "primary" cell.
    # =====================================================================
    A("## Executive summary\n")
    primary = pick_primary_cell(cells, agg)
    if primary is None:
        A("_No data._")
        write_files(run_dir, lines, agg, cells, meta)
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
    # Smallest program slowdown (only when the work model was enabled).
    slow = [s for s in summary if agg[primary][s]["slowdown_pct"].n > 0]
    if slow:
        best_slow = min(slow, key=lambda s: agg[primary][s]["slowdown_pct"].mean)
        a = agg[primary][best_slow]["slowdown_pct"]
        lpl = meta.get("lines_per_log")
        every = f" when logging every {lpl} lines of code" if lpl else ""
        A(f"- **Smallest program slowdown{every}:** `{best_slow}` — "
          f"{fmt_ci_pct(a)} slower than the same work with no logging.")

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
    # Payload-size crossovers — make rank changes across message sizes explicit.
    # =====================================================================
    crossover = payload_ranking_section(agg, cells, primary)
    if crossover:
        A("\n" + crossover)

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

    write_files(run_dir, lines, agg, cells, meta)


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
    # Only show the slowdown column when at least one strategy actually measured
    # it (the work model was enabled for this run).
    has_slowdown = any(cell_aggs[s]["slowdown_pct"].n > 0 for s in strats)
    headers = ["#", "strategy", "family", "trials", "p50", "p99", "p99.9",
               "throughput", "p99 CV%"]
    if has_slowdown:
        headers.append("slowdown")
    rows = []
    for i, s in enumerate(strats, 1):
        m = cell_aggs[s]
        row = [
            i, f"`{s}`", FAMILY.get(s, "?"), m["_n_trials"],
            fmt_ci_ns(m["p50_ns"]),
            fmt_ci_ns(m["p99_ns"]),
            fmt_ci_ns(m["p999_ns"]),
            fmt_ci_rate(m["throughput"]),
            f"{m['p99_ns'].cv:.1f}" if m["p99_ns"].cv == m["p99_ns"].cv else "n/a",
        ]
        if has_slowdown:
            row.append(fmt_ci_pct(m["slowdown_pct"]))
        rows.append(tuple(row))
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


def whats_being_tested_section(all_strats, cells):
    """A long-form 'what exactly is on the table here' section.

    Spells out the unit under test (one `log()` call), the synthetic payload, the
    swept workload axes, the concrete strategies (with the layers each exercises),
    and the distinct *kinds of measurement* (the test types) so a reader never has
    to guess what a number means.
    """
    out = []
    A = out.append

    A("## What is being tested\n")
    A(
        "The unit under test is **a single `log()` call on a producer thread** — "
        "the moment your application stops doing its own work to emit one log "
        "record. Every number in this report is derived from timing that call, "
        "across many strategies and many workloads, so you can see not just *which "
        "logger is fastest* but *under which conditions* and *by how much beyond "
        "the run-to-run noise*.\n"
    )
    A("### The record being logged\n")
    sizes = sorted({c[0] for c in cells})
    size_list = ", ".join(f"**{s} B**" for s in sizes) if sizes else "the configured sizes"
    A(
        f"Each producer emits a fixed-size byte payload as the log message. This "
        f"run sweeps payload sizes of {size_list}. Payload size is the axis this "
        f"report leans on hardest: a logger that wins on tiny 64 B lines can lose "
        f"badly once messages grow, because per-record formatting, copying and "
        f"transport costs scale differently. The transport baselines write those "
        f"bytes raw; the real crates wrap them in a level, a timestamp and their "
        f"own framing first.\n"
    )

    A("### The workload axes (what we sweep)\n")
    axis_rows = [
        ("payload size", "Bytes per log record.",
         "Bigger records cost more to format, copy and flush — the main driver of crossovers."),
        ("producers", "Concurrent threads all logging at once.",
         "Exposes lock / queue contention; more producers than cores is flagged as oversubscribed."),
        ("buffer capacity", "Records the async channel / queue can hold.",
         "A bigger buffer absorbs bursts before back-pressure (block) or drops (drop) kick in."),
        ("rate", "Target records/second per producer (`max` = unthrottled).",
         "Throttled runs measure steady-state cost; `max` measures the logger flat-out."),
        ("full policy", "`block` (lossless back-pressure) vs `drop` (shed load).",
         "`block` guarantees every record lands; `drop` trades records for never stalling the producer."),
        ("lines-per-log", "Synthetic code-lines of real work between `log()` calls.",
         "Turns per-call latency into an applied *program slowdown* (see below)."),
    ]
    A(md_table(["axis", "what it is", "why it matters"], axis_rows))
    A("")

    A("### The strategies under test\n")
    A(
        "Each strategy drives a different code path for the same `log()` call. The "
        "**layers** column makes the central point of this suite concrete: these "
        "are not rival choices but *stacked layers* — a raw transport touches only "
        "**Transport**, a synchronous facade backend adds **Facade + Formatting**, "
        "and a full production stack touches all four. The transport baselines pay "
        "**no formatting cost** and exist purely as an honest reference point for "
        "what the real crates add.\n"
    )
    present = [s for s in strategy_order(all_strats)]
    rows = []
    for s in present:
        info = STRATEGY_INFO.get(s)
        if not info:
            rows.append((f"`{s}`", "?", "?", "?", "?"))
            continue
        layers = " + ".join(info["layers"])
        rows.append((
            f"`{s}`", f"`{info['crate']}`", info["desc"],
            layers, "yes" if info["global_"] else "no",
        ))
    A(md_table(["strategy", "crate(s)", "what its `log()` call exercises",
                "layers", "global?"], rows))
    A("")
    A("_Layer key — **Facade**: the call-site API that decouples call sites from the "
      "backend. **Structured**: real key/value fields, not just a message string. "
      "**Formatting**: turning a record into bytes (text / JSON). **Transport**: "
      "getting those bytes off the hot path (a lock, a channel, a non-blocking "
      "queue, a dedicated thread)._\n")

    A("### The kinds of measurement (test types)\n")
    A(
        "For every (strategy × workload) cell we capture several distinct metrics. "
        "They answer different questions, so read them together rather than picking "
        "one:\n"
    )
    metric_rows = [
        ("Hot-path latency — p50",
         "Median time the producer thread is held inside `log()`.",
         "lower", "The typical cost you pay on every line."),
        ("Hot-path latency — p99 / p99.9",
         "Tail of that same per-call latency distribution (HdrHistogram).",
         "lower", "The stalls that actually hurt — a logger with a great median but a long tail will hitch your request handler."),
        ("Hot-path latency — max",
         "Worst single `log()` call observed.",
         "lower", "Worst-case pause; dominated by buffer-full / flush events."),
        ("End-to-end throughput",
         "Records/second start-to-finish, **including the final drain/flush**.",
         "higher", "Sustained volume the whole pipeline can clear, not just enqueue."),
        ("Drain / flush cost",
         "Time `finish()` blocks to make every buffered record durable.",
         "lower", "Async loggers move cost here; a cheap hot path with an enormous drain isn't free."),
        ("Dropped records",
         "Records shed under the `drop` policy when the buffer is full.",
         "lower", "Only meaningful under `--full-policy drop`; lossless `block` runs report 0."),
    ]
    metric_rows.append((
        "Program slowdown",
        "How much longer the *same work* runs when it also logs every "
        "`lines-per-log` lines (`100 × logging_time / work_time`).",
        "lower",
        "Translates per-call latency into 'how much slower is my program?' — the applied bottom line.",
    ))
    A(md_table(["metric (test type)", "what it measures", "good direction", "what it tells you"], metric_rows))
    A("")
    A(
        "**Statistical reading.** Each trial is one independent observation; we "
        "report the **mean ± the 95% confidence interval** across trials. Two "
        "strategies whose intervals overlap are *not statistically "
        "distinguishable* on this machine — treat them as tied rather than reading "
        "a winner into the noise.\n"
    )
    return "\n".join(out)


def _metric_at(agg, cell, strat, key):
    """Mean of `key` for (cell, strat), or None if absent / unmeasured."""
    cm = agg.get(cell)
    if not cm or strat not in cm:
        return None
    m = cm[strat].get(key)
    if m is None or m.n == 0 or m.mean != m.mean:
        return None
    return m.mean


def payload_ranking_section(agg, cells, primary):
    """Make payload-size crossovers explicit in text (not just in the plots).

    For the primary scenario (its buffer / rate / policy), and for each producer
    count present, rank the strategies by p99 at the **smallest** payload and at
    the **largest** payload, then surface who moved — and call out the actual
    order-swaps where A beats B on small messages but loses on large ones.
    """
    if primary is None:
        return ""
    _, p_cap, _, p_rate, p_policy = primary
    sizes = sorted({c[0] for c in cells
                    if c[1] == p_cap and c[3] == p_rate and c[4] == p_policy})
    if len(sizes) < 2:
        return ""  # need at least two payload sizes to talk about crossovers
    small, large = sizes[0], sizes[-1]
    producers = sorted({c[2] for c in cells
                        if c[1] == p_cap and c[3] == p_rate and c[4] == p_policy})

    out = []
    A = out.append
    A("## How payload size changes the ranking\n")
    cap_s = "∞" if p_cap == 0 else str(p_cap)
    A(
        f"This is the question the leaderboards alone can't answer: *does the "
        f"winner on small messages stay the winner on large ones?* Below, for the "
        f"primary scenario (buffer {cap_s} · rate {p_rate} · {p_policy}), each "
        f"strategy is ranked by **p99 hot-path latency** at the smallest payload "
        f"(**{small} B**) and again at the largest (**{large} B**). `Δrank` is how "
        f"many places it moved (▲ = better on large messages, ▼ = worse); `growth` "
        f"is its p99 at {large} B divided by its p99 at {small} B. The interactive "
        f"`plots.html` shows the full curve across every size.\n"
    )
    for prod in producers:
        small_cell = (small, p_cap, prod, p_rate, p_policy)
        large_cell = (large, p_cap, prod, p_rate, p_policy)
        strats = sorted(
            {s for s in agg.get(small_cell, {})} & {s for s in agg.get(large_cell, {})},
        )
        vals = []
        for s in strats:
            vs = _metric_at(agg, small_cell, s, "p99_ns")
            vl = _metric_at(agg, large_cell, s, "p99_ns")
            if vs is None or vl is None:
                continue
            vals.append((s, vs, vl))
        if len(vals) < 2:
            continue
        rank_small = {s: i for i, (s, _, _) in
                      enumerate(sorted(vals, key=lambda t: t[1]), 1)}
        rank_large = {s: i for i, (s, _, _) in
                      enumerate(sorted(vals, key=lambda t: t[2]), 1)}
        A(f"\n### {prod} producer(s)\n")
        rows = []
        for s, vs, vl in sorted(vals, key=lambda t: t[2]):
            d = rank_small[s] - rank_large[s]  # positive => improved on large
            if d > 0:
                arrow = f"▲ {d}"
            elif d < 0:
                arrow = f"▼ {-d}"
            else:
                arrow = "—"
            growth = vl / vs if vs else float("nan")
            growth_s = f"{growth:.1f}×" if growth == growth else "n/a"
            rows.append((
                f"`{s}`",
                f"{rank_small[s]} ({fmt_ns(vs)})",
                f"{rank_large[s]} ({fmt_ns(vl)})",
                arrow, growth_s,
            ))
        A(md_table(["strategy", f"rank @ {small} B", f"rank @ {large} B",
                    "Δrank", "p99 growth"], rows))

        # Explicit order-swaps: pairs that trade places between small and large.
        swaps = []
        order = [s for s, _, _ in sorted(vals, key=lambda t: t[1])]
        for i in range(len(order)):
            for j in range(i + 1, len(order)):
                a, b = order[i], order[j]  # a beats b at small payload
                if rank_large[a] > rank_large[b]:  # ...but loses at large
                    swaps.append((a, b))
        # Keep the most dramatic few (largest combined rank movement).
        swaps.sort(key=lambda ab: (rank_large[ab[0]] - rank_small[ab[0]]), reverse=True)
        for a, b in swaps[:4]:
            A(f"- **Crossover:** `{a}` beats `{b}` on {small} B "
              f"({fmt_ns(_metric_at(agg, small_cell, a, 'p99_ns'))} vs "
              f"{fmt_ns(_metric_at(agg, small_cell, b, 'p99_ns'))}) but `{b}` "
              f"wins on {large} B "
              f"({fmt_ns(_metric_at(agg, large_cell, b, 'p99_ns'))} vs "
              f"{fmt_ns(_metric_at(agg, large_cell, a, 'p99_ns'))}).")
    A("")
    return "\n".join(out)


# --- interactive Bokeh plots ------------------------------------------------

def build_plot_sections(agg, cells):
    """Shape the aggregates into the JSON the plots.html template consumes.

    One *section* per scenario (buffer × rate × policy). Within a section, one
    *row* per metric, and within a row one *panel* (figure) per producer count.
    Each panel carries one *series* per strategy: x = payload sizes, y = the
    metric's mean, with lo/hi = the 95% CI for the hover tooltip.
    """
    all_strats = set()
    for c in cells:
        all_strats.update(agg[c].keys())
    colors = strategy_colors(all_strats)

    # scenario -> sizes/producers present
    scen_sizes = defaultdict(set)
    scen_prods = defaultdict(set)
    scen_strats = defaultdict(set)
    for (size, cap, prod, rate, policy) in cells:
        scen = (cap, rate, policy)
        scen_sizes[scen].add(size)
        scen_prods[scen].add(prod)
        scen_strats[scen].update(agg[(size, cap, prod, rate, policy)].keys())

    # Which metrics actually have data anywhere (slowdown may be absent).
    metrics = []
    for md in PLOT_METRICS:
        has = any(
            (agg[c].get(s, {}).get(md["key"]) is not None
             and agg[c][s][md["key"]].n > 0)
            for c in cells for s in agg[c]
        )
        if has:
            metrics.append(md)

    sections = []
    for scen in sorted(scen_sizes):
        cap, rate, policy = scen
        sizes = sorted(scen_sizes[scen])
        if len(sizes) < 1:
            continue
        producers = sorted(scen_prods[scen])
        order = strategy_order(scen_strats[scen])
        cap_s = "∞" if cap == 0 else str(cap)
        scen_label = f"buffer {cap_s} · rate {rate} · {policy}"

        rows = []
        for md in metrics:
            key = md["key"]
            panels = []
            for prod in producers:
                series = []
                for s in order:
                    xs, ys, los, his = [], [], [], []
                    for size in sizes:
                        cell = (size, cap, prod, rate, policy)
                        m = agg.get(cell, {}).get(s, {}).get(key)
                        if m is None or m.n == 0 or m.mean != m.mean:
                            continue
                        xs.append(size)
                        ys.append(round(m.mean, 4))
                        los.append(round(m.lo if m.n > 1 else m.mean, 4))
                        his.append(round(m.hi if m.n > 1 else m.mean, 4))
                    if xs:
                        series.append(dict(name=s, family=FAMILY.get(s, "?"),
                                           color=colors[s], x=xs, y=ys,
                                           lo=los, hi=his))
                if series:
                    panels.append(dict(
                        title=f"{prod} producer(s)", series=series,
                        y_axis_type=md["axis"], unit=md["unit"],
                        metric_label=md["label"]))
            if panels:
                rows.append(dict(metric_label=md["label"],
                                 better=md["better"], unit=md["unit"],
                                 panels=panels))
        if rows:
            sections.append(dict(label=scen_label, sizes=sizes, rows=rows))
    return sections


# Static JS that turns the embedded PLOT_DATA into a grid of BokehJS figures.
# Kept out of any f-string so the JS braces don't need escaping; data is spliced
# in by replacing the __PLOT_DATA__ / __META_LINE__ markers.
_PLOTS_JS = r"""
function fig(panel) {
  const p = Bokeh.Plotting.figure({
    title: panel.title,
    width: 470, height: 360,
    x_axis_type: 'log', y_axis_type: panel.y_axis_type,
    x_axis_label: 'payload size (bytes)',
    y_axis_label: panel.metric_label + (panel.unit ? ' (' + panel.unit + ')' : ''),
    tools: 'pan,wheel_zoom,box_zoom,reset,save',
    toolbar_location: 'above',
    sizing_mode: 'fixed'
  });
  const circles = [];
  for (const s of panel.series) {
    const n = s.x.length;
    const src = new Bokeh.ColumnDataSource({ data: {
      x: s.x, y: s.y, lo: s.lo, hi: s.hi,
      name: Array(n).fill(s.name), fam: Array(n).fill(s.family)
    }});
    p.line({field: 'x'}, {field: 'y'},
      {source: src, line_color: s.color, line_width: 2, legend_label: s.name});
    const c = p.scatter({field: 'x'}, {field: 'y'},
      {source: src, marker: 'circle', size: 8,
       fill_color: s.color, line_color: s.color, legend_label: s.name});
    circles.push(c);
  }
  const hover = new Bokeh.HoverTool({
    renderers: circles,
    tooltips: [
      ['strategy', '@name'], ['family', '@fam'],
      ['payload', '@x B'],
      [panel.metric_label, '@y' + (panel.unit ? ' ' + panel.unit : '')],
      ['95% CI', '@lo … @hi']
    ]
  });
  p.add_tools(hover);
  // Pin x ticks to the actual payload sizes (a log axis with a few points
  // otherwise crams auto-ticks like "3000 4000 5000" on top of each other).
  try {
    const xs = new Set();
    panel.series.forEach(s => s.x.forEach(v => xs.add(v)));
    const ticks = Array.from(xs).sort((a, b) => a - b);
    const axes = Array.isArray(p.xaxis) ? p.xaxis : [p.xaxis];
    for (const ax of axes) {
      ax.ticker = new Bokeh.FixedTicker({ ticks: ticks });
      ax.major_label_overrides = Object.fromEntries(ticks.map(t => [t, String(t)]));
    }
  } catch (e) { /* tick pinning is best-effort */ }
  try {
    const lg = Array.isArray(p.legend) ? p.legend[0] : p.legend;
    if (lg) {
      lg.click_policy = 'hide';
      lg.label_text_font_size = '8pt';
      lg.location = 'top_left';
      lg.background_fill_alpha = 0.7;
      lg.padding = 4;
      lg.spacing = 0;
    }
  } catch (e) { /* legend styling is best-effort */ }
  return p;
}

function render() {
  const root = document.getElementById('plots');
  const data = window.PLOT_DATA;
  for (const section of data.sections) {
    const h = document.createElement('h2');
    h.textContent = section.label;
    root.appendChild(h);
    for (const row of section.rows) {
      const cap = document.createElement('p');
      cap.className = 'metric-cap';
      cap.innerHTML = '<b>' + row.metric_label + '</b> — ' +
        (row.better === 'higher' ? 'higher is better' : 'lower is better') +
        '. Click a legend entry to hide that strategy; hover a point for its 95% CI.';
      root.appendChild(cap);
      const figs = row.panels.map(fig);
      const grid = Bokeh.Plotting.gridplot([figs],
        {toolbar_location: 'right', merge_tools: true});
      const holder = document.createElement('div');
      holder.className = 'row';
      root.appendChild(holder);
      Bokeh.Plotting.show(grid, holder);
    }
  }
}
document.addEventListener('DOMContentLoaded', render);
"""


def write_plots_html(run_dir, sections, meta):
    """Write a self-contained interactive plots.html.

    BokehJS is inlined from the vendored bundles so the file works offline; if
    those bundles are missing it falls back to the Bokeh CDN.
    """
    path = os.path.join(run_dir, "plots.html")
    if not sections:
        # Still leave a stub so the link in REPORT.md isn't dangling.
        with open(path, "w", encoding="utf-8") as f:
            f.write("<!doctype html><meta charset=utf-8><title>logbench plots</title>"
                    "<p>No plottable data in this run.</p>")
        print(f"wrote {path} (empty — no plottable data)")
        return

    host = meta.get("host", "this machine")
    cpu = meta.get("cpu_model", "")
    data_json = json.dumps({"sections": sections}, separators=(",", ":"))
    meta_line = (f"{host} — {cpu}" if cpu else host)

    html = _PLOTS_TEMPLATE
    html = html.replace("__PLOT_DATA__", data_json)
    html = html.replace("__META_LINE__", json.dumps(meta_line))
    html = html.replace("__PLOTS_JS__", _PLOTS_JS)
    # Replace the BokehJS marker last: the inlined bundle is ~1 MB and must not be
    # scanned for the other (already-substituted) markers.
    bokeh_tags = bokehjs_script_tags()
    html = html.replace("__BOKEHJS__", bokeh_tags)
    with open(path, "w", encoding="utf-8") as f:
        f.write(html)
    inlined = bokeh_tags.lstrip().startswith("<script>")
    print(f"wrote {path}" + ("" if inlined else " (BokehJS via CDN — vendored bundles not found)"))


_PLOTS_TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>logbench — performance vs. payload size</title>
__BOKEHJS__
<style>
  body { font-family: -apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif;
         margin: 1.5rem auto; max-width: 1100px; color: #1a1a1a; padding: 0 1rem; }
  h1 { font-size: 1.5rem; margin-bottom: .2rem; }
  h2 { margin-top: 2rem; border-bottom: 2px solid #ddd; padding-bottom: .25rem; }
  .sub { color: #555; margin-top: 0; }
  .metric-cap { color: #444; margin: 1rem 0 .25rem; }
  .row { display: flex; flex-wrap: wrap; gap: 1rem; }
  .note { background: #f6f8fa; border: 1px solid #e1e4e8; border-radius: 6px;
          padding: .75rem 1rem; margin: 1rem 0; font-size: .92rem; }
  code { background: #f0f0f0; padding: 0 .25rem; border-radius: 3px; }
</style>
</head>
<body>
<h1>logbench — performance vs. payload size</h1>
<p class="sub" id="metaline"></p>
<div class="note">
  Each chart plots one metric against <b>payload size</b> (log x-axis), with one
  line per strategy. This is the view that answers
  <i>"is this logger better on small messages but worse on large ones?"</i> —
  watch for lines that <b>cross</b>. <b>Click</b> a legend entry to mute that
  strategy; <b>hover</b> a marker for its mean and 95% confidence interval. Charts
  are grouped by scenario (buffer · rate · policy), then by metric, with one panel
  per producer count. Latency and throughput use a log y-axis. This file is fully
  self-contained — BokehJS is embedded, so it renders offline.
</div>
<div id="plots"></div>
<script>window.PLOT_DATA = __PLOT_DATA__;</script>
<script>document.getElementById('metaline').textContent = __META_LINE__;</script>
<script>__PLOTS_JS__</script>
</body>
</html>
"""


def write_files(run_dir, lines, agg, cells, meta):
    report_path = os.path.join(run_dir, "REPORT.md")
    with open(report_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {report_path}")

    csv_path = os.path.join(run_dir, "summary_stats.csv")
    with open(csv_path, "w", newline="", encoding="utf-8") as f:
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

    # Interactive metric-vs-payload-size charts.
    sections = build_plot_sections(agg, cells)
    write_plots_html(run_dir, sections, meta)


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
