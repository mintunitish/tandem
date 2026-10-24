//! tandem-review: pure review logic over [`DiffSet`]s (no git, no UI).
//!
//! * [`state`]: per-hunk accept/reject, navigation, materialization of accepted hunks.
//! * [`symbols`]: tree-sitter symbol lookup ("what did this hunk touch").
//! * [`conflicts`]: intent-conflict detection across concurrent agent tasks.
//! * [`blame`]: agent blame entries with line-shift bookkeeping.
//! * [`summary`]: counts, touched symbols and risk flags for a DiffSet.
//!
//! See `docs/specs/2026-10-24-tandem-design.md` ("Review", "Agent Blame").

pub mod blame;
pub mod conflicts;
pub mod hunks;
pub mod state;
pub mod summary;
pub mod symbols;

pub use blame::{BlameIndex, blame_entries, merge_blame};
pub use conflicts::{ConflictKind, ConflictSide, IntentConflict, Severity, detect};
pub use hunks::diff_texts;
pub use state::{Decision, HunkRef, ReviewCounts, ReviewState};
pub use summary::{ReviewSummary, describe};
pub use symbols::{Lang, LineRange, SymbolKind, SymbolRef, enclosing_symbols};

pub use tandem_core::TaskMeta;
