//! Configuration types shared across the benchmark engine.

use std::path::PathBuf;
use std::str::FromStr;

/// A logging strategy under test.
///
/// There are two families here:
///
/// * **Transport baselines** ([`Strategy::Direct`], [`Strategy::Crossbeam`],
///   [`Strategy::Flume`], [`Strategy::TracingAppender`]) write *raw bytes* to a
///   file behind various hand-off mechanisms. They isolate the cost of the
///   transport itself (lock vs. channel vs. non-blocking queue) with no
///   formatting overhead.
/// * **Real logging crates** (everything else) drive an actual popular logging
///   crate through its real macro → format → sink path: `log`-facade backends
///   (`env_logger`, `fern`, `log4rs`, `flexi_logger`), the `tracing`
///   instrumentation stack (plain events, a non-blocking writer, and
///   span-wrapped events), structured `slog`, and the high-throughput `ftlog`.
///   These capture the *crate differences* — timestamping, level filtering,
///   field/encoding work and the writer machinery — that a raw transport hides.
///
/// What every strategy has in common is that the benchmark measures the same
/// thing: how long the producing thread is held inside a single `log()` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strategy {
    // --- transport baselines (raw bytes) ---
    /// Synchronous baseline: a `Mutex<BufWriter<File>>` written on the calling
    /// thread. Simple, but the `log()` call blocks on the lock and on any flush.
    Direct,
    /// A [`crossbeam_channel`] hands the record to a single background writer
    /// thread. The hot path only does an allocation + channel send.
    Crossbeam,
    /// Same shape as [`Strategy::Crossbeam`] but using the [`flume`] channel.
    Flume,
    /// The `tracing-appender` `NonBlocking` writer — the same machinery the
    /// `tracing` ecosystem uses for non-blocking file logging.
    TracingAppender,

    // --- real `log`-facade backends (install one global logger per process) ---
    /// The `log` facade with the `env_logger` backend writing to a file.
    LogEnvLogger,
    /// The `log` facade with the `fern` dispatch backend writing to a file.
    LogFern,
    /// The `log` facade with the `log4rs` file appender.
    LogLog4rs,
    /// The `flexi_logger` crate writing to a file in buffered mode
    /// (`BufferAndFlush`). Its `WriteMode::Async` uses an unbounded queue with no
    /// back-pressure, so a buffered writer on the producing thread is used here to
    /// bound memory like the other strategies (see `loggers::log_facade`).
    LogFlexi,

    // --- instrumentation / structured / high-throughput crates ---
    /// Structured `slog` with the `slog-async` drain to a file.
    SlogAsync,
    /// `tracing` with a `tracing-subscriber` fmt layer writing synchronously.
    TracingFmt,
    /// `tracing` fmt layer over a `tracing-appender` non-blocking writer — the
    /// full, idiomatic non-blocking `tracing` file-logging stack.
    TracingNonBlocking,
    /// Like [`Strategy::TracingFmt`] but each event is wrapped in an entered
    /// span, to show the cost of span-based instrumentation.
    TracingSpan,
    /// The high-throughput `ftlog` logger (dedicated log thread, `log` facade).
    Ftlog,
}

impl Strategy {
    /// The transport-baseline strategies, in a stable order. This is the default
    /// `--strategies all` set: none of them install process-global state, so
    /// they can all be swept inside a single process.
    pub const ALL: [Strategy; 4] = [
        Strategy::Direct,
        Strategy::Crossbeam,
        Strategy::Flume,
        Strategy::TracingAppender,
    ];

    /// The real-logging-crate strategies, in a stable order. Most of these
    /// install a process-global logger (see [`Strategy::is_global`]), so the
    /// overnight harness runs each one in its own process.
    pub const CRATES: [Strategy; 9] = [
        Strategy::LogEnvLogger,
        Strategy::LogFern,
        Strategy::LogLog4rs,
        Strategy::LogFlexi,
        Strategy::SlogAsync,
        Strategy::TracingFmt,
        Strategy::TracingNonBlocking,
        Strategy::TracingSpan,
        Strategy::Ftlog,
    ];

    /// Every strategy, transport baselines first then real crates.
    pub const EVERY: [Strategy; 13] = [
        Strategy::Direct,
        Strategy::Crossbeam,
        Strategy::Flume,
        Strategy::TracingAppender,
        Strategy::LogEnvLogger,
        Strategy::LogFern,
        Strategy::LogLog4rs,
        Strategy::LogFlexi,
        Strategy::SlogAsync,
        Strategy::TracingFmt,
        Strategy::TracingNonBlocking,
        Strategy::TracingSpan,
        Strategy::Ftlog,
    ];

