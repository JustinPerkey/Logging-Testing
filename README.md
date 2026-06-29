# logbench — compare Rust logging crates (and async transports) on your machine

`logbench` is a self-contained Rust suite for benchmarking **popular logging
crates** head-to-head — `env_logger`, `fern`, `log4rs`, `flexi_logger`, `slog`,
the `tracing` instrumentation stack and the high-throughput `ftlog` — alongside
a set of raw **async transport baselines** (`direct`, `crossbeam`, `flume`,
`tracing-appender`). Clone it, run it on *your* machine, and it will tell you
which logging crate keeps your hot path fastest while still durably writing
every record — for *your* message sizes, buffer budget, log rate and thread
count.

The thing it actually measures is the one that matters most: **how long the
`log()` call holds the producing thread** (the hot-path latency distribution),
alongside throughput, drain cost and dropped records. For the real crates that
includes their genuine per-record cost — timestamp formatting, level filtering,
field encoding and the hand-off to their sink — which is exactly the *crate
difference* this suite exists to surface.

## Quick start

```bash
# Build optimized (always benchmark in release).
cargo build --release

# Compare the async transport baselines (the default):
./target/release/logbench

# Compare a couple of real crates directly (one global crate per process —
# see the note below):
./target/release/logbench --strategies tracing-fmt
./target/release/logbench --strategies slog-async,crossbeam

# Run the full, statistically-significant comparison of EVERY crate overnight:
scripts/overnight.sh            # writes overnight-out/REPORT.md
```

