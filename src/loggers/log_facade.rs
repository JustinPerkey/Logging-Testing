//! Real `log`-facade backends: `env_logger`, `fern`, `log4rs` and
//! `flexi_logger`.
//!
//! These all route `log::info!` through the `log` facade to a file, but each
//! crate brings its own formatting, level filtering and writer machinery — which
//! is exactly the difference this benchmark exists to surface. The producing
//! thread calls `log::info!`, so every measurement includes the crate's real
//! per-record cost (timestamp, level check, format, hand-off to its sink).
//!
//! The `log` facade allows only **one** global logger per process, so each of
//! these is built behind [`claim_global`] and installed exactly once; later
//! cases in the same process reuse the already-installed logger. The overnight
//! harness runs each backend in its own process.

use std::any::Any;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};

use super::{claim_global, record_str, Logger};
use crate::config::{LoggerConfig, Strategy};

/// Keeps process-global guards/handles (e.g. a `flexi_logger` `LoggerHandle`)
/// alive for the lifetime of the process so their background workers keep
/// running. We only ever install one global backend per process.
static KEEPALIVE: Mutex<Vec<Box<dyn Any + Send>>> = Mutex::new(Vec::new());

fn keep_alive(guard: Box<dyn Any + Send>) {
    KEEPALIVE
        .lock()
        .expect("keepalive mutex poisoned")
        .push(guard);
}

/// A thin handle over an already-installed global `log`-facade logger.
///
/// `log()` emits a real `log::info!` event; `finish()` runs the crate's flush so
/// every record is durably written before the next case starts.
struct FacadeLogger {
    /// How to durably flush the backend (drain its buffers/async queue).
    flush: Box<dyn Fn() + Send + Sync>,
}

impl Logger for FacadeLogger {
    fn log(&self, record: &[u8]) {
        log::info!(target: "logbench", "{}", record_str(record));
    }

    fn finish(&self) -> u64 {
        (self.flush)();
        0
    }
}

/// Flush whatever logger is currently installed behind the `log` facade.
fn facade_flush() {
    log::logger().flush();
}

// ---------------------------------------------------------------------------
// env_logger
// ---------------------------------------------------------------------------

pub fn build_env_logger(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::LogEnvLogger)?;
    install_env_logger(cfg)?;
    Ok(Arc::new(FacadeLogger {
        flush: Box::new(facade_flush),
    }))
}

fn install_env_logger(cfg: &LoggerConfig) -> std::io::Result<()> {
    use std::sync::Once;
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    INIT.call_once(|| {
        result = (|| {
            let file = File::create(&path)?;
            let mut builder = env_logger::Builder::new();
            builder
                .target(env_logger::Target::Pipe(Box::new(file)))
                .filter_level(log::LevelFilter::Info)
                .format(|buf, record| {
                    writeln!(
                        buf,
                        "{} [{}] {}",
                        humantime_now(),
                        record.level(),
                        record.args()
                    )
                });
            let logger = builder.build();
            log::set_max_level(logger.filter());
            log::set_boxed_logger(Box::new(logger)).map_err(to_io_err)
        })();
    });
    result
}

/// A cheap timestamp string for the env_logger/fern formatters. We intentionally
/// build it ourselves (rather than pulling another time crate) using
/// `SystemTime` so the formatting cost is comparable across the `log` backends.
fn humantime_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// fern
// ---------------------------------------------------------------------------

pub fn build_fern(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::LogFern)?;
    install_fern(cfg)?;
    Ok(Arc::new(FacadeLogger {
        flush: Box::new(facade_flush),
    }))
}

fn install_fern(cfg: &LoggerConfig) -> std::io::Result<()> {
    use std::sync::Once;
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    INIT.call_once(|| {
        result = (|| {
            let dispatch = fern::Dispatch::new()
                .format(|out, message, record| {
                    out.finish(format_args!(
                        "{} [{}] {}",
                        humantime_now(),
                        record.level(),
                        message
                    ))
                })
                .level(log::LevelFilter::Info)
                .chain(fern::log_file(&path).map_err(to_io_err)?);
            let (level, logger) = dispatch.into_log();
            log::set_max_level(level);
            log::set_boxed_logger(logger).map_err(to_io_err)
        })();
    });
    result
}

