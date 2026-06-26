//! The benchmark measurement loop.

use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

use crate::config::{LoggerConfig, Strategy, Workload};
use crate::loggers::{self, Logger};
use crate::metrics::{new_latency_hist, CaseResult};

/// Build a log record of the requested size.
///
/// The payload is real bytes (so the OS actually writes them) and ends in a
/// newline so the output file is a valid line-oriented log. A rotating ASCII
/// pattern keeps it from being a single repeated byte.
fn make_payload(size: usize, producer: usize) -> Box<[u8]> {
    let size = size.max(1);
    let mut buf = vec![0u8; size];
    let tag = b"logbench record payload ";
    for (i, b) in buf.iter_mut().enumerate() {
        *b = tag[(i + producer) % tag.len()];
    }
    // Guarantee a trailing newline so downstream tooling sees discrete lines.
    buf[size - 1] = b'\n';
    buf.into_boxed_slice()
}

/// Perform `lines` units of synthetic CPU work, standing in for that many
/// "lines of code" running between two `log()` calls.
///
/// Each unit threads an accumulator through a cheap, data-dependent arithmetic
/// step (a PCG-style LCG mix) behind [`std::hint::black_box`], so the optimizer
/// can neither elide the loop nor hoist it out: the work is genuinely executed
/// on the producing thread, exactly as real interleaved code would be. The
/// returned accumulator must be consumed (e.g. black-boxed) by the caller so the
/// whole computation stays observable.
#[inline]
fn do_work(lines: u64, mut acc: u64) -> u64 {
    for _ in 0..lines {
        acc = acc
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        acc ^= acc >> 33;
        acc = std::hint::black_box(acc);
    }
    acc
}

/// Estimate the wall-clock cost of one [`do_work`] unit ("line of code") on this
/// machine, in nanoseconds.
///
/// Runs a fixed, warmed batch single-threaded so the figure reflects the
/// intrinsic per-line cost without scheduler/contention noise. It is reported
/// alongside results purely as context for `lines_per_log` — the headline
/// slowdown is measured empirically per case, not derived from this number.
pub fn calibrate_ns_per_line() -> f64 {
    const WARM: u64 = 2_000_000;
    const ITERS: u64 = 20_000_000;
    // Warm caches / branch predictor and let any frequency scaling settle.
    let mut acc = std::hint::black_box(0x9e3779b97f4a7c15u64);
    acc = do_work(WARM, acc);
    let start = Instant::now();
    acc = do_work(ITERS, acc);
    let elapsed = start.elapsed().as_nanos() as f64;
    std::hint::black_box(acc);
    elapsed / ITERS as f64
}

