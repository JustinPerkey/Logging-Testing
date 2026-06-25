//! Configuration types shared across the benchmark engine.

use std::path::PathBuf;
use std::str::FromStr;

/// A logging strategy under test.
///
/// Every strategy ultimately writes log records to a file. What differs is *how*
/// the producing thread hands a record off to the bytes-hit-disk machinery —
/// which is exactly what determines hot-path latency and the "non-blocking"
/// property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Strategy {
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
}

impl Strategy {
    /// All strategies, in a stable order.
    pub const ALL: [Strategy; 4] = [
        Strategy::Direct,
        Strategy::Crossbeam,
        Strategy::Flume,
        Strategy::TracingAppender,
    ];

    /// Stable, lowercase, machine-friendly name (also used in CSV output).
    pub fn name(self) -> &'static str {
        match self {
            Strategy::Direct => "direct",
            Strategy::Crossbeam => "crossbeam",
            Strategy::Flume => "flume",
            Strategy::TracingAppender => "tracing-appender",
        }
    }

    /// Whether this strategy is asynchronous (hands work to a background thread).
    pub fn is_async(self) -> bool {
        !matches!(self, Strategy::Direct)
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" | "blocking" | "sync" => Ok(Strategy::Direct),
            "crossbeam" | "crossbeam-channel" => Ok(Strategy::Crossbeam),
            "flume" => Ok(Strategy::Flume),
            "tracing" | "tracing-appender" | "appender" => Ok(Strategy::TracingAppender),
            other => Err(format!(
                "unknown strategy '{other}' (expected one of: direct, crossbeam, flume, tracing-appender)"
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
            other => Err(format!("unknown full-policy '{other}' (expected: block or drop)")),
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
