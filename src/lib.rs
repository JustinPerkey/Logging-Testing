//! # logbench
//!
//! A small, self-contained benchmark engine for comparing **real Rust logging
//! crates** — `env_logger`, `fern`, `log4rs`, `flexi_logger`, `slog`, the
//! `tracing` instrumentation stack and `ftlog` — alongside raw **async
//! transport baselines** (direct, crossbeam, flume, tracing-appender).
//!
//! The central question this crate answers is: *given my hardware, my message
//! sizes, my buffer budget and my log rate, which logging crate keeps the hot
//! path fastest while still durably writing every record?* For the real crates
//! the measurement includes their genuine per-record cost (timestamp, level
//! filtering, formatting, sink hand-off) — the actual crate difference.
//!
//! To answer that it sweeps a matrix of:
//! * **strategies** ([`loggers`]) — the transport baselines plus the real
//!   logging crates listed above (see [`config::Strategy`]);
//! * **message sizes** — bytes per log record;
//! * **buffer amounts** — channel capacity / writer buffer size;
//! * **log frequency** — either max throughput, or a pinned target rate;
//! * **producer threads** — concurrency on the logging hot path.
//!
//! For each cell of the matrix it records the **producer-side latency**
//! distribution of the `log()` call (the thing that actually matters for a
//! non-blocking logger) plus throughput, drain time and dropped-record counts.
//!
//! See [`runner::run_case`] for the core measurement loop and [`report`] for
//! the CSV / JSON / console / recommendation output.

pub mod config;
pub mod loggers;
pub mod metrics;
pub mod report;
pub mod runner;

pub use config::{FullPolicy, LoggerConfig, Strategy, Workload};
pub use metrics::CaseResult;
pub use runner::run_case;
