//! Synchronous baseline: write on the calling thread under a mutex.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;

use super::Logger;
use crate::config::LoggerConfig;

/// The "just write it now" strategy.
///
/// Records are written directly on the producing thread, serialised through a
/// `Mutex<BufWriter<File>>`. This is the honest baseline every async strategy
/// is trying to beat: it has zero hand-off machinery, but the `log()` call
/// blocks on the lock and pays for any buffer flush inline.
pub struct DirectLogger {
    writer: Mutex<BufWriter<File>>,
}

impl DirectLogger {
    pub fn new(cfg: &LoggerConfig) -> std::io::Result<Self> {
        let file = File::create(&cfg.path)?;
        let writer = BufWriter::with_capacity(cfg.writer_buf_bytes.max(1), file);
        Ok(DirectLogger {
            writer: Mutex::new(writer),
        })
    }
}

impl Logger for DirectLogger {
    fn log(&self, record: &[u8]) {
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        // Ignore write errors in the benchmark hot path; a real logger would
        // surface them, but doing so here would distort the measurement.
        let _ = w.write_all(record);
    }

    fn finish(&self) -> u64 {
        let mut w = self.writer.lock().expect("writer mutex poisoned");
        let _ = w.flush();
        0
    }
}
