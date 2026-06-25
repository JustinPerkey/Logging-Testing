//! Result types and latency aggregation.

use hdrhistogram::Histogram;

use crate::config::{FullPolicy, Strategy, Workload};

/// Latency percentiles (nanoseconds) for the `log()` hot-path call.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LatencyStats {
    pub count: u64,
    pub mean_ns: f64,
    pub min_ns: u64,
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub max_ns: u64,
}

impl LatencyStats {
    /// Summarise a histogram of per-call latencies (in nanoseconds).
    pub fn from_hist(h: &Histogram<u64>) -> Self {
        LatencyStats {
            count: h.len(),
            mean_ns: h.mean(),
            min_ns: h.min(),
            p50_ns: h.value_at_quantile(0.50),
            p90_ns: h.value_at_quantile(0.90),
            p99_ns: h.value_at_quantile(0.99),
            p999_ns: h.value_at_quantile(0.999),
            max_ns: h.max(),
        }
    }
}

/// A thin wrapper to build per-producer histograms with consistent bounds.
///
/// We track 1 ns .. 60 s with 3 significant figures — plenty of resolution for
/// log-call latencies while keeping the histograms small enough to merge cheaply.
pub fn new_latency_hist() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).expect("valid histogram bounds")
}

/// Everything we learned from running one cell of the sweep matrix.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseResult {
    // --- what was run ---
    pub strategy: Strategy,
    pub producers: usize,
    pub messages_per_producer: u64,
    pub total_messages: u64,
    pub msg_size: usize,
    pub capacity: usize,
    pub writer_buf_bytes: usize,
    pub full_policy: FullPolicy,
    /// Target rate per producer if pacing was requested, else `None`.
    pub target_rate_per_producer: Option<f64>,

    // --- what happened ---
    /// Records dropped because a bounded buffer was full (only possible under
    /// [`FullPolicy::Drop`]).
    pub dropped: u64,
    /// Wall-clock seconds for all producers to enqueue their records.
    pub enqueue_secs: f64,
    /// Seconds spent draining/flushing the background writer after producers
    /// finished. Captures the cost of *not* losing buffered records.
    pub drain_secs: f64,
    /// Throughput measured over the enqueue phase (records/second).
    pub enqueue_throughput: f64,
    /// Throughput measured over enqueue + drain (records/second) — the honest
    /// end-to-end number including the cost of durably flushing.
    pub end_to_end_throughput: f64,
    /// Payload megabytes per second over enqueue + drain.
    pub mb_per_sec: f64,
    /// Hot-path latency distribution of the `log()` call.
    pub latency: LatencyStats,
}

impl CaseResult {
    /// Build a result from a finished run.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        strategy: Strategy,
        workload: &Workload,
        capacity: usize,
        writer_buf_bytes: usize,
        full_policy: FullPolicy,
        latency: &Histogram<u64>,
        dropped: u64,
        enqueue_secs: f64,
        drain_secs: f64,
    ) -> Self {
        let total = workload.total_messages();
        let delivered = total.saturating_sub(dropped);
        let enqueue_throughput = if enqueue_secs > 0.0 {
            total as f64 / enqueue_secs
        } else {
            0.0
        };
        let total_secs = enqueue_secs + drain_secs;
        let end_to_end_throughput = if total_secs > 0.0 {
            delivered as f64 / total_secs
        } else {
            0.0
        };
        let mb_per_sec = if total_secs > 0.0 {
            (delivered as f64 * workload.msg_size as f64) / total_secs / 1.0e6
        } else {
            0.0
        };

        CaseResult {
            strategy,
            producers: workload.producers,
            messages_per_producer: workload.messages_per_producer,
            total_messages: total,
            msg_size: workload.msg_size,
            capacity,
            writer_buf_bytes,
            full_policy,
            target_rate_per_producer: workload.target_rate_per_producer,
            dropped,
            enqueue_secs,
            drain_secs,
            enqueue_throughput,
            end_to_end_throughput,
            mb_per_sec,
            latency: LatencyStats::from_hist(latency),
        }
    }
}

/// Render a nanosecond duration compactly (ns / µs / ms / s).
pub fn fmt_ns(ns: f64) -> String {
    if ns < 1_000.0 {
        format!("{ns:.0}ns")
    } else if ns < 1_000_000.0 {
        format!("{:.2}µs", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.2}ms", ns / 1_000_000.0)
    } else {
        format!("{:.2}s", ns / 1_000_000_000.0)
    }
}

/// Render a records/second figure compactly.
pub fn fmt_rate(r: f64) -> String {
    if r >= 1.0e6 {
        format!("{:.2}M/s", r / 1.0e6)
    } else if r >= 1.0e3 {
        format!("{:.1}k/s", r / 1.0e3)
    } else {
        format!("{r:.0}/s")
    }
}
