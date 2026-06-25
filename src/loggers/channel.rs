//! Channel + background-writer strategies (crossbeam and flume).
//!
//! Both strategies share the same shape: the producer allocates the record and
//! sends it over a channel; a single background thread owns the `BufWriter` and
//! drains the channel. The only difference is which channel implementation is
//! used, so the worker body and the `Logger` glue are shared here.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use super::Logger;
use crate::config::{FullPolicy, LoggerConfig};

/// Message passed to the background writer thread.
enum Msg {
    /// A log record to write.
    Record(Box<[u8]>),
    /// Flush everything written so far, acknowledge, and stop the worker.
    Shutdown(crossbeam_channel::Sender<()>),
}

/// Drain `recv` (an iterator of [`Msg`]) into `writer`. Returns when a
/// [`Msg::Shutdown`] is seen (after flushing) or the channel disconnects.
fn run_worker<I>(mut writer: BufWriter<File>, recv: I)
where
    I: IntoIterator<Item = Msg>,
{
    for msg in recv {
        match msg {
            Msg::Record(bytes) => {
                let _ = writer.write_all(&bytes);
            }
            Msg::Shutdown(ack) => {
                let _ = writer.flush();
                let _ = ack.send(());
                return;
            }
        }
    }
    let _ = writer.flush();
}

/// Shared finish logic: tell the worker to flush+stop, wait for the ack, join.
///
/// We never rely on sender disconnect here because the hot-path sender is kept
/// alive for the lifetime of the logger; an explicit shutdown message is what
/// lets us join the worker deterministically.
fn finish_via_shutdown(
    send: impl FnOnce(Msg),
    handle: &Mutex<Option<JoinHandle<()>>>,
    dropped: &AtomicU64,
) -> u64 {
    let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
    send(Msg::Shutdown(ack_tx));
    let _ = ack_rx.recv();
    if let Some(h) = handle.lock().expect("handle mutex poisoned").take() {
        let _ = h.join();
    }
    dropped.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// crossbeam-channel
// ---------------------------------------------------------------------------

/// Background-writer logger backed by [`crossbeam_channel`].
pub struct CrossbeamLogger {
    sender: crossbeam_channel::Sender<Msg>,
    handle: Mutex<Option<JoinHandle<()>>>,
    full_policy: FullPolicy,
    dropped: AtomicU64,
}

impl CrossbeamLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let file = File::create(&cfg.path)?;
        let writer = BufWriter::with_capacity(cfg.writer_buf_bytes.max(1), file);
        let (tx, rx) = if cfg.capacity == 0 {
            crossbeam_channel::unbounded()
        } else {
            crossbeam_channel::bounded(cfg.capacity)
        };
        let handle = std::thread::Builder::new()
            .name("logbench-crossbeam".into())
            .spawn(move || run_worker(writer, rx))?;
        Ok(CrossbeamLogger {
            sender: tx,
            handle: Mutex::new(Some(handle)),
            full_policy: cfg.full_policy,
            dropped: AtomicU64::new(0),
        })
    }
}

impl Logger for CrossbeamLogger {
    fn log(&self, record: &[u8]) {
        let msg = Msg::Record(Box::from(record));
        match self.full_policy {
            FullPolicy::Block => {
                let _ = self.sender.send(msg);
            }
            FullPolicy::Drop => {
                if self.sender.try_send(msg).is_err() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn finish(&self) -> u64 {
        finish_via_shutdown(
            |m| {
                let _ = self.sender.send(m);
            },
            &self.handle,
            &self.dropped,
        )
    }
}

// ---------------------------------------------------------------------------
// flume
// ---------------------------------------------------------------------------

/// Background-writer logger backed by [`flume`].
pub struct FlumeLogger {
    sender: flume::Sender<Msg>,
    handle: Mutex<Option<JoinHandle<()>>>,
    full_policy: FullPolicy,
    dropped: AtomicU64,
}

impl FlumeLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let file = File::create(&cfg.path)?;
        let writer = BufWriter::with_capacity(cfg.writer_buf_bytes.max(1), file);
        let (tx, rx) = if cfg.capacity == 0 {
            flume::unbounded()
        } else {
            flume::bounded(cfg.capacity)
        };
        let handle = std::thread::Builder::new()
            .name("logbench-flume".into())
            .spawn(move || run_worker(writer, rx.into_iter()))?;
        Ok(FlumeLogger {
            sender: tx,
            handle: Mutex::new(Some(handle)),
            full_policy: cfg.full_policy,
            dropped: AtomicU64::new(0),
        })
    }
}

impl Logger for FlumeLogger {
    fn log(&self, record: &[u8]) {
        let msg = Msg::Record(Box::from(record));
        match self.full_policy {
            FullPolicy::Block => {
                let _ = self.sender.send(msg);
            }
            FullPolicy::Drop => {
                if self.sender.try_send(msg).is_err() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn finish(&self) -> u64 {
        finish_via_shutdown(
            |m| {
                let _ = self.sender.send(m);
            },
            &self.handle,
            &self.dropped,
        )
    }
}
