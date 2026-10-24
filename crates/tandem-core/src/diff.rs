use crate::{AgentId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind { Context, Added, Removed }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffLine { pub kind: LineKind, pub old_no: Option<u32>, pub new_no: Option<u32>, pub text: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32, pub old_lines: u32,
    pub new_start: u32, pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange { Added, Modified, Deleted, Renamed }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub change: FileChange,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
}

/// All changes a task produced: worktree vs its base commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffSet { pub task: TaskId, pub agent: AgentId, pub base_commit: String, pub files: Vec<FileDiff> }

/// Agent blame: who produced a line and from which prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlameEntry {
    pub path: PathBuf,
    pub new_start: u32, pub new_lines: u32,
    pub task: TaskId,
    pub agent: AgentId,
    pub prompt_excerpt: String,
    pub permission_by: Option<crate::Decider>,
}

/// Line-based hunks between two texts using `similar`, with `context` lines around each change.
/// With `strip_eol` each line's `\n`/`\r\n` terminator is removed; otherwise kept verbatim
/// (a last line without one marks "no newline at end of file").
pub fn hunks_from_texts(old: &str, new: &str, context: usize, strip_eol: bool) -> Vec<Hunk> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let start = |s: usize, n: usize| if n == 0 { s as u32 } else { s as u32 + 1 };
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(context) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (o0, o1) = (first.old_range().start, last.old_range().end);
        let (n0, n1) = (first.new_range().start, last.new_range().end);
        let mut lines = Vec::new();
        for op in &group {
            for ch in diff.iter_changes(op) {
                let kind = match ch.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Delete => LineKind::Removed,
                    ChangeTag::Insert => LineKind::Added,
                };
                let v = ch.value();
                let text = if strip_eol {
                    let s = v.strip_suffix('\n').unwrap_or(v);
                    s.strip_suffix('\r').unwrap_or(s)
                } else {
                    v
                };
                lines.push(DiffLine {
                    kind,
                    old_no: ch.old_index().map(|i| i as u32 + 1),
                    new_no: ch.new_index().map(|i| i as u32 + 1),
                    text: text.to_string(),
                });
            }
        }
        hunks.push(Hunk {
            old_start: start(o0, o1 - o0),
            old_lines: (o1 - o0) as u32,
            new_start: start(n0, n1 - n0),
            new_lines: (n1 - n0) as u32,
            lines,
        });
    }
    hunks
}

impl Hunk {
    /// Unified-diff header, e.g. `@@ -1,3 +1,4 @@`.
    pub fn header(&self) -> String {
        format!("@@ -{},{} +{},{} @@", self.old_start, self.old_lines, self.new_start, self.new_lines)
    }
}

impl FileDiff {
    /// Git-style unified patch. Handles both line conventions: hunks whose lines keep their
    /// terminator (tandem-git) and hunks with stripped lines (tandem-review).
    pub fn to_patch(&self) -> String {
        use std::fmt::Write as _;
        let path = self.path.to_string_lossy();
        let old = self.old_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
        let mut s = String::new();
        let _ = writeln!(s, "diff --git a/{old} b/{path}");
        match self.change {
            FileChange::Added => {
                let _ = writeln!(s, "new file mode 100644\n--- /dev/null\n+++ b/{path}");
            }
            FileChange::Deleted => {
                let _ = writeln!(s, "deleted file mode 100644\n--- a/{old}\n+++ /dev/null");
            }
            _ => {
                let _ = writeln!(s, "--- a/{old}\n+++ b/{path}");
            }
        }
        let keeps_eol = self.hunks.iter().flat_map(|h| &h.lines).any(|l| l.text.ends_with('\n'));
        for h in &self.hunks {
            let _ = writeln!(s, "{}", h.header());
            for l in &h.lines {
                let sign = match l.kind {
                    LineKind::Context => ' ',
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                };
                match l.text.strip_suffix('\n') {
                    Some(t) => {
                        let _ = writeln!(s, "{sign}{}", t.strip_suffix('\r').unwrap_or(t));
                    }
                    None if !keeps_eol => {
                        let _ = writeln!(s, "{sign}{}", l.text);
                    }
                    None => {
                        let _ = writeln!(s, "{sign}{}\n\\ No newline at end of file", l.text);
                    }
                }
            }
        }
        s
    }
}