/// Sleep/spin until `deadline`. Uses a coarse sleep for the bulk of the wait
/// and a short spin at the end to stay accurate enough for rate pacing.
fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline - now;
        if remaining > Duration::from_micros(1_500) {
            std::thread::sleep(remaining - Duration::from_micros(1_000));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Run a single producer thread, returning its per-call latency histogram.
///
/// When `workload.lines_per_log > 0` the thread runs two barrier-synced phases:
/// first a **work-only baseline** (the synthetic inter-log work with no logging)
/// and then the **measured phase** (the same work *plus* a timed `log()` after
/// every `lines_per_log` units). The runner times each phase's wall clock to
/// derive the logging slowdown. With `lines_per_log == 0` there is no baseline
/// phase and the loop is exactly a back-to-back logging measurement.
fn run_producer(
    logger: Arc<dyn Logger>,
    barrier: Arc<Barrier>,
    workload: Workload,
    producer_idx: usize,
) -> Histogram<u64> {
    let payload = make_payload(workload.msg_size, producer_idx);
    let mut hist = new_latency_hist();
    let lines = workload.lines_per_log;
    // Per-producer seed so threads don't share a work accumulator value.
    let mut acc = 0x9e3779b97f4a7c15u64 ^ producer_idx as u64;

    // Warm up: spin the background writer and caches without measuring.
    for _ in 0..workload.warmup {
        logger.log(&payload);
    }

    // --- Phase A: no-logging baseline (only when the work model is enabled) ---
    // Run exactly the same amount of inter-log work as the measured phase but
    // without any `log()` call, so its wall time is the "without logging" cost.
    if lines > 0 {
        barrier.wait();
        for _ in 0..workload.messages_per_producer {
            acc = do_work(lines, acc);
        }
        // Keep the baseline work from being optimized away.
        std::hint::black_box(acc);
    }

    // --- Phase B: the measured phase (work + timed log() calls) ---
    // All producers start it together (this is the only rendezvous when the
    // work model is disabled).
    barrier.wait();

    let interval = workload
        .target_rate_per_producer
        .filter(|r| *r > 0.0)
        .map(|r| Duration::from_secs_f64(1.0 / r));
    let start = Instant::now();

    for i in 0..workload.messages_per_producer {
        if lines > 0 {
            acc = do_work(lines, acc);
        }
        if let Some(interval) = interval {
            // Schedule against an absolute timeline so we don't accumulate drift.
            wait_until(start + interval.mul_f64(i as f64));
        }
        let t0 = Instant::now();
        logger.log(&payload);
        let elapsed = t0.elapsed().as_nanos() as u64;
        // Clamp into the histogram's representable range.
        let _ = hist.record(elapsed.clamp(1, 60_000_000_000));
    }
    std::hint::black_box(acc);

    hist
}

/// Run one cell of the sweep matrix and return its [`CaseResult`].
pub fn run_case(
    strategy: Strategy,
    cfg: &LoggerConfig,
    workload: Workload,
) -> std::io::Result<CaseResult> {
    let logger = loggers::build(strategy, cfg)?;
    let barrier = Arc::new(Barrier::new(workload.producers + 1));

    let mut handles = Vec::with_capacity(workload.producers);
    for idx in 0..workload.producers {
        let logger = Arc::clone(&logger);
        let barrier = Arc::clone(&barrier);
        handles.push(
            std::thread::Builder::new()
                .name(format!("logbench-producer-{idx}"))
                .spawn(move || run_producer(logger, barrier, workload, idx))
                .expect("spawn producer"),
        );
    }

    let work_enabled = workload.lines_per_log > 0;

    // Phase A (no-logging baseline) — only when the work model is enabled. The
    // producers gate on the barrier at the start and end of this phase, so the
    // runner can time it on the wall clock. The reused barrier then doubles as
    // the release into the measured phase.
    let work_only_secs = if work_enabled {
        barrier.wait(); // release into the baseline phase
        let baseline_start = Instant::now();
        barrier.wait(); // all producers finished the baseline phase
        baseline_start.elapsed().as_secs_f64()
    } else {
        0.0
    };

    // Release the producers into the measured phase (for the work-enabled path
    // the barrier.wait() above that ended phase A already released them, so this
    // timestamp marks phase B's start; for the disabled path this barrier is the
    // single release).
    let enqueue_start = if work_enabled {
        Instant::now()
    } else {
        barrier.wait();
        Instant::now()
    };

    let mut combined = new_latency_hist();
    for h in handles {
        let hist = h.join().expect("producer thread panicked");
        combined.add(&hist).expect("compatible histogram bounds");
    }
    let measured_secs = enqueue_start.elapsed().as_secs_f64();

    // When the work model is on, the measured phase includes the inter-log work,
    // so the logging-attributable wall time is the measured time minus the
    // baseline work time. That keeps throughput an apples-to-apples logging
    // figure and is what the slowdown percentage is computed against.
    let enqueue_secs = if work_enabled {
        (measured_secs - work_only_secs).max(0.0)
    } else {
        measured_secs
    };

    // Drain: flush the background writer so nothing is left buffered.
    let drain_start = Instant::now();
    let dropped = logger.finish();
    let drain_secs = drain_start.elapsed().as_secs_f64();

    Ok(CaseResult::new(
        strategy,
        &workload,
        cfg.capacity,
        cfg.writer_buf_bytes,
        cfg.full_policy,
        &combined,
        dropped,
        enqueue_secs,
        drain_secs,
        work_only_secs,
    ))
}
