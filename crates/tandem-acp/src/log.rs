//! Flight-recorder log: one JSON [`LogRecord`] per line in `<dir>/log.jsonl`.
//!
//! Writes are buffered and flushed on `Done` / `Error` / `PermissionRequested`, at least every
//! [`FLUSH_INTERVAL`], and on drop, so a crash loses at most that much of a running
//! turn (a torn trailing line is skipped by [`ReplayLog::read`]).

use anyhow::{Context as _, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tandem_core::{AgentEvent, LogRecord, TaskId};

pub const LOG_FILE: &str = "log.jsonl";
/// Longest time a record may sit in the buffer.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

pub struct ReplayLog {
    file: BufWriter<File>,
    path: PathBuf,
    last_flush: Instant,
}

impl ReplayLog {
    /// Open (creating `dir` if needed) for appending.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join(LOG_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        Ok(Self { file: BufWriter::new(file), path, last_flush: Instant::now() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, record: &LogRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        // The turn ended or now waits for a human: nothing may be lost from here on.
        let pause = matches!(record.event, AgentEvent::Done { .. } | AgentEvent::Error { .. } | AgentEvent::PermissionRequested(_));
        if pause || self.last_flush.elapsed() >= FLUSH_INTERVAL {
            self.flush()?;
        }
        Ok(())
    }

    /// Write buffered records to the file.
    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        self.last_flush = Instant::now();
        Ok(())
    }

    /// Append `event` stamped with the current time.
    pub fn record(&mut self, task: &TaskId, event: &AgentEvent) -> Result<()> {
        self.append(&LogRecord {
            at: chrono::Utc::now(),
            task: task.clone(),
            event: event.clone(),
        })
    }

    /// Read all records. A torn trailing line (crash mid-write) is skipped with a warning.
    pub fn read(dir: impl AsRef<Path>) -> Result<Vec<LogRecord>> {
        let path = dir.as_ref().join(LOG_FILE);
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let mut out = Vec::new();
        for (n, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(r) => out.push(r),
                Err(e) => {
                    tracing::warn!(path = %path.display(), line = n + 1, "skipping bad log line: {e}")
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let task = TaskId::from("t1");
        {
            let mut log = ReplayLog::open(dir.path().join("tasks/t1")).unwrap();
            log.record(
                &task,
                &AgentEvent::TextDelta {
                    role: tandem_core::Role::Assistant,
                    text: "hi".into(),
                },
            )
            .unwrap();
        }
        let mut log = ReplayLog::open(dir.path().join("tasks/t1")).unwrap();
        log.record(
            &task,
            &AgentEvent::Done {
                reason: tandem_core::StopReason::EndTurn,
            },
        )
        .unwrap();
        let records = ReplayLog::read(dir.path().join("tasks/t1")).unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(records[1].event, AgentEvent::Done { .. }));
    }

    #[test]
    fn buffers_deltas_and_flushes_on_done() {
        let dir = tempfile::tempdir().unwrap();
        let task = TaskId::from("t1");
        let mut log = ReplayLog::open(dir.path()).unwrap();
        let delta = AgentEvent::TextDelta { role: tandem_core::Role::Assistant, text: "hi".into() };
        log.record(&task, &delta).unwrap();
        // Still in the buffer (well within FLUSH_INTERVAL of opening).
        assert_eq!(ReplayLog::read(dir.path()).unwrap().len(), 0);
        log.record(&task, &AgentEvent::Done { reason: tandem_core::StopReason::EndTurn }).unwrap();
        assert_eq!(ReplayLog::read(dir.path()).unwrap().len(), 2, "Done flushes everything");
        log.record(&task, &delta).unwrap();
        assert_eq!(ReplayLog::read(dir.path()).unwrap().len(), 2);
        log.flush().unwrap();
        assert_eq!(ReplayLog::read(dir.path()).unwrap().len(), 3);
        // A torn trailing line is skipped.
        use std::io::Write as _;
        std::fs::OpenOptions::new().append(true).open(log.path()).unwrap().write_all(b"{\"at\":").unwrap();
        assert_eq!(ReplayLog::read(dir.path()).unwrap().len(), 3);
    }
}
