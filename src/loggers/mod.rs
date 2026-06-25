//! Logging strategies under test.
//!
//! Every strategy implements the tiny [`Logger`] trait. The benchmark runner
//! only ever calls [`Logger::log`] on the hot path (this is the thing being
//! measured) and [`Logger::finish`] once at the end of each case to drain and
//! flush.
//!
//! Two families live here:
//!
//! * **transport baselines** ([`direct`], [`channel`], [`tracing_nb`]) write the
//!   raw payload bytes through different hand-off mechanisms; and
//! * **real logging crates** ([`log_facade`], [`slog_logger`], [`tracing_full`],
//!   [`ftlog_logger`]) drive an actual crate through its real macro → format →
//!   sink path, so the measurement includes timestamping, level filtering and
//!   the crate's own writer machinery.
//!
//! Many real-crate backends install a *process-global* logger (the `log`
//! facade's global logger, or `tracing`'s global default subscriber). Only one
//! such global may be active per process; [`claim_global`] enforces that and the
//! overnight harness runs each global strategy in its own process.

mod channel;
mod direct;
mod ftlog_logger;
mod log_facade;
mod slog_logger;
mod tracing_full;
mod tracing_nb;

use std::sync::{Arc, Mutex};

use crate::config::{LoggerConfig, Strategy};

/// A pluggable logging backend.
///
/// Implementations must be cheap to share across producer threads (`Send +
/// Sync`) because the runner clones an `Arc<dyn Logger>` into each one.
pub trait Logger: Send + Sync {
    /// Append one log record. For asynchronous strategies this should hand the
    /// record off without blocking (subject to the configured full-policy).
    ///
    /// For real-crate strategies `record` is the message payload; the crate
    /// adds its own level, timestamp and formatting.
    fn log(&self, record: &[u8]);

    /// Flush and drain all buffered records, blocking until they are durably
    /// written. Returns the number of records that were dropped over the life
    /// of this logger (always `0` for lossless strategies).
    ///
    /// For non-global strategies this also tears the logger down (a fresh one
    /// is built for the next case). For process-global strategies it only
    /// flushes — the underlying global logger stays installed for later cases.
    fn finish(&self) -> u64;
}

/// Interpret a raw payload as the message string a real logging crate would be
/// asked to log: valid UTF-8 with any trailing newline stripped (the crate adds
/// its own line terminator). The benchmark's payloads are ASCII, so this is a
/// cheap validation with no allocation.
pub(crate) fn record_str(record: &[u8]) -> &str {
    let trimmed = match record.last() {
        Some(b'\n') => &record[..record.len() - 1],
        _ => record,
    };
    std::str::from_utf8(trimmed).unwrap_or("logbench record payload")
}

/// Process-wide record of which global strategy (if any) has installed itself.
static GLOBAL_OWNER: Mutex<Option<Strategy>> = Mutex::new(None);

/// Claim the single process-global logger slot for `strategy`.
///
/// Returns `Ok(())` if the slot is free or already held by the same strategy,
/// and an error if a *different* global strategy already installed itself —
/// because the `log` facade and `tracing`'s global default can each only be set
/// once per process. The overnight harness avoids this by running one global
/// strategy per process.
pub(crate) fn claim_global(strategy: Strategy) -> std::io::Result<()> {
    let mut owner = GLOBAL_OWNER.lock().expect("global-owner mutex poisoned");
    match *owner {
        None => {
            *owner = Some(strategy);
            Ok(())
        }
        Some(s) if s == strategy => Ok(()),
        Some(s) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "cannot install global logger '{}': '{}' is already installed in this process. \
                 Only one global logging crate can be active per process — run each in its own \
                 process (the overnight harness does this automatically).",
                strategy.name(),
                s.name()
            ),
        )),
    }
}

/// Construct a logger for one benchmark case.
pub fn build(strategy: Strategy, cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    Ok(match strategy {
        Strategy::Direct => Arc::new(direct::DirectLogger::new(cfg)?),
        Strategy::Crossbeam => Arc::new(channel::CrossbeamLogger::new(cfg)?),
        Strategy::Flume => Arc::new(channel::FlumeLogger::new(cfg)?),
        Strategy::TracingAppender => Arc::new(tracing_nb::TracingAppenderLogger::new(cfg)?),
        Strategy::LogEnvLogger => log_facade::build_env_logger(cfg)?,
        Strategy::LogFern => log_facade::build_fern(cfg)?,
        Strategy::LogLog4rs => log_facade::build_log4rs(cfg)?,
        Strategy::LogFlexi => log_facade::build_flexi(cfg)?,
        Strategy::SlogAsync => Arc::new(slog_logger::SlogLogger::new(cfg)?),
        Strategy::TracingFmt => tracing_full::build_fmt(cfg)?,
        Strategy::TracingNonBlocking => tracing_full::build_non_blocking(cfg)?,
        Strategy::TracingSpan => tracing_full::build_span(cfg)?,
        Strategy::Ftlog => ftlog_logger::build(cfg)?,
    })
}