You'll get a per-case progress line, a results table, a plain-language
recommendation, and `bench-out/results.csv` + `bench-out/results.json` for
further analysis. The overnight harness additionally produces a full Markdown
**report** with means and 95% confidence intervals — see
[Overnight comparison & report](#overnight-comparison--report).

## What's compared

### Real logging crates (the headline comparison)

These drive an actual crate through its real macro → format → sink path, so the
measured latency includes everything the crate does per record.

| Strategy        | Crate                            | Kind                         | Global? |
| --------------- | -------------------------------- | ---------------------------- | ------- |
| `env_logger`    | `log` + `env_logger`             | Simple `log`-facade backend  | Yes     |
| `fern`          | `log` + `fern`                   | `log`-facade dispatch        | Yes     |
| `log4rs`        | `log` + `log4rs`                 | `log`-facade, appender-based | Yes     |
| `flexi_logger`  | `flexi_logger` (buffered mode)   | `log`-facade, buffered writer| Yes     |
| `slog-async`    | `slog` + `slog-async`            | Structured logging           | No      |
| `tracing-fmt`   | `tracing` + `tracing-subscriber` | Instrumentation (sync fmt)   | Yes     |
| `tracing-nb`    | `tracing` + `tracing-appender`   | Instrumentation (async)      | Yes     |
| `tracing-span`  | `tracing` (span-wrapped events)  | Instrumentation (+ spans)    | Yes     |
| `tracing-json`  | `tracing` + JSON + appender      | **Combined** stack (structured + JSON + async) | Yes |
| `ftlog`         | `ftlog`                          | High-throughput async        | Yes     |

### Transport baselines (raw bytes, no formatting)

These write the raw payload to a file behind different hand-off mechanisms, with
**no formatting cost**. They isolate the cost of the transport itself, and give
the real crates an honest reference point.

| Strategy           | How it works                                                                 | Non-blocking? |
| ------------------ | ---------------------------------------------------------------------------- | ------------- |
| `direct`           | `Mutex<BufWriter<File>>` written on the calling thread. The honest baseline. | No            |
| `crossbeam`        | `crossbeam-channel` → single background writer thread.                       | Yes           |
| `flume`            | `flume` channel → single background writer thread.                           | Yes           |
| `tracing-appender` | The `tracing-appender` `NonBlocking` writer (the queue, without formatting). | Yes           |

Shortcuts for `--strategies`: `all` (the four transport baselines, the default),
`crates` (all ten real crates), `every` (both).

> **One global logger per process.** The `log` facade and `tracing`'s global
> default subscriber can each be installed only **once per process**, so the
> crates marked *Global* above cannot be swept together in a single run — only
> the first one installs; the rest are skipped with a clear message. This is not
> a limitation of the benchmark but of the ecosystem. The
> [overnight harness](#overnight-comparison--report) sidesteps it by running
> **each strategy in its own process**, which is also better for statistical
> isolation. `slog` is the one real crate that is *not* global — a
> `slog::Logger` is an ordinary value.

## These "types" are layers, not separate options

The tables above split strategies into categories — transport baselines vs. real
crates, and within the crates a "Kind" column (simple `log`-facade backend,
structured, instrumentation, high-throughput async). It is tempting to read those
as **mutually exclusive choices**: pick *either* structured logging *or* an async
transport *or* a formatting backend. They are not. A real logging solution is a
**stack of layers**, and most of these strategies already combine several:

| Layer            | What it does                                      | Where you see it isolated here | Where it's combined |
| ---------------- | ------------------------------------------------- | ------------------------------ | ------------------- |
| **Facade**       | The call-site API (`log!`, `tracing::info!`, `slog::info!`) that decouples call sites from the backend | `env_logger`, `fern`, `log4rs` | every real crate |
| **Structured**   | Real key/value fields, not just a message string  | `slog`                         | `tracing-json`, `slog-async` |
| **Formatting**   | Turning a record into bytes (text / JSON / etc.)  | the `fmt` layer in `tracing-fmt` | `tracing-json` (JSON), every backend |
| **Transport**    | Getting those bytes off the hot path — a lock, a channel, a non-blocking queue, a dedicated thread | `direct`, `crossbeam`, `flume`, `tracing-appender` | `tracing-nb`, `slog-async`, `ftlog`, `tracing-json` |

Read that way, several existing strategies are *already* combinations: `tracing-nb`
is the instrumentation **facade** + text **formatting** + an async **transport**;
`slog-async` is **structured** fields + an async **transport**; `ftlog` is the
`log` **facade** + a dedicated-thread **transport**.

To make the point concrete, **`tracing-json`** deliberately stacks all four layers
at once — it is the idiomatic production `tracing` configuration: the `tracing`
**facade** emits real **structured** key/value fields, a `tracing-subscriber`
JSON **formatter** encodes each event, and a `tracing-appender` non-blocking
writer is the async **transport**. Benchmarking it next to the strategies that
isolate one layer apiece shows what each layer costs *and* what they cost stacked
together — which is what you actually ship.

```bash
# The combined stack vs. the layers it's built from:
./target/release/logbench --strategies tracing-json
./target/release/logbench --strategies slog-async        # structured + async
./target/release/logbench --strategies tracing-appender  # async transport alone
```

## What gets swept

`logbench` runs the full cartesian product of these axes (all configurable):

| Axis            | Flag           | Default        | Meaning                                                        |
| --------------- | -------------- | -------------- | -------------------------------------------------------------- |
| Strategy        | `--strategies` | `all`          | Comma list or `all`.                                           |
| **Log size**    | `--msg-sizes`  | `64,512,4096`  | Bytes per record.                                              |
| **Buffer**      | `--buffers`    | `8192`         | Channel capacity in records (`0` = unbounded). Ignored by `direct`. |
| Producers       | `--producers`  | `4`            | Concurrent threads on the logging hot path.                    |
| **Log rate**    | `--rates`      | `0`            | Target records/sec **per producer** (`0` = max throughput).    |
| **Lines/log**   | `--lines-per-log` | `30`        | Synthetic code-lines of work between `log()` calls; drives the **slowdown** column (`0` disables it). |
| Messages        | `--messages`   | `200000`       | Measured records per producer per case.                        |
| Warmup          | `--warmup`     | `5000`         | Untimed records per producer before measuring.                 |
| Writer buffer   | `--writer-buf` | `65536`        | Bytes for the background `BufWriter`.                          |
| Full policy     | `--full-policy`| `block`        | `block` (lossless back-pressure) or `drop` (lossy, stays non-blocking). |

Other flags: `--out-dir`, `--keep-logs`, `--csv <path>`, `--json <path>`.
Run `./target/release/logbench --help` for everything.

## Reading the results

The table reports, per case:

- **p50 / p99 / p99.9 / max** — the `log()` call latency distribution. This is
  the cost imposed on your application's hot path. Async strategies typically
  have a much lower *median* (they just do a queue push) but can have a higher
  *tail* under back-pressure; that trade-off is the whole point.
- **thrpt** — end-to-end records/sec, including the drain/flush phase.
- **MB/s** — payload throughput, including drain.
- **drop** — records lost (only possible under `--full-policy drop`).
- **lines / slowdn** — the interleaved-work model. With `--lines-per-log N`
  (default `30`) each producer runs a calibrated chunk of synthetic CPU work
  standing in for ~N lines of code between consecutive `log()` calls, and the
  case is timed both with and without the `log()` calls. **slowdn** is how much
  longer the program ran *because of logging* (`100 × logging_time /
  work_time`) — i.e. the actual device slowdown for logging every ~N lines.
  `logging_time` is the time actually spent inside `log()`, so the figure is
  independent of `--rates`: rate pacing makes the producer *sleep* between calls,
  and that idle time is not logging cost, so it is excluded.
  `--lines-per-log 0` disables it (the column shows `—`). At startup `logbench`
  prints the calibrated cost of one synthetic line so you can judge how your
  real code compares (e.g. tune `--lines-per-log` up if your inter-log code is
  heavier than simple arithmetic).

The **Recommendations** section then picks, for each comparable workload, the
lowest-tail-latency *lossless* strategy and the highest-throughput strategy on
your machine, plus a single headline pick.

> Note: on a fast local disk the `direct` baseline often wins raw throughput
> because there is no real I/O stall to hide. The async strategies prove their
> worth when (a) you pin a **target rate** and care about tail latency, or (b)
> your sink is slow/bursty (network, slow disk, fsync) so producers must not be
> blocked by I/O. Always re-run with your real sizes and rates.

## Example sweeps

```bash
# Sweep buffer sizes for the async strategies at a fixed message size.
cargo run --release -- \
  --strategies crossbeam,flume,tracing-appender \
  --msg-sizes 512 --buffers 256,1024,8192,65536,0 --producers 8

# Pin a sustained rate and compare tail latency under that load.
cargo run --release -- --rates 50000 --msg-sizes 256 --producers 4

# Lossy mode: how non-blocking can you get if you accept dropped lines?
cargo run --release -- --full-policy drop --buffers 1024 --producers 8

# Quantify the program slowdown of logging at different code densities
# (a log line every 10 / 30 / 100 lines of code).
cargo run --release -- --lines-per-log 10,30,100 --msg-sizes 256 --producers 1

# Keep the produced log files for inspection.
cargo run --release -- --keep-logs --out-dir ./my-run
```

## Overnight comparison & report

A single `logbench` run measures each case once. For a decision you can trust —
one that separates real crate differences from run-to-run noise — use the
overnight harness, which adds **statistical significance** by running many
independent trials and reporting confidence intervals.

```bash
scripts/overnight.sh                       # overnight-sized defaults (~hours)
SMOKE=1 scripts/overnight.sh               # ~1 minute, validates the pipeline
TRIALS=60 MESSAGES=1000000 scripts/overnight.sh
```

What it does:

- Runs **each strategy in its own process** (so every global crate is happy),
  repeated for many **trials** (default 40). The strategy order is reshuffled
  every trial so thermal drift or a transient background load can't
  systematically favour whichever ran first.
- Writes one JSON file per trial under `overnight-out/results/<strategy>/`, plus
  a captured `run_meta.json` (host, CPU, kernel, rustc, git commit, parameters).
- Aggregates everything with `scripts/aggregate.py`, which treats **each trial as
  one observation** and reports, per metric, the **mean ± 95% confidence
  interval** (Student's t). When two strategies' intervals overlap, the report
  says so explicitly — they are *not statistically distinguishable* on your
  machine.
- Produces three artifacts in `overnight-out/`:
  - **`REPORT.md`** — leaderboards per workload, best-in-each crate-family,
    significance notes and caveats, plus a **"What is being tested"** section that
    spells out exactly what each strategy exercises (crate, the Facade /
    Structured / Formatting / Transport layers it touches, global-or-not) and what
    each metric (test type) means, and a **"How payload size changes the ranking"**
    section that ranks strategies at the smallest vs. largest message size and
    calls out the crossovers in plain text.
  - **`plots.html`** — **interactive [Bokeh](https://bokeh.org) charts** of every
    metric (p50 / p99 / p99.9 latency, throughput, program slowdown) plotted
    against **payload size**, one line per strategy, on log axes. This is the view
    that answers *"is this crate faster on small messages but slower on large
    ones?"* — watch for lines that cross. Hover a point for its 95% CI; click a
    legend entry to mute that strategy. The file is **fully self-contained and
    renders offline** — BokehJS is inlined from the bundles vendored under
    `scripts/vendor/bokehjs/`, so the aggregator needs no extra Python packages and
    the report works on an air-gapped device.
  - **`summary_stats.csv`** — mean / CI / stdev / CV / min / median / max for
    every cell.

It is safe to `Ctrl-C`: it aggregates whatever trials finished. A `MAX_HOURS`
budget (default 10) stops launching new trials so it always lands a report by
morning. Everything is tunable via environment variables documented at the top
of the script (`TRIALS`, `MESSAGES`, `MSG_SIZES`, `PRODUCERS`, `STRATEGIES`, …).

See [`sample-report/REPORT.md`](sample-report/REPORT.md) for an illustrative
(short, noisy CI-VM) example of the output.

You can also re-aggregate an existing run directory at any time:

```bash
python3 scripts/aggregate.py overnight-out
```

### Running the overnight comparison on another device

You usually want the numbers for a *specific* device (a slower laptop, a server,
an SBC like a Raspberry Pi) but don't want to install a toolchain there or babysit
the run. Point **`LOGBENCH_REMOTE`** at the device over SSH and
`scripts/overnight.sh` does the entire round-trip for you — no manual steps:

1. **builds** the binary on this (build) host,
2. **copies** it to the device and `chmod +x`'s it,
3. runs **every trial on the device** over SSH,
4. **copies each trial's results back** to this host as they complete,
5. **aggregates the report here** — `REPORT.md`, `plots.html`, `summary_stats.csv`, `run_meta.json`.

```bash
# Same architecture as this host (e.g. another x86-64 box) — nothing else needed:
LOGBENCH_REMOTE=user@device.local scripts/overnight.sh

# Different architecture — cross-compile for the device (install the Rust target
# + a cross-linker on this host first, e.g. `rustup target add <triple>`):
LOGBENCH_REMOTE=pi@raspberrypi.local LOGBENCH_TARGET=aarch64-unknown-linux-gnu \
  scripts/overnight.sh

# Validate the whole remote pipeline in ~1 minute before committing to a night:
SMOKE=1 LOGBENCH_REMOTE=user@device.local scripts/overnight.sh
```

The **device needs nothing but an SSH server** and the ability to run the binary
— no Rust, no Python, no repo checkout. Before launching trials the script copies
the binary over and runs `--help` on it; if that fails (almost always an
architecture mismatch) it stops immediately with a message telling you to set
`LOGBENCH_TARGET`, so you never waste a night on a binary the device can't run.
The captured `run_meta.json` records the **device's** CPU / kernel / memory (that
is what was benchmarked) and notes this host as the `build_host`. Staged files
are removed from the device on exit (keep them with `LOGBENCH_KEEP_REMOTE=1`).

| Variable               | Default                 | Meaning                                                                                          |
| ---------------------- | ----------------------- | ------------------------------------------------------------------------------------------------ |
| `LOGBENCH_REMOTE`      | *(unset)*               | `user@host` of the device. **Unset → ordinary local run.**                                       |
| `LOGBENCH_TARGET`      | *(unset)*               | Rust target triple to cross-compile for (e.g. `aarch64-unknown-linux-gnu`). Unset → build for this host's architecture. |
| `LOGBENCH_REMOTE_DIR`  | `~/logbench-overnight` (device home) | Staging directory on the device. The executable is placed in, and runs from, its `bin/` subdirectory. Defaults under the device's `$HOME` rather than `/tmp` — `/tmp` is often mounted `noexec`, which would refuse to run the copied binary. |
| `LOGBENCH_SSH`         | `ssh`                   | SSH command, e.g. `ssh -p 2222 -i ~/key`.                                                         |
| `LOGBENCH_SCP`         | `scp`                   | SCP command, e.g. `scp -P 2222 -i ~/key`.                                                         |
| `LOGBENCH_KEEP_REMOTE` | `0`                     | Set to `1` to leave the staged binary/results on the device.                                      |

All the usual knobs (`TRIALS`, `MESSAGES`, `MSG_SIZES`, `PRODUCERS`,
`STRATEGIES`, `MAX_HOURS`, …) work unchanged in remote mode.

> **Without `LOGBENCH_REMOTE`** the harness simply benchmarks whatever machine
> runs the script. If you'd rather drive it *on* the device by hand, you can also
> skip the build and point it at a binary you copied over yourself —
> `SKIP_BUILD=1 LOGBENCH_BIN=./logbench scripts/overnight.sh` (the device then
> needs `python3` for aggregation, but `aggregate.py` is standard-library only).
> This is the same `LOGBENCH_*` family used by the test runner under
> [Running the tests on a different device](#running-the-tests-on-a-different-device-than-the-one-that-builds-them);
> note the overnight harness drives SSH itself rather than going through the
> Cargo target runner.

## Criterion micro-benchmarks (optional)

A complementary, statistically rigorous look at the single-call cost of each
strategy:

```bash
cargo bench          # writes an HTML report to target/criterion/
```

## Tests

```bash
cargo test --release
```

The integration tests verify that lossless (`block`) strategies durably write
*every* record and that the `drop` policy actually sheds load under pressure.

### Running the tests on a different device than the one that builds them

Because `logbench` measures behaviour on a *specific* machine, you may want to
build the tests/benchmarks on one host but run them on the actual device under
test (a slower laptop, an SBC like a Raspberry Pi, a server, etc.). This repo
ships a Cargo **target runner** (`scripts/remote-test-runner.sh`, wired up in
`.cargo/config.toml`) that makes this seamless: it copies each freshly built
binary to a target device over SSH, runs it *there*, and forwards the arguments
and exit code back to Cargo. All the test logic — temp dirs, file writes, the
byte-count assertions — executes on the device.

```bash
# Build on this machine, run the tests on the target device:
LOGBENCH_REMOTE=user@device.local \
  cargo test --release --target aarch64-unknown-linux-gnu

# You can also run the benchmark binary itself on the device:
LOGBENCH_REMOTE=user@device.local \
  cargo run --release --target aarch64-unknown-linux-gnu -- --producers 4
```

Configure it with these environment variables:

| Variable               | Default                | Meaning                                                       |
| ---------------------- | ---------------------- | ------------------------------------------------------------ |
| `LOGBENCH_REMOTE`      | *(unset)*              | `user@host` of the target device. **Unset → run locally** (the runner is a no-op, so normal same-machine builds are unaffected). |
| `LOGBENCH_REMOTE_DIR`  | `/tmp/logbench-tests`  | Directory on the target to stage binaries in.                |
| `LOGBENCH_SSH`         | `ssh`                  | SSH command, e.g. `ssh -p 2222 -i ~/key` for a custom port/key. |
| `LOGBENCH_SCP`         | `scp`                  | SCP command, e.g. `scp -P 2222 -i ~/key`.                    |
| `LOGBENCH_KEEP_REMOTE` | `0`                    | Set to `1` to keep the copied binary on the device after the run. |

The target device only needs to be reachable over SSH and able to run the
compiled binary — no Rust toolchain required on it. Cross-compiling for the
device's architecture is the usual reason to set `--target`; if the build and
target share an architecture you can omit it.

## How it's built

```
src/
  config.rs        Strategy / FullPolicy / Workload / LoggerConfig
  metrics.rs       CaseResult + latency histogram aggregation (HdrHistogram)
  runner.rs        the measurement loop (barrier-synced producers, rate pacing)
  report.rs        CSV / JSON / console table / recommendations
  loggers/
    direct.rs        Mutex<BufWriter> baseline
    channel.rs       crossbeam + flume background-writer strategies
    tracing_nb.rs    tracing-appender NonBlocking writer (no formatting)
    log_facade.rs    env_logger / fern / log4rs / flexi_logger (the `log` facade)
    slog_logger.rs   slog + slog-async (structured, non-global)
    tracing_full.rs  tracing fmt / non-blocking / span / combined-json variants
    ftlog_logger.rs  ftlog high-throughput async
scripts/
  overnight.sh     statistically-significant overnight harness (one process/strategy)
  aggregate.py     trial aggregation → REPORT.md + plots.html + summary_stats.csv (stdlib only)
benches/logging.rs Criterion harness
tests/integration.rs end-to-end correctness checks
```

Adding your own strategy is a matter of implementing the small `Logger` trait
(`log` + `finish`) in `src/loggers/` and wiring it into `Strategy` and
`loggers::build`. Real-crate backends use `record_str()` to turn the payload
into the message string they log; global crates install behind `claim_global()`
so only one is ever active per process.

## License

MIT — see [LICENSE](LICENSE).
