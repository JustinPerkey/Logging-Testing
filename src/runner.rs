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
fn run_producer(
    logger: Arc<dyn Logger>,
    barrier: Arc<Barrier>,
    workload: Workload,
    producer_idx: usize,
) -> Histogram<u64> {
    let payload = make_payload(workload.msg_size, producer_idx);
    let mut hist = new_latency_hist();

    // Warm up: spin the background writer and caches without measuring.
    for _ in 0..workload.warmup {
        logger.log(&payload);
    }

    // All producers start the measured phase together.
    barrier.wait();

    let interval = workload
        .target_rate_per_producer
        .filter(|r| *r > 0.0)
        .map(|r| Duration::from_secs_f64(1.0 / r));
    let start = Instant::now();

    for i in 0..workload.messages_per_producer {
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

    // Release the producers and time the enqueue phase.
    barrier.wait();
    let enqueue_start = Instant::now();

    let mut combined = new_latency_hist();
    for h in handles {
        let hist = h.join().expect("producer thread panicked");
        combined.add(&hist).expect("compatible histogram bounds");
    }
    let enqueue_secs = enqueue_start.elapsed().as_secs_f64();

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
    ))
}
