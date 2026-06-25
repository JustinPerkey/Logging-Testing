# Rust logging crates — benchmark report

_Generated 2026-06-25 19:37 UTC by `scripts/aggregate.py`._

## Run environment

| field | value |
| --- | --- |
| `host` | vm |
| `kernel` | Linux 6.18.5 |
| `cpu_model` | Intel(R) Xeon(R) Processor @ 2.80GHz |
| `cpu_count` | 4 |
| `memory` | 15.7 GiB |
| `rustc` | rustc 1.94.1 (e408947bf 2026-03-25) |
| `git_commit` | 4189b2a |
| `started` | 2026-06-25T19:35:38Z |
| `finished` | 2026-06-25T19:37:19Z |
| `trials_requested` | 12 |
| `messages` | 20000 |
| `warmup` | 2000 |
| `writer_buf` | 65536 |
| `full_policy` | block |
| `msg_sizes` | 64,1024 |
| `producers` | 1,4 |
| `rates` | 0 |
| `buffers` | 8192 |

Result files parsed: **156**.

## Methodology

Every strategy was run **in its own process**, repeated for many independent **trials**. Each trial spins up the logger fresh, warms it up, then times a single `log()` call on each of several concurrent producer threads while they emit a fixed number of records. The headline metric is the **producer-side hot-path latency** of that `log()` call — the time your application thread is held inside the logging call — captured as a full HdrHistogram per trial. We also record end-to-end throughput (including the final drain/flush).

