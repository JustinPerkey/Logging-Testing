//! Criterion micro-benchmarks for the `log()` hot-path call of each strategy.
//!
//! The main `logbench` binary is the primary tool (it sweeps a whole matrix and
//! reports producer-side latency distributions). This Criterion harness is a
//! complementary, statistically rigorous look at the single-call cost of each
//! strategy at a couple of message sizes. Run with `cargo bench`.

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use logbench::config::{FullPolicy, LoggerConfig, Strategy};
use logbench::loggers;

fn bench_log_call(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");

    let mut group = c.benchmark_group("log_call");
    for &size in &[64usize, 1024] {
        let payload = vec![b'x'; size];
        group.throughput(Throughput::Bytes(size as u64));

        for &strategy in &Strategy::ALL {
            let path = dir.path().join(format!("{}_{}.log", strategy.name(), size));
            let cfg = LoggerConfig {
                path: path.clone(),
                capacity: 8192,
                writer_buf_bytes: 64 * 1024,
                full_policy: FullPolicy::Block,
            };
            let logger: Arc<dyn loggers::Logger> =
                loggers::build(strategy, &cfg).expect("build logger");

            group.bench_with_input(
                BenchmarkId::new(strategy.name(), size),
                &payload,
                |b, payload| {
                    b.iter(|| logger.log(std::hint::black_box(payload)));
                },
            );

            // Drain so the background thread is cleanly shut down between cases.
            logger.finish();
            // Criterion runs the closure millions of times, so each strategy's
            // file can grow large. Reclaim it now (the logger is drained and
            // dropped) rather than letting every case pile up in the tempdir.
            let _ = std::fs::remove_file(&path);
        }
    }
    group.finish();
}

criterion_group!(benches, bench_log_call);
criterion_main!(benches);
