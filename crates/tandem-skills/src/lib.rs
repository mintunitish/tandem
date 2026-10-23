//! tandem-skills: curated agent skills in the open `SKILL.md` format, embedded in the binary
//! and materialized into a task worktree so every agent sees the same instructions.
//!
//! See `docs/specs/2026-10-24-tandem-design.md` ("Built-in skill packs") and `skills/LICENSES.md`.
//!
//! ```no_run
//! use tandem_skills::{materialize, AgentTarget, Preset};
//! let wt = std::path::Path::new("/tmp/worktree");
//! materialize(wt, &Preset::Lazy.skills(), &[AgentTarget::ClaudeCode, AgentTarget::Agents]).unwrap();
//! ```

mod catalog;
mod materialize;
mod preset;
mod target;

pub use catalog::{Pack, Skill, SkillId, catalog, find_skill, skill_files};
pub use materialize::{
    BLOCK_END, BLOCK_START, MaterializeOptions, MaterializeReport, clean, managed_paths, materialize,
    materialize_with, render_index_block,
};
pub use preset::Preset;
pub use target::AgentTarget;

#[cfg(test)]
mod tests;
