//! The full `tracing` instrumentation stack.
//!
//! Three variants, all driving real `tracing` events through a
//! `tracing-subscriber` `fmt` layer so the measurement includes event
//! construction, field visiting, formatting and the writer:
//!
//! * [`build_fmt`] — synchronous `fmt` layer writing to a buffered file;
//! * [`build_non_blocking`] — the same layer over a `tracing-appender`
//!   non-blocking writer (the idiomatic non-blocking `tracing` file stack);
//! * [`build_span`] — like `fmt`, but every event is wrapped in an entered span
//!   to expose the cost of span-based instrumentation;
//! * [`build_json`] — a deliberately *combined* stack that layers several
//!   logging types at once: real structured key/value fields, a JSON formatter,
//!   and the `tracing-appender` non-blocking async transport. It stands in for a
//!   realistic production logging solution rather than isolating one concern.
//!
//! `tracing` installs a global default subscriber, which can be set only once
//! per process, so these are built behind [`claim_global`] and the overnight
//! harness runs each in its own process.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::MakeWriter;

use super::{claim_global, record_str, Logger};
use crate::config::{LoggerConfig, Strategy};

/// A buffered file that several `tracing` writer handles can share. We flush it
/// explicitly in `finish()` so a synchronous `fmt` layer is durable per case.
#[derive(Clone)]
struct SharedFile(Arc<Mutex<BufWriter<File>>>);

/// The short-lived writer handed to the `fmt` layer for one event.
struct SharedFileGuard<'a>(MutexGuard<'a, BufWriter<File>>);

impl Write for SharedFileGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<'a> MakeWriter<'a> for SharedFile {
    type Writer = SharedFileGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        SharedFileGuard(self.0.lock().expect("shared file mutex poisoned"))
    }
}

/// Kept-alive process-global state for whichever tracing variant is installed.
static SYNC_SINK: OnceLock<SharedFile> = OnceLock::new();
static NB_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// Handle over the installed global tracing subscriber.
struct TracingLogger {
    /// Whether to wrap each event in an entered span (the `tracing-span` variant).
    enter_span: bool,
    /// Whether a synchronous sink needs an explicit flush in `finish()`.
    sync_flush: bool,
    /// Whether to attach real structured key/value fields to each event (the
    /// combined `tracing-json` stack). The other variants log just the message.
    structured: bool,
}

impl Logger for TracingLogger {
    fn log(&self, record: &[u8]) {
        let msg = record_str(record);
        if self.structured {
            // Emit genuine structured fields alongside the message; the JSON
            // formatter renders them as keys, exercising the field-encoding path
            // a structured logging solution actually pays for.
            tracing::info!(target: "logbench", bytes = msg.len(), "{}", msg);
        } else if self.enter_span {
            let span = tracing::info_span!(target: "logbench", "op");
            let _enter = span.enter();
            tracing::info!(target: "logbench", "{}", msg);
        } else {
            tracing::info!(target: "logbench", "{}", msg);
        }
    }

    fn finish(&self) -> u64 {
        if self.sync_flush {
            if let Some(sink) = SYNC_SINK.get() {
                let _ = sink.0.lock().expect("shared file mutex poisoned").flush();
            }
        }
        // The non-blocking worker has no public mid-life flush; it drains on
        // process exit when its guard drops. tracing does not surface a dropped
        // count, so we report 0 here.
        0
    }
}

fn build_subscriber_sync(cfg: &LoggerConfig) -> std::io::Result<()> {
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    let buf = cfg.writer_buf_bytes.max(1);
    INIT.call_once(|| {
        result = (|| {
            let file = File::create(&path)?;
            let sink = SharedFile(Arc::new(Mutex::new(BufWriter::with_capacity(buf, file))));
            let _ = SYNC_SINK.set(sink.clone());
            let subscriber = tracing_subscriber::fmt()
                .with_writer(sink)
                .with_ansi(false)
                .with_target(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).map_err(to_io_err)
        })();
    });
    result
}

pub fn build_fmt(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::TracingFmt)?;
    build_subscriber_sync(cfg)?;
    Ok(Arc::new(TracingLogger {
        enter_span: false,
        sync_flush: true,
        structured: false,
    }))
}

pub fn build_span(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::TracingSpan)?;
    build_subscriber_sync(cfg)?;
    Ok(Arc::new(TracingLogger {
        enter_span: true,
        sync_flush: true,
        structured: false,
    }))
}

pub fn build_non_blocking(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::TracingNonBlocking)?;
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    let cap = cfg.capacity;
    INIT.call_once(|| {
        result = (|| {
            let file = File::create(&path)?;
            let mut builder = tracing_appender::non_blocking::NonBlockingBuilder::default();
            if cap > 0 {
                builder = builder.buffered_lines_limit(cap);
            }
            let (writer, guard) = builder.finish(file);
            let _ = NB_GUARD.set(guard);
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).map_err(to_io_err)
        })();
    });
    result?;
    Ok(Arc::new(TracingLogger {
        enter_span: false,
        sync_flush: false,
        structured: false,
    }))
}

/// The combined stack: structured fields + JSON formatting + non-blocking async
/// transport. This layers four logging "types" (facade, structured fields,
/// formatting, async hand-off) into one logger to model a realistic production
/// solution rather than isolating a single concern.
pub fn build_json(cfg: &LoggerConfig) -> std::io::Result<Arc<dyn Logger>> {
    claim_global(Strategy::TracingJson)?;
    static INIT: Once = Once::new();
    let mut result = Ok(());
    let path = cfg.path.clone();
    let cap = cfg.capacity;
    INIT.call_once(|| {
        result = (|| {
            let file = File::create(&path)?;
            let mut builder = tracing_appender::non_blocking::NonBlockingBuilder::default();
            if cap > 0 {
                builder = builder.buffered_lines_limit(cap);
            }
            let (writer, guard) = builder.finish(file);
            let _ = NB_GUARD.set(guard);
            let subscriber = tracing_subscriber::fmt()
                .json()
                .with_writer(writer)
                .with_target(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).map_err(to_io_err)
        })();
    });
    result?;
    Ok(Arc::new(TracingLogger {
        enter_span: false,
        sync_flush: false,
        structured: true,
    }))
}

fn to_io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}
