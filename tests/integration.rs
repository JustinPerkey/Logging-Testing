//! End-to-end checks that each strategy behaves correctly: lossless strategies
//! durably write every record, and the drop policy actually drops under load.

use std::sync::Arc;

use logbench::config::{FullPolicy, LoggerConfig, Strategy, Workload};
use logbench::loggers;
use logbench::runner::run_case;

const MSG_SIZE: usize = 64;

fn small_workload(producers: usize, messages: u64, warmup: u64) -> Workload {
    Workload {
        producers,
        messages_per_producer: messages,
        msg_size: MSG_SIZE,
        target_rate_per_producer: None,
        warmup,
        lines_per_log: 0,
    }
}

/// Under the lossless (block) policy every strategy must write exactly
/// `(warmup + messages) * producers` records of `MSG_SIZE` bytes — nothing lost.
#[test]
fn block_policy_is_lossless_and_durable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let producers = 3;
    let messages = 5_000;
    let warmup = 200;

    for &strategy in &Strategy::ALL {
        let path = dir.path().join(format!("{}.log", strategy.name()));
        let cfg = LoggerConfig {
            path: path.clone(),
            capacity: 1024,
            writer_buf_bytes: 16 * 1024,
            full_policy: FullPolicy::Block,
        };
        let workload = small_workload(producers, messages, warmup);

        let result = run_case(strategy, &cfg, workload).expect("run_case");
        assert_eq!(
            result.dropped,
            0,
            "{} dropped under block policy",
            strategy.name()
        );
        assert_eq!(result.total_messages, messages * producers as u64);

        // finish() guarantees the background writer flushed, so the file must
        // hold every measured *and* warmup record.
        let expected_records = (messages + warmup) * producers as u64;
        let expected_bytes = expected_records * MSG_SIZE as u64;
        let actual = std::fs::metadata(&path).expect("log file exists").len();
        assert_eq!(
            actual,
            expected_bytes,
            "{} wrote {} bytes, expected {}",
            strategy.name(),
            actual,
            expected_bytes
        );
    }
}

/// A tiny channel under the drop policy, hammered hard, should lose some
/// records rather than block — proving the non-blocking guarantee.
#[test]
fn drop_policy_can_drop_under_pressure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = LoggerConfig {
        path: dir.path().join("crossbeam-drop.log"),
        capacity: 1,         // pathologically small to force drops
        writer_buf_bytes: 1, // tiny writer buffer slows the consumer
        full_policy: FullPolicy::Drop,
    };
    let workload = small_workload(4, 50_000, 0);

    let result = run_case(Strategy::Crossbeam, &cfg, workload).expect("run_case");
    assert!(
        result.dropped > 0,
        "expected some drops with capacity=1 under load, got {}",
        result.dropped
    );
    assert!(result.dropped <= result.total_messages);
}

/// `slog` is the one real-crate strategy that is not process-global, so we can
/// exercise it directly here: it must accept records, drop nothing under the
/// block policy, and durably write a non-empty file.
#[test]
fn slog_async_writes_durably() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slog.log");
    let cfg = LoggerConfig {
        path: path.clone(),
        capacity: 4096,
        writer_buf_bytes: 16 * 1024,
        full_policy: FullPolicy::Block,
    };
    let workload = small_workload(2, 5_000, 100);

    let result = run_case(Strategy::SlogAsync, &cfg, workload).expect("run_case");
    assert_eq!(result.dropped, 0, "slog-async dropped under block policy");
    assert_eq!(result.total_messages, 5_000 * 2);

    // slog adds its own timestamp/level, so we can't assert exact byte counts,
    // but every record must have made it to disk: the file must be non-empty and
    // hold at least one line per measured record.
    let contents = std::fs::read_to_string(&path).expect("read slog log");
    let lines = contents.lines().count() as u64;
    assert!(
        lines >= result.total_messages,
        "slog wrote {lines} lines, expected at least {}",
        result.total_messages
    );
}

/// Only one global logging crate can be installed per process; a second,
/// different global strategy must fail with a clear error instead of panicking.
#[test]
fn one_global_logger_per_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mk = |name: &str| LoggerConfig {
        path: dir.path().join(format!("{name}.log")),
        capacity: 1024,
        writer_buf_bytes: 4096,
        full_policy: FullPolicy::Block,
    };

    // First global install succeeds.
    loggers::build(Strategy::LogEnvLogger, &mk("env")).expect("install env_logger");
    // A second, different global must be refused.
    match loggers::build(Strategy::LogFern, &mk("fern")) {
        Ok(_) => panic!("a second, different global logger must be refused"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
    }
    // Re-building the same global strategy is fine (later cases reuse it).
    loggers::build(Strategy::LogEnvLogger, &mk("env2")).expect("reuse env_logger");
}

/// `finish()` must be safe to drive directly (used by the Criterion bench too).
#[test]
fn logger_builds_and_finishes_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    for &strategy in &Strategy::ALL {
        let cfg = LoggerConfig {
            path: dir.path().join(format!("{}-direct.log", strategy.name())),
            capacity: 256,
            writer_buf_bytes: 4096,
            full_policy: FullPolicy::Block,
        };
        let logger: Arc<dyn loggers::Logger> = loggers::build(strategy, &cfg).expect("build");
        let payload = vec![b'z'; MSG_SIZE];
        for _ in 0..1000 {
            logger.log(&payload);
        }
        assert_eq!(logger.finish(), 0);
    }
}
