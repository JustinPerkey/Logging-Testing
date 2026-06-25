//! Logging strategies under test.
//!
//! Every strategy implements the tiny [`Logger`] trait. The benchmark runner
//! only ever calls [`Logger::log`] on the hot path (this is the thing being
//! measured) and [`Logger::finish`] once at the end to drain and flush.

mod channel;
mod direct;
mod tracing_nb;

use std::sync::Arc;

use crate::config::{LoggerConfig, Strategy};

/// A pluggable logging backend.
///
/// Implementations must be cheap to share across producer threads (`Send +
/// Sync`) because the runner clones an `Arc<dyn Logger>` into each one.
pub trait Logger: Send + Sync {
    /// Append one log record. For asynchronous strategies this should hand the
    /// record off without blocking (subject to the configured full-policy).
    fn log(&self, record: &[u8]);

    /// Flush and drain all buffered records, blocking until they are durably
    /// written. Returns the number of records that were dropped over the life
    /// of this logger (always `0` for lossless strategies).
    fn finish(&self) -> u64;
}

/// Construct a logger for one benchmark case.
pub fn build(strategy: Strategy, cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    Ok(match strategy {
        Strategy::Direct => Arc::new(direct::DirectLogger::new(cfg)?),
        Strategy::Crossbeam => Arc::new(channel::CrossbeamLogger::new(cfg)?),
        Strategy::Flume => Arc::new(channel::FlumeLogger::new(cfg)?),
        Strategy::TracingAppender => Arc::new(tracing_nb::TracingAppenderLogger::new(cfg)?),
    })
}
