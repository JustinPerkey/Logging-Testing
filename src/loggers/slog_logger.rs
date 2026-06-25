//! Structured logging with `slog` + `slog-async`.
//!
//! Unlike the `log`-facade and `tracing` backends, `slog` is **not** global: a
//! `slog::Logger` is an ordinary value, so we can build a fresh one per case
//! without fighting over a process-wide singleton.
//!
//! The hot path calls `slog::info!` on a shared `&slog::Logger` (no extra
//! locking of our own — `slog-async` already hands the record to its background
//! thread over a channel). `finish()` drops the `AsyncGuard`, which sends a
//! flush-and-terminate message and blocks until the worker has drained.

use std::fs::File;
use std::sync::Mutex;

use slog::Drain;

use super::{record_str, Logger};
use crate::config::{FullPolicy, LoggerConfig};

pub struct SlogLogger {
    logger: slog::Logger,
    /// Dropping this guard flushes and stops the `slog-async` worker thread.
    guard: Mutex<Option<slog_async::AsyncGuard>>,
}

impl SlogLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let file = File::create(&cfg.path)?;
        let decorator = slog_term::PlainDecorator::new(file);
        let drain = slog_term::FullFormat::new(decorator).build().fuse();

        // slog-async needs a positive channel size; treat "unbounded" (0) as a
        // generous fixed bound.
        let chan = if cfg.capacity == 0 {
            16_384
        } else {
            cfg.capacity
        };
        let overflow = match cfg.full_policy {
            FullPolicy::Block => slog_async::OverflowStrategy::Block,
            FullPolicy::Drop => slog_async::OverflowStrategy::Drop,
        };
        let (async_drain, guard) = slog_async::Async::new(drain)
            .chan_size(chan)
            .overflow_strategy(overflow)
            .thread_name("logbench-slog".into())
            .build_with_guard();

        let logger = slog::Logger::root(async_drain.fuse(), slog::o!());
        Ok(SlogLogger {
            logger,
            guard: Mutex::new(Some(guard)),
        })
    }
}

impl Logger for SlogLogger {
    fn log(&self, record: &[u8]) {
        slog::info!(self.logger, "{}", record_str(record));
    }

    fn finish(&self) -> u64 {
        // Dropping the guard flushes the async worker and joins it.
        drop(self.guard.lock().expect("slog guard mutex poisoned").take());
        // slog-async's Drop strategy does not surface a dropped count, so we
        // report 0; run with --full-policy block for a lossless comparison.
        0
    }
}
