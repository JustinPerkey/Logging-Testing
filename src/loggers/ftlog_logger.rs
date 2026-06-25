//! The high-throughput `ftlog` logger.
//!
//! `ftlog` is purpose-built for low-latency logging: the producing thread does
//! almost nothing, handing the record to a dedicated log thread over a channel.
//! It plugs into the `log` facade, so the hot path is a normal `log::info!` and
//! `finish()` drains via `log::logger().flush()` (which `ftlog` implements as a
//! blocking flush of its queue).
//!
//! Like the other facade backends it installs a global logger, so it is built
//! behind [`claim_global`] and the overnight harness runs it in its own process.

use std::any::Any;
use std::sync::{Arc, Mutex, Once};

use ftlog::appender::FileAppender;

use super::{claim_global, record_str, Logger};
use crate::config::{FullPolicy, LoggerConfig, Strategy};

/// Keeps the `ftlog` guard alive for the process so its log thread keeps running.
static KEEPALIVE: Mutex<Vec<Box<dyn Any + Send>>> = Mutex::new(Vec::new());

struct FtlogLogger;

impl Logger for FtlogLogger {
    fn log(&self, record: &[u8]) {
        log::info!(target: "logbench", "{}", record_str(record));
    }

    fn finish(&self) -> u64 {
        // ftlog implements the facade flush as a blocking drain of its queue.
        log::logger().flush();
        0
    }
}

pub fn build(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::Ftlog)?;
    install(cfg)?;
    Ok(Arc::new(FtlogLogger))
}

fn install(cfg: &LoggerConfig) -> std::io::Result<()> {
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    let capacity = cfg.capacity;
    let block = matches!(cfg.full_policy, FullPolicy::Block);
    INIT.call_once(|| {
        result = (|| {
            let appender = FileAppender::new(&path);
            let mut builder = ftlog::builder()
                .max_log_level(log::LevelFilter::Info)
                .root(appender);
            builder = if capacity == 0 {
                builder.unbounded()
            } else {
                builder.bounded(capacity, block)
            };
            let guard = builder
                .try_init()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            KEEPALIVE
                .lock()
                .expect("ftlog keepalive mutex poisoned")
                .push(Box::new(guard));
            Ok(())
        })();
    });
    result
}
