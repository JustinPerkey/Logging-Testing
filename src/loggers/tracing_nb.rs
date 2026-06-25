//! The `tracing-appender` non-blocking writer strategy.
//!
//! This is the off-the-shelf machinery the `tracing` ecosystem ships for
//! non-blocking file logging. It runs its own background worker thread and a
//! bounded queue; we drive its `NonBlocking` writer directly so we measure the
//! appender itself rather than `tracing`'s event-formatting overhead.

use std::fs::File;
use std::io::Write;
use std::sync::Mutex;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};

use super::Logger;
use crate::config::{FullPolicy, LoggerConfig};

/// Wraps `tracing_appender`'s `NonBlocking` writer.
///
/// `NonBlocking` is cheap to clone (it is an `Arc` around a channel sender), so
/// each `log()` call clones it to get the `&mut Write` it needs. Dropping the
/// [`WorkerGuard`] flushes and stops the background thread, which is how
/// `finish()` drains.
pub struct TracingAppenderLogger {
    writer: NonBlocking,
    guard: Mutex<Option<WorkerGuard>>,
}

impl TracingAppenderLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let file = File::create(&cfg.path)?;
        let mut builder = tracing_appender::non_blocking::NonBlockingBuilder::default()
            // Mirror the channel strategies' full-policy: lossy=true drops on a
            // full queue, lossy=false applies back-pressure.
            .lossy(matches!(cfg.full_policy, FullPolicy::Drop));
        if cfg.capacity > 0 {
            builder = builder.buffered_lines_limit(cfg.capacity);
        }
        let (writer, guard) = builder.finish(file);
        Ok(TracingAppenderLogger {
            writer,
            guard: Mutex::new(Some(guard)),
        })
    }
}

impl Logger for TracingAppenderLogger {
    fn log(&self, record: &[u8]) {
        // `NonBlocking: Write` requires `&mut self`; cloning is just an Arc
        // bump and is the intended way to share the writer across threads.
        let mut w = self.writer.clone();
        let _ = w.write_all(record);
    }

    fn finish(&self) -> u64 {
        // Dropping the guard flushes the queue and joins the worker thread.
        drop(self.guard.lock().expect("guard mutex poisoned").take());
        // tracing-appender does not expose its dropped-record count, so we
        // report 0 here; run with --full-policy block for a lossless comparison.
        0
    }
}
