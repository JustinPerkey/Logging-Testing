//! The tokio async file-writer strategy.
//!
//! The producer hands the record to a [`tokio::sync::mpsc`] channel; a single
//! writer task, running on a dedicated single-threaded tokio runtime, drains it
//! into a `tokio::io::BufWriter<tokio::fs::File>`. This is the shape an async
//! application would use to keep file I/O off its own threads.
//!
//! To limit the number of I/O operations the writer task pulls records off the
//! channel in batches (`recv_many`) and coalesces them in the `BufWriter`, so a
//! burst of small records becomes one large `write` rather than many tiny ones.
//! Note that `tokio::fs` performs the actual file writes on tokio's blocking
//! thread pool (there is no portable async file API), so this measures tokio's
//! channel + async-file machinery, not kernel-level async I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{mpsc, oneshot};

use super::Logger;
use crate::config::{FullPolicy, LoggerConfig};

/// Maximum records the writer task takes off the channel per wake-up.
const BATCH: usize = 256;

/// Message passed to the writer task.
enum Msg {
    /// A log record to write.
    Record(Box<[u8]>),
    /// Flush everything written so far, acknowledge, and stop the task.
    Shutdown(oneshot::Sender<()>),
}

/// The channel's sending half: bounded (capacity > 0) or unbounded (0).
enum Sender {
    Bounded(mpsc::Sender<Msg>),
    Unbounded(mpsc::UnboundedSender<Msg>),
}

/// Background-writer logger backed by a tokio runtime and `tokio::fs::File`.
///
/// The producers are plain OS threads (not tokio tasks), so the bounded
/// back-pressure path uses `blocking_send`, which parks the producer until the
/// writer task frees a slot.
pub struct TokioFsLogger {
    sender: Sender,
    handle: Mutex<Option<JoinHandle<()>>>,
    full_policy: FullPolicy,
    dropped: AtomicU64,
}

impl TokioFsLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let path = cfg.path.clone();
        let buf_bytes = cfg.writer_buf_bytes.max(1);
        let (sender, rx) = if cfg.capacity == 0 {
            let (tx, rx) = mpsc::unbounded_channel();
            (Sender::Unbounded(tx), Receiver::Unbounded(rx))
        } else {
            let (tx, rx) = mpsc::channel(cfg.capacity);
            (Sender::Bounded(tx), Receiver::Bounded(rx))
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .thread_name("logbench-tokio-fs-io")
            .build()?;
        // Open the file before spawning so a bad path surfaces as an error from
        // `new()` rather than a silently dead writer.
        let file = rt.block_on(tokio::fs::File::create(&path))?;
        let writer = BufWriter::with_capacity(buf_bytes, file);
        let handle = std::thread::Builder::new()
            .name("logbench-tokio-fs".into())
            .spawn(move || rt.block_on(run_writer(writer, rx)))?;

        Ok(TokioFsLogger {
            sender,
            handle: Mutex::new(Some(handle)),
            full_policy: cfg.full_policy,
            dropped: AtomicU64::new(0),
        })
    }

    /// Send a message, always applying back-pressure (used for shutdown).
    fn send_blocking(&self, msg: Msg) {
        match &self.sender {
            Sender::Bounded(tx) => {
                let _ = tx.blocking_send(msg);
            }
            Sender::Unbounded(tx) => {
                let _ = tx.send(msg);
            }
        }
    }
}

/// The channel's receiving half, matching [`Sender`].
enum Receiver {
    Bounded(mpsc::Receiver<Msg>),
    Unbounded(mpsc::UnboundedReceiver<Msg>),
}

impl Receiver {
    async fn recv_many(&mut self, buf: &mut Vec<Msg>) -> usize {
        match self {
            Receiver::Bounded(rx) => rx.recv_many(buf, BATCH).await,
            Receiver::Unbounded(rx) => rx.recv_many(buf, BATCH).await,
        }
    }
}

/// Drain `rx` into `writer` in batches. Returns when a [`Msg::Shutdown`] is
/// seen (after flushing) or the channel disconnects.
async fn run_writer(mut writer: BufWriter<tokio::fs::File>, mut rx: Receiver) {
    let mut batch = Vec::with_capacity(BATCH);
    loop {
        if rx.recv_many(&mut batch).await == 0 {
            break; // every sender dropped
        }
        for msg in batch.drain(..) {
            match msg {
                Msg::Record(bytes) => {
                    let _ = writer.write_all(&bytes).await;
                }
                Msg::Shutdown(ack) => {
                    // `flush` on a tokio `File` also waits for the in-flight
                    // background write to land, so the bytes are in the file.
                    let _ = writer.flush().await;
                    let _ = ack.send(());
                    return;
                }
            }
        }
    }
    let _ = writer.flush().await;
}

impl Logger for TokioFsLogger {
    fn log(&self, record: &[u8]) {
        let msg = Msg::Record(Box::from(record));
        match (&self.sender, self.full_policy) {
            (Sender::Bounded(tx), FullPolicy::Block) => {
                let _ = tx.blocking_send(msg);
            }
            (Sender::Bounded(tx), FullPolicy::Drop) => {
                if tx.try_send(msg).is_err() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            // An unbounded channel never fills, so the policy is moot.
            (Sender::Unbounded(tx), _) => {
                let _ = tx.send(msg);
            }
        }
    }

    fn finish(&self) -> u64 {
        // As with the channel strategies, the hot-path sender lives as long as
        // the logger, so an explicit shutdown message is what lets us join.
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send_blocking(Msg::Shutdown(ack_tx));
        let _ = ack_rx.blocking_recv();
        if let Some(h) = self.handle.lock().expect("handle mutex poisoned").take() {
            let _ = h.join();
        }
        self.dropped.load(Ordering::Relaxed)
    }
}