// ---------------------------------------------------------------------------
// log4rs
// ---------------------------------------------------------------------------

pub fn build_log4rs(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::LogLog4rs)?;
    install_log4rs(cfg)?;
    Ok(Arc::new(FacadeLogger {
        flush: Box::new(facade_flush),
    }))
}

fn install_log4rs(cfg: &LoggerConfig) -> std::io::Result<()> {
    use log4rs::append::file::FileAppender;
    use log4rs::config::{Appender, Config, Root};
    use log4rs::encode::pattern::PatternEncoder;
    use std::sync::Once;

    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    INIT.call_once(|| {
        result = (|| {
            let appender = FileAppender::builder()
                .encoder(Box::new(PatternEncoder::new("{d(%+)} [{l}] {m}{n}")))
                .build(&path)
                .map_err(to_io_err)?;
            let config = Config::builder()
                .appender(Appender::builder().build("file", Box::new(appender)))
                .build(
                    Root::builder()
                        .appender("file")
                        .build(log::LevelFilter::Info),
                )
                .map_err(to_io_err)?;
            let handle = log4rs::init_config(config).map_err(to_io_err)?;
            keep_alive(Box::new(handle));
            Ok(())
        })();
    });
    result
}

// ---------------------------------------------------------------------------
// flexi_logger (buffered file write mode)
// ---------------------------------------------------------------------------

pub fn build_flexi(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::LogFlexi)?;
    install_flexi(cfg)?;
    // flexi exposes flush on its handle; route the facade flush through it.
    Ok(Arc::new(FacadeLogger {
        flush: Box::new(flexi_flush),
    }))
}

fn flexi_flush() {
    // The handle is the canonical flush path for flexi's buffered writer.
    if let Some(h) = FLEXI_HANDLE
        .lock()
        .expect("flexi handle mutex poisoned")
        .as_ref()
    {
        h.flush();
    } else {
        facade_flush();
    }
}

static FLEXI_HANDLE: Mutex<Option<flexi_logger::LoggerHandle>> = Mutex::new(None);

fn install_flexi(cfg: &LoggerConfig) -> std::io::Result<()> {
    use flexi_logger::{FileSpec, Logger as FlexiBuilder, WriteMode};
    use std::sync::Once;
    use std::time::Duration;

    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    let buf_bytes = cfg.writer_buf_bytes;
    INIT.call_once(|| {
        result = (|| {
            let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "logbench".into());
            let handle = FlexiBuilder::try_with_str("info")
                .map_err(to_io_err)?
                .log_to_file(
                    FileSpec::default()
                        .directory(dir)
                        .basename(stem)
                        .suppress_timestamp()
                        .suffix("log"),
                )
                // flexi's `WriteMode::Async` hands records to a background thread
                // through an *unbounded* channel, so under an unbounded firehose
                // (rate=max) the queue grows without limit and the process is
                // OOM-killed (rc=137). Every other strategy here bounds memory and
                // applies back-pressure; match that by buffering on the producing
                // thread instead. The fixed-size `BufWriter` blocks the caller on
                // the file write once full — lossless back-pressure consistent with
                // `full_policy=block` — using the same `writer_buf_bytes` knob as
                // the `direct` baseline. Records are still durably flushed by
                // `finish()` (and periodically by flexi's flush thread).
                .write_mode(WriteMode::BufferAndFlushWith(
                    buf_bytes,
                    Duration::from_secs(1),
                ))
                .start()
                .map_err(to_io_err)?;
            *FLEXI_HANDLE.lock().expect("flexi handle mutex poisoned") = Some(handle);
            Ok(())
        })();
    });
    result
}

// ---------------------------------------------------------------------------

/// Map any error into an `io::Error` so the `build` signature stays uniform.
fn to_io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
