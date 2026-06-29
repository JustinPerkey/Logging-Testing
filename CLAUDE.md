# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`logbench` is a self-contained Rust suite that benchmarks real Rust logging
crates (`env_logger`, `fern`, `log4rs`, `flexi_logger`, `slog`, the `tracing`
stack, `ftlog`) against each other and against raw async transport baselines
(`direct`, `crossbeam`, `flume`, `tracing-appender`). The metric that matters
most is **how long the `log()` call holds the producing thread** — the hot-path
latency distribution (p50/p99/p99.9/max) — alongside throughput, drain cost and
dropped records. It also turns that per-call cost into an applied figure: the
**program slowdown** of logging interleaved with real work (`--lines-per-log N`,
default 30), i.e. how much slower your program runs when it emits a log line
every ~N lines of code. It is a `[lib]` (the reusable engine) plus a `[[bin]]`
(the CLI that drives it).

## Commands

Always benchmark in release — the `[profile.release]` enables LTO + `opt-level=3`.

```bash
cargo build --release
./target/release/logbench                          # default: the 4 transport baselines
./target/release/logbench --strategies tracing-fmt # one real crate (see global note)
./target/release/logbench --strategies every       # all 14 (globals get skipped, see below)

cargo test --release                               # integration + unit tests
cargo test --release durably                       # run a single test by name substring
cargo bench                                         # Criterion micro-benchmarks → target/criterion/

scripts/overnight.sh                               # full statistically-significant comparison → overnight-out/REPORT.md
SMOKE=1 scripts/overnight.sh                        # ~1 min end-to-end pipeline validation → smoke-out/
python3 scripts/aggregate.py overnight-out          # re-aggregate an existing run directory
```

`bench-out/`, `overnight-out/`, `smoke-out/` and `*.log` are gitignored. The
curated `sample-report/` IS committed as an illustrative example of overnight output.

## The one-global-logger-per-process constraint

This is the central architectural fact and the source of most surprises:

- The `log` facade's global logger and `tracing`'s global default subscriber
  can each be installed **only once per process**. `Strategy::is_global()` marks
  which strategies do this (all real crates **except `slog`**, which is an
  ordinary value).
- `loggers::claim_global()` enforces this with a process-wide `Mutex<Option<Strategy>>`.
  In a single `logbench` run, the first global strategy installs; later *different*
  globals return `ErrorKind::AlreadyExists` and `main.rs` **skips** them with a message.