For statistical significance we treat **each trial as one observation** of each metric and report the **mean across trials ± the 95% confidence interval** (Student's t). When two strategies' confidence intervals overlap, the difference between them is within run-to-run noise and we say they are *not statistically distinguishable* on this machine. The transport baselines (`direct`, `crossbeam`, `flume`, `tracing-appender`) write raw bytes and pay **no formatting cost**; the real-crate strategies pay their genuine timestamp/level/format/sink cost, which is the whole point of the comparison.

## Executive summary

Headline numbers are for the representative workload: **64 B payload · 4 producer(s) · buffer 8192 · rate max · Block** (other workloads are tabulated below).

- **Lowest p99 hot-path latency (lossless):** `tracing-nb` — 2.67 µs ±579 ns over 12 trials.
- **Highest end-to-end throughput:** `crossbeam` — 4.40 M/s ±828.0 k/s.

**Best in each family (by mean p99 at the representative workload):**

| family | best member | p99 latency | throughput |
| --- | --- | --- | --- |
| Transport baselines (raw bytes) | `crossbeam` | 3.94 µs ±1.35 µs | 4.40 M/s ±828.0 k/s |
| `log` facade backends | `flexi_logger` | 4.45 µs ±873 ns | 3.29 M/s ±555.0 k/s |
| Structured logging | `slog-async` | 239.84 µs ±11.70 µs | 148.6 k/s ±8.3 k/s |
| Instrumentation (tracing) | `tracing-nb` | 2.67 µs ±579 ns | 1.97 M/s ±212.1 k/s |
| High-throughput async | `ftlog` | 218.92 µs ±8.75 µs | 190.1 k/s ±11.7 k/s |

## Leaderboard — 64 B payload · 4 producer(s) · buffer 8192 · rate max · Block

Sorted by mean p99 hot-path latency (lower is better). `±` is the 95% CI half-width across trials; `CV` is the coefficient of variation (run-to-run noise).

| # | strategy | family | trials | p50 | p99 | p99.9 | throughput | p99 CV% |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `tracing-nb` | Instrumentation (tracing) | 12 | 1.10 µs ±133 ns | 2.67 µs ±579 ns | 20.23 µs ±1.06 µs | 1.97 M/s ±212.1 k/s | 34.1 |
| 2 | `crossbeam` | Transport baselines (raw bytes) | 12 | 360 ns ±85 ns | 3.94 µs ±1.35 µs | 25.14 µs ±26.00 µs | 4.40 M/s ±828.0 k/s | 54.0 |
| 3 | `tracing-span` | Instrumentation (tracing) | 12 | 1.29 µs ±97 ns | 4.42 µs ±668 ns | 89.05 µs ±2.62 µs | 1.98 M/s ±142.8 k/s | 23.8 |
| 4 | `flexi_logger` | `log` facade backends | 12 | 568 ns ±140 ns | 4.45 µs ±873 ns | 43.32 µs ±28.70 µs | 3.29 M/s ±555.0 k/s | 30.9 |
| 5 | `tracing-fmt` | Instrumentation (tracing) | 12 | 884 ns ±147 ns | 6.00 µs ±3.71 µs | 84.20 µs ±4.53 µs | 2.72 M/s ±329.9 k/s | 97.3 |
| 6 | `direct` | Transport baselines (raw bytes) | 12 | 417 ns ±64 ns | 18.09 µs ±2.85 µs | 69.89 µs ±4.33 µs | 3.47 M/s ±377.9 k/s | 24.7 |
| 7 | `flume` | Transport baselines (raw bytes) | 12 | 564 ns ±108 ns | 71.89 µs ±5.28 µs | 276.31 µs ±29.03 µs | 1.08 M/s ±108.8 k/s | 11.6 |
| 8 | `tracing-appender` | Transport baselines (raw bytes) | 12 | 577 ns ±130 ns | 95.79 µs ±6.38 µs | 465.81 µs ±94.46 µs | 687.6 k/s ±59.4 k/s | 10.5 |
| 9 | `fern` | `log` facade backends | 12 | 566 ns ±11 ns | 113.89 µs ±3.70 µs | 204.98 µs ±8.39 µs | 537.1 k/s ±22.4 k/s | 5.1 |
| 10 | `env_logger` | `log` facade backends | 12 | 820 ns ±19 ns | 114.47 µs ±4.58 µs | 196.98 µs ±8.33 µs | 432.1 k/s ±13.4 k/s | 6.3 |
| 11 | `log4rs` | `log` facade backends | 12 | 1.27 µs ±38 ns | 137.68 µs ±4.43 µs | 265.61 µs ±13.66 µs | 361.9 k/s ±18.5 k/s | 5.1 |
| 12 | `ftlog` | High-throughput async | 12 | 1.04 µs ±102 ns | 218.92 µs ±8.75 µs | 425.83 µs ±29.86 µs | 190.1 k/s ±11.7 k/s | 6.3 |
| 13 | `slog-async` | Structured logging | 12 | 2.91 µs ±124 ns | 239.84 µs ±11.70 µs | 528.55 µs ±39.68 µs | 148.6 k/s ±8.3 k/s | 7.7 |

**Statistical significance notes:**

- `tracing-nb` and `crossbeam` have overlapping 95% CIs on p99 (2.67 µs ±579 ns vs 3.94 µs ±1.35 µs) — **not statistically distinguishable** at this workload.
- `crossbeam` and `tracing-span` have overlapping 95% CIs on p99 (3.94 µs ±1.35 µs vs 4.42 µs ±668 ns) — **not statistically distinguishable** at this workload.
- `tracing-span` and `flexi_logger` have overlapping 95% CIs on p99 (4.42 µs ±668 ns vs 4.45 µs ±873 ns) — **not statistically distinguishable** at this workload.
- `flexi_logger` and `tracing-fmt` have overlapping 95% CIs on p99 (4.45 µs ±873 ns vs 6.00 µs ±3.71 µs) — **not statistically distinguishable** at this workload.
- `fern` and `env_logger` have overlapping 95% CIs on p99 (113.89 µs ±3.70 µs vs 114.47 µs ±4.58 µs) — **not statistically distinguishable** at this workload.

## All workloads

### 64 B payload · 1 producer(s) · buffer 8192 · rate max · Block

| # | strategy | family | trials | p50 | p99 | p99.9 | throughput | p99 CV% |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `direct` | Transport baselines (raw bytes) | 12 | 41 ns ±1 ns | 48 ns ±6 ns | 7.61 µs ±3.24 µs | 10.45 M/s ±496.4 k/s | 20.0 |
| 2 | `tracing-fmt` | Instrumentation (tracing) | 12 | 612 ns ±2 ns | 1.07 µs ±96 ns | 22.09 µs ±973 ns | 1.38 M/s ±13.8 k/s | 14.1 |
| 3 | `crossbeam` | Transport baselines (raw bytes) | 12 | 129 ns ±39 ns | 1.66 µs ±514 ns | 6.53 µs ±2.61 µs | 3.36 M/s ±521.6 k/s | 48.7 |
| 4 | `tracing-nb` | Instrumentation (tracing) | 12 | 856 ns ±98 ns | 1.87 µs ±418 ns | 19.76 µs ±1.10 µs | 977.3 k/s ±91.3 k/s | 35.1 |
| 5 | `tracing-span` | Instrumentation (tracing) | 12 | 960 ns ±4 ns | 1.88 µs ±488 ns | 23.85 µs ±982 ns | 903.6 k/s ±14.1 k/s | 40.8 |
| 6 | `tracing-appender` | Transport baselines (raw bytes) | 12 | 288 ns ±64 ns | 2.17 µs ±243 ns | 19.28 µs ±2.95 µs | 1.63 M/s ±136.1 k/s | 17.6 |
| 7 | `fern` | `log` facade backends | 12 | 545 ns ±15 ns | 2.57 µs ±266 ns | 6.17 µs ±540 ns | 1.47 M/s ±33.0 k/s | 16.3 |
| 8 | `flexi_logger` | `log` facade backends | 12 | 256 ns ±36 ns | 2.67 µs ±314 ns | 18.51 µs ±921 ns | 2.22 M/s ±179.8 k/s | 18.5 |
| 9 | `env_logger` | `log` facade backends | 12 | 771 ns ±15 ns | 2.73 µs ±106 ns | 18.88 µs ±625 ns | 1.09 M/s ±17.5 k/s | 6.1 |
| 10 | `log4rs` | `log` facade backends | 12 | 1.08 µs ±9 ns | 3.48 µs ±473 ns | 21.09 µs ±588 ns | 803.4 k/s ±14.8 k/s | 21.4 |
| 11 | `flume` | Transport baselines (raw bytes) | 12 | 122 ns ±33 ns | 15.15 µs ±689 ns | 22.75 µs ±2.13 µs | 1.69 M/s ±151.1 k/s | 7.2 |
| 12 | `slog-async` | Structured logging | 12 | 1.48 µs ±618 ns | 39.34 µs ±5.34 µs | 62.45 µs ±2.96 µs | 234.9 k/s ±18.8 k/s | 21.4 |
| 13 | `ftlog` | High-throughput async | 12 | 541 ns ±85 ns | 58.94 µs ±7.42 µs | 83.61 µs ±7.70 µs | 214.9 k/s ±26.7 k/s | 19.8 |

### 64 B payload · 4 producer(s) · buffer 8192 · rate max · Block

| # | strategy | family | trials | p50 | p99 | p99.9 | throughput | p99 CV% |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `tracing-nb` | Instrumentation (tracing) | 12 | 1.10 µs ±133 ns | 2.67 µs ±579 ns | 20.23 µs ±1.06 µs | 1.97 M/s ±212.1 k/s | 34.1 |
| 2 | `crossbeam` | Transport baselines (raw bytes) | 12 | 360 ns ±85 ns | 3.94 µs ±1.35 µs | 25.14 µs ±26.00 µs | 4.40 M/s ±828.0 k/s | 54.0 |
| 3 | `tracing-span` | Instrumentation (tracing) | 12 | 1.29 µs ±97 ns | 4.42 µs ±668 ns | 89.05 µs ±2.62 µs | 1.98 M/s ±142.8 k/s | 23.8 |
| 4 | `flexi_logger` | `log` facade backends | 12 | 568 ns ±140 ns | 4.45 µs ±873 ns | 43.32 µs ±28.70 µs | 3.29 M/s ±555.0 k/s | 30.9 |
| 5 | `tracing-fmt` | Instrumentation (tracing) | 12 | 884 ns ±147 ns | 6.00 µs ±3.71 µs | 84.20 µs ±4.53 µs | 2.72 M/s ±329.9 k/s | 97.3 |
| 6 | `direct` | Transport baselines (raw bytes) | 12 | 417 ns ±64 ns | 18.09 µs ±2.85 µs | 69.89 µs ±4.33 µs | 3.47 M/s ±377.9 k/s | 24.7 |
| 7 | `flume` | Transport baselines (raw bytes) | 12 | 564 ns ±108 ns | 71.89 µs ±5.28 µs | 276.31 µs ±29.03 µs | 1.08 M/s ±108.8 k/s | 11.6 |
| 8 | `tracing-appender` | Transport baselines (raw bytes) | 12 | 577 ns ±130 ns | 95.79 µs ±6.38 µs | 465.81 µs ±94.46 µs | 687.6 k/s ±59.4 k/s | 10.5 |
| 9 | `fern` | `log` facade backends | 12 | 566 ns ±11 ns | 113.89 µs ±3.70 µs | 204.98 µs ±8.39 µs | 537.1 k/s ±22.4 k/s | 5.1 |
| 10 | `env_logger` | `log` facade backends | 12 | 820 ns ±19 ns | 114.47 µs ±4.58 µs | 196.98 µs ±8.33 µs | 432.1 k/s ±13.4 k/s | 6.3 |
| 11 | `log4rs` | `log` facade backends | 12 | 1.27 µs ±38 ns | 137.68 µs ±4.43 µs | 265.61 µs ±13.66 µs | 361.9 k/s ±18.5 k/s | 5.1 |
| 12 | `ftlog` | High-throughput async | 12 | 1.04 µs ±102 ns | 218.92 µs ±8.75 µs | 425.83 µs ±29.86 µs | 190.1 k/s ±11.7 k/s | 6.3 |
| 13 | `slog-async` | Structured logging | 12 | 2.91 µs ±124 ns | 239.84 µs ±11.70 µs | 528.55 µs ±39.68 µs | 148.6 k/s ±8.3 k/s | 7.7 |

### 1024 B payload · 1 producer(s) · buffer 8192 · rate max · Block

| # | strategy | family | trials | p50 | p99 | p99.9 | throughput | p99 CV% |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `fern` | `log` facade backends | 12 | 644 ns ±7 ns | 4.27 µs ±445 ns | 26.10 µs ±995 ns | 792.6 k/s ±19.7 k/s | 16.4 |
| 2 | `log4rs` | `log` facade backends | 12 | 1.61 µs ±6 ns | 5.62 µs ±323 ns | 28.22 µs ±1.73 µs | 438.8 k/s ±5.4 k/s | 9.0 |
| 3 | `env_logger` | `log` facade backends | 12 | 2.45 µs ±274 ns | 7.62 µs ±991 ns | 32.04 µs ±2.04 µs | 317.1 k/s ±14.3 k/s | 20.5 |
| 4 | `direct` | Transport baselines (raw bytes) | 12 | 50 ns ±0 ns | 12.29 µs ±256 ns | 18.03 µs ±1.42 µs | 3.26 M/s ±104.5 k/s | 3.3 |
| 5 | `flexi_logger` | `log` facade backends | 12 | 1.48 µs ±196 ns | 19.38 µs ±1.24 µs | 42.24 µs ±7.44 µs | 384.8 k/s ±35.0 k/s | 10.1 |
| 6 | `tracing-nb` | Instrumentation (tracing) | 12 | 4.74 µs ±89 ns | 21.30 µs ±1.35 µs | 44.22 µs ±6.78 µs | 176.7 k/s ±8.9 k/s | 10.0 |
| 7 | `tracing-fmt` | Instrumentation (tracing) | 12 | 4.34 µs ±5 ns | 25.20 µs ±796 ns | 42.60 µs ±4.00 µs | 198.5 k/s ±2.8 k/s | 5.0 |
| 8 | `tracing-span` | Instrumentation (tracing) | 12 | 4.69 µs ±4 ns | 26.33 µs ±577 ns | 47.23 µs ±4.47 µs | 183.4 k/s ±1.8 k/s | 3.5 |
| 9 | `flume` | Transport baselines (raw bytes) | 12 | 234 ns ±11 ns | 26.38 µs ±1.42 µs | 57.54 µs ±3.91 µs | 456.9 k/s ±50.3 k/s | 8.5 |
| 10 | `crossbeam` | Transport baselines (raw bytes) | 12 | 212 ns ±14 ns | 27.87 µs ±3.73 µs | 57.74 µs ±3.52 µs | 464.4 k/s ±56.2 k/s | 21.0 |
| 11 | `tracing-appender` | Transport baselines (raw bytes) | 12 | 503 ns ±101 ns | 33.40 µs ±7.37 µs | 64.29 µs ±4.56 µs | 331.4 k/s ±19.5 k/s | 34.7 |
| 12 | `slog-async` | Structured logging | 12 | 593 ns ±47 ns | 55.98 µs ±4.48 µs | 81.42 µs ±5.23 µs | 163.2 k/s ±9.8 k/s | 12.6 |
| 13 | `ftlog` | High-throughput async | 12 | 583 ns ±33 ns | 65.53 µs ±2.93 µs | 96.22 µs ±5.01 µs | 156.6 k/s ±7.6 k/s | 7.0 |

### 1024 B payload · 4 producer(s) · buffer 8192 · rate max · Block

| # | strategy | family | trials | p50 | p99 | p99.9 | throughput | p99 CV% |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `tracing-nb` | Instrumentation (tracing) | 12 | 4.84 µs ±101 ns | 27.26 µs ±3.30 µs | 238.54 µs ±64.89 µs | 464.4 k/s ±17.6 k/s | 19.1 |
| 2 | `direct` | Transport baselines (raw bytes) | 12 | 55 ns ±1 ns | 54.85 µs ±17.48 µs | 192.60 µs ±49.22 µs | 1.44 M/s ±240.0 k/s | 50.2 |
| 3 | `tracing-span` | Instrumentation (tracing) | 12 | 5.03 µs ±88 ns | 106.20 µs ±3.03 µs | 171.55 µs ±53.58 µs | 370.6 k/s ±11.5 k/s | 4.5 |
| 4 | `tracing-fmt` | Instrumentation (tracing) | 12 | 4.65 µs ±84 ns | 106.49 µs ±1.88 µs | 159.33 µs ±30.32 µs | 389.8 k/s ±6.8 k/s | 2.8 |
| 5 | `flexi_logger` | `log` facade backends | 12 | 2.22 µs ±54 ns | 123.73 µs ±12.48 µs | 311.43 µs ±47.07 µs | 421.1 k/s ±55.8 k/s | 15.9 |
| 6 | `flume` | Transport baselines (raw bytes) | 12 | 453 ns ±61 ns | 132.62 µs ±6.27 µs | 302.89 µs ±24.65 µs | 418.0 k/s ±26.7 k/s | 7.4 |
| 7 | `fern` | `log` facade backends | 12 | 1.11 µs ±28 ns | 147.81 µs ±7.09 µs | 286.43 µs ±12.30 µs | 318.7 k/s ±11.8 k/s | 7.5 |
| 8 | `env_logger` | `log` facade backends | 12 | 4.13 µs ±132 ns | 156.46 µs ±4.70 µs | 316.96 µs ±20.55 µs | 234.5 k/s ±13.2 k/s | 4.7 |
| 9 | `crossbeam` | Transport baselines (raw bytes) | 12 | 335 ns ±46 ns | 182.83 µs ±13.31 µs | 523.69 µs ±47.82 µs | 405.1 k/s ±41.8 k/s | 11.5 |
| 10 | `tracing-appender` | Transport baselines (raw bytes) | 12 | 643 ns ±121 ns | 189.42 µs ±15.03 µs | 382.08 µs ±24.08 µs | 309.5 k/s ±32.4 k/s | 12.5 |
| 11 | `log4rs` | `log` facade backends | 12 | 3.55 µs ±110 ns | 197.31 µs ±3.88 µs | 364.05 µs ±13.56 µs | 186.2 k/s ±8.2 k/s | 3.1 |
| 12 | `slog-async` | Structured logging | 12 | 4.21 µs ±358 ns | 258.64 µs ±11.76 µs | 640.89 µs ±54.60 µs | 114.8 k/s ±5.7 k/s | 7.2 |
| 13 | `ftlog` | High-throughput async | 12 | 2.62 µs ±408 ns | 302.04 µs ±15.58 µs | 535.23 µs ±28.22 µs | 91.7 k/s ±7.4 k/s | 8.1 |

## Crate differences, by family

### Transport baselines (raw bytes)

| strategy | p50 | p99 | throughput |
| --- | --- | --- | --- |
| `crossbeam` | 360 ns ±85 ns | 3.94 µs ±1.35 µs | 4.40 M/s ±828.0 k/s |
| `direct` | 417 ns ±64 ns | 18.09 µs ±2.85 µs | 3.47 M/s ±377.9 k/s |
| `flume` | 564 ns ±108 ns | 71.89 µs ±5.28 µs | 1.08 M/s ±108.8 k/s |
| `tracing-appender` | 577 ns ±130 ns | 95.79 µs ±6.38 µs | 687.6 k/s ±59.4 k/s |

### `log` facade backends

| strategy | p50 | p99 | throughput |
| --- | --- | --- | --- |
| `flexi_logger` | 568 ns ±140 ns | 4.45 µs ±873 ns | 3.29 M/s ±555.0 k/s |
| `fern` | 566 ns ±11 ns | 113.89 µs ±3.70 µs | 537.1 k/s ±22.4 k/s |
| `env_logger` | 820 ns ±19 ns | 114.47 µs ±4.58 µs | 432.1 k/s ±13.4 k/s |
| `log4rs` | 1.27 µs ±38 ns | 137.68 µs ±4.43 µs | 361.9 k/s ±18.5 k/s |

### Structured logging

| strategy | p50 | p99 | throughput |
| --- | --- | --- | --- |
| `slog-async` | 2.91 µs ±124 ns | 239.84 µs ±11.70 µs | 148.6 k/s ±8.3 k/s |

### Instrumentation (tracing)

| strategy | p50 | p99 | throughput |
| --- | --- | --- | --- |
| `tracing-nb` | 1.10 µs ±133 ns | 2.67 µs ±579 ns | 1.97 M/s ±212.1 k/s |
| `tracing-span` | 1.29 µs ±97 ns | 4.42 µs ±668 ns | 1.98 M/s ±142.8 k/s |
| `tracing-fmt` | 884 ns ±147 ns | 6.00 µs ±3.71 µs | 2.72 M/s ±329.9 k/s |

### High-throughput async

| strategy | p50 | p99 | throughput |
| --- | --- | --- | --- |
| `ftlog` | 1.04 µs ±102 ns | 218.92 µs ±8.75 µs | 190.1 k/s ±11.7 k/s |

## Caveats & methodology notes

- **Machine-specific.** These numbers reflect *this* host's CPU, allocator and disk. On a faster/slower disk or with a slow/bursty sink the ranking can change — re-run on your target hardware.
- **One global logger per process.** The `log` facade and `tracing`'s global default subscriber can each only be installed once per process, so every global crate (`env_logger`, `fern`, `log4rs`, `flexi_logger`, all `tracing-*`, `ftlog`) is benchmarked in its own process. `slog` is the exception — a `slog::Logger` is a plain value.
- **`tracing-nb` drain.** `tracing-appender`'s non-blocking worker has no public mid-life flush, so its per-case *drain* time is not separately captured; its hot-path latency (the headline metric) is measured exactly like everyone else's.
- **Dropped counts.** `slog`, `tracing-*` and the synchronous `log` backends don't surface a programmatic dropped-record count, so `drop` reads 0 for them; run lossless (`--full-policy block`) for an apples-to-apples comparison (the default).
- **Formatting is included on purpose.** Real-crate latencies include timestamp formatting and level filtering; the raw transport baselines do not. That gap *is* the cost of structured, human-readable logs.

