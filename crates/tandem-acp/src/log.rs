//! Flight-recorder log: one JSON [`LogRecord`] per line in `<dir>/log.jsonl`.

use anyhow::{Context as _, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use tandem_core::{AgentEvent, LogRecord, TaskId};

pub const LOG_FILE: &str = "log.jsonl";

pub struct ReplayLog {
    file: File,
    path: PathBuf,
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
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, record: &LogRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
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
}
