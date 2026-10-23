use crate::{Repo, Result};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::path::Path;

/// A run of consecutive lines last changed by the same commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameHunk {
    /// 1-based, inclusive line range in the current file.
    pub lines: RangeInclusive<u32>,
    pub commit: String,
    pub author: String,
    pub summary: String,
    /// Commit author time, unix seconds.
    pub time: i64,
}

impl Repo {
    /// `git blame` of a committed file (HEAD version). Errors for untracked files.
    pub fn blame(&self, path: impl AsRef<Path>) -> Result<Vec<BlameHunk>> {
        let rel = self.rel(path.as_ref())?;
        let blame = self.repo.blame_file(&rel, None)?;
        let mut out = Vec::new();
        for h in blame.iter() {
            let start = h.final_start_line() as u32;
            let n = h.lines_in_hunk() as u32;
            if n == 0 {
                continue;
            }
            let id = h.final_commit_id();
            let (summary, time) = match self.repo.find_commit(id) {
                Ok(c) => (c.summary().unwrap_or_default().to_string(), c.author().when().seconds()),
                Err(_) => (String::new(), 0),
            };
            let author = h
                .final_signature()
                .name()
                .unwrap_or_default()
                .to_string();
            out.push(BlameHunk {
                lines: start..=start + n - 1,
                commit: id.to_string(),
                author,
                summary,
                time,
            });
        }
        Ok(out)
    }
}
