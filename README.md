# logbench — find your optimal async logging strategy

`logbench` is a self-contained Rust suite for benchmarking **asynchronous,
non-blocking logging** strategies. Clone it, run it on *your* machine, and it
will tell you which logging approach keeps your hot path fastest while still
durably writing every record — for *your* message sizes, buffer budget, log
rate and thread count.

The thing it actually measures is the one that matters for non-blocking
logging: **how long the `log()` call blocks the producing thread** (the
hot-path latency distribution), alongside throughput, drain cost and dropped
records.

## Quick start

```bash
# Build optimized (always benchmark in release).
cargo build --release

# Run the default sweep and print a table + recommendation.
./target/release/logbench

# Or in one step:
cargo run --release
```

You'll get a per-case progress line, a results table, a plain-language
recommendation, and `bench-out/results.csv` + `bench-out/results.json` for
further analysis.

## Strategies compared

Every strategy ultimately writes records to a file behind a `BufWriter`. What
differs is how the producing thread hands a record off — which is exactly what
drives hot-path latency and the non-blocking property.

| Strategy           | How it works                                                                 | Non-blocking? |
| ------------------ | ---------------------------------------------------------------------------- | ------------- |
| `direct`           | `Mutex<BufWriter<File>>` written on the calling thread. The honest baseline. | No            |
| `crossbeam`        | `crossbeam-channel` → single background writer thread.                       | Yes           |
| `flume`            | `flume` channel → single background writer thread.                           | Yes           |
| `tracing-appender` | The `tracing-appender` `NonBlocking` writer (what `tracing` uses for files). | Yes           |

The three async strategies move the file I/O off the hot path: the producer
only allocates the record and pushes it onto a queue, while a background thread
owns the disk writes.

## What gets swept

`logbench` runs the full cartesian product of these axes (all configurable):

| Axis            | Flag           | Default        | Meaning                                                        |
| --------------- | -------------- | -------------- | -------------------------------------------------------------- |
| Strategy        | `--strategies` | `all`          | Comma list or `all`.                                           |
| **Log size**    | `--msg-sizes`  | `64,512,4096`  | Bytes per record.                                              |
| **Buffer**      | `--buffers`    | `8192`         | Channel capacity in records (`0` = unbounded). Ignored by `direct`. |
| Producers       | `--producers`  | `4`            | Concurrent threads on the logging hot path.                    |
| **Log rate**    | `--rates`      | `0`            | Target records/sec **per producer** (`0` = max throughput).    |
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

# Keep the produced log files for inspection.
cargo run --release -- --keep-logs --out-dir ./my-run
```

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
    tracing_nb.rs    tracing-appender NonBlocking writer
benches/logging.rs Criterion harness
tests/integration.rs end-to-end correctness checks
```

Adding your own strategy is a matter of implementing the small `Logger` trait
(`log` + `finish`) in `src/loggers/` and wiring it into `Strategy` and
`loggers::build`.

## License

MIT — see [LICENSE](LICENSE).