    /// Stable, lowercase, machine-friendly name (also used in CSV output).
    pub fn name(self) -> &'static str {
        match self {
            Strategy::Direct => "direct",
            Strategy::Crossbeam => "crossbeam",
            Strategy::Flume => "flume",
            Strategy::TracingAppender => "tracing-appender",
            Strategy::LogEnvLogger => "env_logger",
            Strategy::LogFern => "fern",
            Strategy::LogLog4rs => "log4rs",
            Strategy::LogFlexi => "flexi_logger",
            Strategy::SlogAsync => "slog-async",
            Strategy::TracingFmt => "tracing-fmt",
            Strategy::TracingNonBlocking => "tracing-nb",
            Strategy::TracingSpan => "tracing-span",
            Strategy::Ftlog => "ftlog",
        }
    }

    /// Whether this strategy formats a real log record (level, timestamp,
    /// message, fields) rather than writing the raw payload bytes. True for
    /// every real-crate strategy, false for the transport baselines.
    pub fn is_real_crate(self) -> bool {
        !matches!(
            self,
            Strategy::Direct | Strategy::Crossbeam | Strategy::Flume | Strategy::TracingAppender
        )
    }

    /// Whether building this strategy installs a process-global singleton (the
    /// `log` facade's global logger, or `tracing`'s global default subscriber).
    ///
    /// Only one *global* strategy may be active per process, so the overnight
    /// harness runs each global strategy in its own process. `slog` is the one
    /// real crate that is *not* global: a `slog::Logger` is an ordinary value.
    pub fn is_global(self) -> bool {
        matches!(
            self,
            Strategy::LogEnvLogger
                | Strategy::LogFern
                | Strategy::LogLog4rs
                | Strategy::LogFlexi
                | Strategy::TracingFmt
                | Strategy::TracingNonBlocking
                | Strategy::TracingSpan
                | Strategy::Ftlog
        )
    }

    /// Whether this strategy hands work to a background thread (informational).
    pub fn is_async(self) -> bool {
        matches!(
            self,
            Strategy::Crossbeam
                | Strategy::Flume
                | Strategy::TracingAppender
                | Strategy::SlogAsync
                | Strategy::TracingNonBlocking
                | Strategy::Ftlog
        )
    }
}

// Serialize as the stable lowercase `name()` so JSON output matches the CSV
// and the `name()` used everywhere else (the report aggregator keys on it).
impl serde::Serialize for Strategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.name())
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" | "blocking" | "sync" => Ok(Strategy::Direct),
            "crossbeam" | "crossbeam-channel" => Ok(Strategy::Crossbeam),
            "flume" => Ok(Strategy::Flume),
            "tracing-appender" | "appender" => Ok(Strategy::TracingAppender),
            "env_logger" | "env-logger" | "envlogger" => Ok(Strategy::LogEnvLogger),
            "fern" => Ok(Strategy::LogFern),
            "log4rs" => Ok(Strategy::LogLog4rs),
            "flexi" | "flexi_logger" | "flexi-logger" => Ok(Strategy::LogFlexi),
            "slog" | "slog-async" => Ok(Strategy::SlogAsync),
            "tracing" | "tracing-fmt" => Ok(Strategy::TracingFmt),
            "tracing-nb" | "tracing-nonblocking" | "tracing-non-blocking" => {
                Ok(Strategy::TracingNonBlocking)
            }
            "tracing-span" | "tracing-spans" => Ok(Strategy::TracingSpan),
            "ftlog" => Ok(Strategy::Ftlog),
            other => Err(format!(
                "unknown strategy '{other}' (expected one of: direct, crossbeam, flume, \
                 tracing-appender, env_logger, fern, log4rs, flexi_logger, slog-async, \
                 tracing-fmt, tracing-nb, tracing-span, ftlog)"
            )),
        }
    }
}

/// What to do when a bounded buffer is full at the moment a record is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum FullPolicy {
    /// Apply back-pressure: the `log()` call blocks until space is available.
    /// No records are lost, but the hot path can stall — visible in tail latency.
    Block,
    /// Drop the record and keep going. The hot path stays non-blocking, at the
    /// cost of losing log lines under load. Dropped records are counted.
    Drop,
}

impl FromStr for FullPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "block" | "backpressure" | "blocking" => Ok(FullPolicy::Block),
            "drop" | "lossy" => Ok(FullPolicy::Drop),
            other => Err(format!(
                "unknown full-policy '{other}' (expected: block or drop)"
            )),
        }
    }
}

impl std::fmt::Display for FullPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FullPolicy::Block => "block",
            FullPolicy::Drop => "drop",
        })
    }
}

/// How a single logger instance should be constructed for one benchmark case.
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    /// File the records are written to.
    pub path: PathBuf,
    /// Primary buffer knob. For channel strategies this is the channel capacity
    /// (in records); `0` means an unbounded channel. For [`Strategy::Direct`]
    /// it is ignored in favour of `writer_buf_bytes`.
    pub capacity: usize,
    /// Size of the `BufWriter` that fronts the file, in bytes.
    pub writer_buf_bytes: usize,
    /// Behaviour when a bounded channel is full.
    pub full_policy: FullPolicy,
}

/// The work each benchmark case performs.
#[derive(Debug, Clone, Copy)]
pub struct Workload {
    /// Number of concurrent producer threads hammering the logger.
    pub producers: usize,
    /// Records each producer emits (after warmup).
    pub messages_per_producer: u64,
    /// Size of each log record payload, in bytes.
    pub msg_size: usize,
    /// Optional target rate **per producer**, in records/second. `None` (or a
    /// non-positive value) means "go as fast as possible".
    pub target_rate_per_producer: Option<f64>,
    /// Untimed records emitted before measurement begins, to warm caches and
    /// spin up the background writer.
    pub warmup: u64,
}

impl Workload {
    /// Total measured records across all producers.
    pub fn total_messages(&self) -> u64 {
        self.messages_per_producer * self.producers as u64
    }
}