- The fix is one process per strategy. `scripts/overnight.sh` does exactly this —
  it runs each strategy in its own process, repeated over many **trials**, and
  reshuffles strategy order each trial to defeat thermal drift. `aggregate.py`
  treats each trial as one observation and reports means ± 95% CI (Student's t),
  flagging overlapping intervals as not statistically distinguishable.

When adding/modifying strategies, keep `is_global()` accurate or the overnight
harness and single-process skip logic will misbehave.

## Architecture

The engine sweeps the cartesian product of strategy × msg-size × buffer ×
producers × rate × lines-per-log. For each cell, `runner::run_case` spawns
`producers` threads that each emit a fixed payload, barrier-synced so they start
the measured phase together; it times each `log()` call into an HdrHistogram,
then calls `Logger::finish()` to drain/flush and times that separately.

When `lines_per_log > 0` the runner also measures the **program slowdown**: each
producer first runs a no-logging baseline phase (the same calibrated synthetic
work, `runner::do_work`, with the `log()` calls removed) and then the measured
phase (work + `log()`), both barrier-synced and wall-clock-timed. The slowdown
is `100 × log_busy / baseline`, where `log_busy` is the time the producer
actually spent inside `log()` (the summed per-call latencies), **not** the
measured-phase wall clock. Using the in-`log()` time keeps the slowdown
independent of any `--rates` pacing: when a target rate is set the producer
sleeps between calls, and that idle time must not be counted as logging cost.
`runner::calibrate_ns_per_line()`
reports the per-line work cost once for context. `lines_per_log == 0` skips the
baseline phase entirely and the run is a pure back-to-back logging measurement
(historical default behaviour).

Source layout (`src/`):

- `config.rs` — `Strategy` (the 14 variants + `ALL`/`CRATES`/`EVERY` sets,
  `name()`, `is_global`/`is_real_crate`/`is_async`/`is_combined`, `FromStr` aliases), `FullPolicy`
  (`block` = lossless back-pressure / `drop` = lossy), `LoggerConfig`, `Workload`
  (incl. `lines_per_log`, the inter-log synthetic-work knob).
- `runner.rs` — the measurement loop (`run_case`, `run_producer`, payload
  generation, absolute-timeline rate pacing, the `do_work` synthetic-work model
  and `calibrate_ns_per_line`).
- `metrics.rs` — `CaseResult` (incl. `slowdown_pct`/`work_only_secs`/
  `lines_per_log`/`ns_per_line`) and HdrHistogram aggregation;
  `fmt_ns`/`fmt_rate`/`fmt_slowdown` helpers.
- `report.rs` — CSV / JSON / console table / plain-language recommendations.
- `loggers/` — one module per strategy family, all behind the `Logger` trait.

### The `Logger` trait (`src/loggers/mod.rs`)

Every strategy implements two methods:

- `log(&self, record: &[u8])` — the hot path being measured; for async
  strategies this hands off without blocking (subject to `FullPolicy`).
- `finish(&self) -> u64` — flush/drain blocking until durable; returns dropped count.
  For non-global strategies this also tears the logger down; for globals it only flushes.

Implementations must be `Send + Sync` (the runner clones an `Arc<dyn Logger>`
into each producer). `loggers::build()` is the single dispatch point from
`Strategy` to a concrete logger.

Two families: **transport baselines** (`direct`, `channel`, `tracing_nb`) write
raw payload bytes with no formatting; **real crates** (`log_facade`,
`slog_logger`, `tracing_full`, `ftlog_logger`) drive the crate's real macro →
format → sink path. Real-crate backends use `record_str()` to turn the payload
into the message string they log.

These two families isolate one logging concern at a time, but the concerns —
**facade**, **structured** fields, **formatting**, async **transport** — are
layers a real solution composes, not rival choices. Several strategies are
already combinations (`tracing-nb` = facade + fmt + async; `slog-async` =
structured + async; `ftlog` = facade + dedicated-thread transport). The
`tracing-json` strategy (`tracing_full::build_json`, marked by
`Strategy::is_combined`) deliberately stacks all four layers — structured fields
+ JSON formatting + non-blocking async transport — to model a realistic
production stack rather than one isolated layer. The README's "These 'types' are
layers, not separate options" section is the canonical write-up of this framing;
keep it in sync when adding combinations.

### Adding a strategy

Implement `Logger` (`log` + `finish`) in `src/loggers/`, then wire it into the
`Strategy` enum (`config.rs`: variant, `name()`, the `ALL`/`CRATES`/`EVERY`
arrays, the `is_*` predicates, `FromStr`) and into `loggers::build()`. If it
installs a process-global, call `claim_global()` in its constructor.

## Cross-device test/bench running

`logbench` measures a *specific* machine, so you may build on one host and run
on another. `.cargo/config.toml` wires a Cargo target runner
(`scripts/remote-test-runner.sh`) for `cfg(all())`. With `LOGBENCH_REMOTE=user@host`
set, freshly built test/bench/run binaries are SCP'd to the device, executed
there over SSH, with args and exit code forwarded; **unset → runs locally (no-op)**,
so ordinary same-machine builds are unaffected. Other knobs: `LOGBENCH_REMOTE_DIR`,
`LOGBENCH_SSH`, `LOGBENCH_SCP`, `LOGBENCH_KEEP_REMOTE`.

```bash
LOGBENCH_REMOTE=pi@host.local cargo test --release --target aarch64-unknown-linux-gnu
```

`scripts/overnight.sh` has its **own** remote orchestration (it does not use the
Cargo target runner above). Setting `LOGBENCH_REMOTE=user@host` makes the script
build the binary on the build host, SCP it to the device, run every trial there
over SSH, copy each trial's JSON/CSV back, and aggregate the report locally —
fully automated, device needs only an SSH server. `LOGBENCH_TARGET=<triple>`
cross-compiles for a different-architecture device (the script runs `--help` on
the staged binary first and aborts with a clear message on an arch mismatch).
The captured `run_meta.json` describes the *device* (CPU/kernel/memory) plus the
`build_host`. It shares the `LOGBENCH_REMOTE_DIR`/`LOGBENCH_SSH`/`LOGBENCH_SCP`/
`LOGBENCH_KEEP_REMOTE` knobs with the test runner. The binary is staged in, and
run from, a `bin/` subdir of the staging dir, which defaults to the device's
`$HOME` (resolved over SSH) rather than `/tmp` — `/tmp` is commonly `noexec`,
which would refuse the copied binary. `SKIP_BUILD=1` +
`LOGBENCH_BIN=<path>` reuse a prebuilt binary instead of building. See the
README's "Running the overnight comparison on another device".

## Testing notes

`tests/integration.rs` is end-to-end: it verifies lossless (`block`) strategies
durably write *every* record (byte-count assertions on the output file) and that
`drop` actually sheds load under pressure. Because of the global-logger
constraint, tests that install globals must be structured to avoid clashing
within one test process.
