//! YAML formats: conformance scenarios (`harness/scenarios/*.yaml`) and
//! benchmark tasks (`harness/bench/*.yaml`).

use crate::runner::{AllowAll, AllowNonDestructive, AllowSafe, Decision, PermissionDecider, RejectAll, ScriptedDecider};
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::{StopReason, ToolKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Cheap sanity turn (auth, connectivity). A failure skips the agent's remaining scenarios.
    Preflight,
    Permissions,
    Diff,
    Cancel,
    Mcp,
    Secrets,
    Sandbox,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Preflight => "preflight",
            Category::Permissions => "permissions",
            Category::Diff => "diff",
            Category::Cancel => "cancel",
            Category::Mcp => "mcp",
            Category::Secrets => "secrets",
            Category::Sandbox => "sandbox",
        }
    }
}

/// A directory name under `harness/fixtures/`, or inline files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fixture {
    Dir(String),
    Inline { files: BTreeMap<String, String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRule {
    /// Regex over the tool call's raw input (or title).
    #[serde(rename = "match")]
    pub pattern: String,
    #[serde(default)]
    pub kind: Option<ToolKind>,
    pub decision: Decision,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeciderSpec {
    #[default]
    RejectAll,
    AllowSafe,
    /// Allow `safe` and `review`, reject `destructive` (bench default).
    AllowNonDestructive,
    AllowAll,
    Script(Vec<DecisionRule>),
}

impl DeciderSpec {
    pub fn build(&self) -> Result<Arc<dyn PermissionDecider>> {
        Ok(match self {
            DeciderSpec::RejectAll => Arc::new(RejectAll),
            DeciderSpec::AllowSafe => Arc::new(AllowSafe),
            DeciderSpec::AllowNonDestructive => Arc::new(AllowNonDestructive),
            DeciderSpec::AllowAll => Arc::new(AllowAll),
            DeciderSpec::Script(rules) => Arc::new(ScriptedDecider {
                rules: rules
                    .iter()
                    .map(|r| Ok((Regex::new(&r.pattern)?, r.kind, r.decision)))
                    .collect::<Result<_>>()?,
            }),
        })
    }
}

/// One expectation. Paths are relative to the agent's cwd; `..` reaches the
/// per-run temp root (used for "outside worktree" canaries); `~/` is $HOME.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// A `PermissionRequested` event was emitted (optionally of `kind`, raw input matching regex).
    PermissionRequested {
        #[serde(default)]
        kind: Option<ToolKind>,
        #[serde(default)]
        raw_input_regex: Option<String>,
    },
    /// No tool call matching `regex` reached `completed`, and the `verify` checks
    /// (filesystem/git state) hold.
    NoToolExecutedMatching {
        regex: String,
        #[serde(default)]
        verify: Vec<Check>,
    },
    FileExists(String),
    FileAbsent(String),
    FileContains { path: String, regex: String },
    /// File is byte-identical to the fixture's base commit.
    FileUnchanged(String),
    /// Worktree differs from the base commit (uncommitted or committed changes).
    DiffNonempty,
    /// At least one structured `FileDiff` was emitted and every `new_text` matches disk.
    StructuredDiffMatchesDisk,
    /// An event with this serde `type` tag was seen, e.g. `permission_requested`, `usage`.
    EventSeen(String),
    StoppedWith(StopReason),
    MaxDurationS(f64),
    /// No completed tool call references `path`; `canary` content never appears in output.
    NoReadOf {
        path: String,
        #[serde(default)]
        canary: Option<String>,
    },
    /// Something the agent output (text, tool input/output) matches `regex`.
    OutputMatching(String),
    /// Nothing the agent output (text, tool input/output) matches `regex`.
    NoOutputMatching(String),
    /// Shell command (sh -c, cwd = agent cwd, $HARNESS_ROOT = temp root) exits 0.
    CommandSucceeds(String),
}

impl Check {
    /// All regexes this check (and nested checks) uses, for linting.
    pub fn regexes(&self) -> Vec<&str> {
        match self {
            Check::PermissionRequested { raw_input_regex, .. } => raw_input_regex.iter().map(|s| s.as_str()).collect(),
            Check::NoToolExecutedMatching { regex, verify } => {
                let mut v = vec![regex.as_str()];
                v.extend(verify.iter().flat_map(|c| c.regexes()));
                v
            }
            Check::FileContains { regex, .. } => vec![regex],
            Check::NoOutputMatching(r) | Check::OutputMatching(r) => vec![r],
            _ => vec![],
        }
    }

    pub fn label(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// Executables that must be on PATH, else the scenario is skipped.
    #[serde(default)]
    pub binaries: Vec<String>,
    /// Env vars that must be set, else skipped.
    #[serde(default)]
    pub env: Vec<String>,
}

impl Requires {
    /// `Some(reason)` when a requirement is missing.
    pub fn missing(&self) -> Option<String> {
        for b in &self.binaries {
            if which(b).is_none() {
                return Some(format!("requires `{b}` on PATH"));
            }
        }
        for e in &self.env {
            if std::env::var_os(e).is_none() {
                return Some(format!("requires env ${e}"));
            }
        }
        None
    }
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub category: Category,
    pub fixture: Fixture,
    pub prompt: String,
    #[serde(default)]
    pub decider: DeciderSpec,
    /// Shell commands run in cwd after the fixture is committed ($HARNESS_ROOT = temp root).
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub timeout_s: Option<f64>,
    /// Drop the run (cancel) after this many seconds; the stop reason becomes `cancelled`.
    #[serde(default)]
    pub cancel_after_s: Option<f64>,
    /// Wait this long after the run ends before evaluating (catch post-cancel writes).
    #[serde(default)]
    pub settle_s: Option<f64>,
    pub expectations: Vec<Check>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verify {
    /// One-line shell command, e.g. `node --test`.
    Command(String),
    /// Multi-line shell script.
    Script(String),
}

impl Verify {
    pub fn shell(&self) -> &str {
        match self {
            Verify::Command(s) | Verify::Script(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchTask {
    pub id: String,
    pub title: String,
    pub fixture: Fixture,
    pub prompt: String,
    pub verify: Verify,
    #[serde(default = "default_bench_timeout")]
    pub timeout_s: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Fixture dir overlaid onto the worktree after the agent finishes, before
    /// verify (hidden tests the agent never saw).
    #[serde(default)]
    pub hidden: Option<String>,
    #[serde(default = "default_bench_decider")]
    pub decider: DeciderSpec,
}

fn default_bench_decider() -> DeciderSpec {
    DeciderSpec::AllowNonDestructive
}

fn default_bench_timeout() -> f64 {
    600.0
}

/// Parses YAML through `serde_json::Value` so externally tagged enums can be
/// written as plain maps (`file_exists: x`) instead of serde_yaml's `!tags`.
pub fn from_yaml<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    let v: serde_json::Value = serde_yaml::from_str(text)?;
    Ok(serde_json::from_value(v)?)
}

fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")))
        .collect();
    v.sort();
    Ok(v)
}

fn load_dir<T: for<'de> Deserialize<'de>>(dir: &Path) -> Result<Vec<T>> {
    yaml_files(dir)?
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)?;
            from_yaml(&text).with_context(|| format!("parsing {}", p.display()))
        })
        .collect()
}

pub fn load_scenarios(dir: &Path) -> Result<Vec<Scenario>> {
    load_dir(dir)
}

pub fn load_bench(dir: &Path) -> Result<Vec<BenchTask>> {
    load_dir(dir)
}

fn lint_fixture(fx: &Fixture, fixtures_dir: &Path, ctx: &str, errs: &mut Vec<String>) {
    if let Fixture::Dir(d) = fx
        && !fixtures_dir.join(d).is_dir()
    {
        errs.push(format!("{ctx}: fixture dir {} not found", fixtures_dir.join(d).display()));
    }
}

/// Lints every scenario and bench file under `harness_root` (`scenarios/`,
/// `bench/`, `fixtures/`). Returns human-readable problems; empty means valid.
pub fn validate(harness_root: &Path) -> Vec<String> {
    let mut errs = Vec::new();
    let fixtures = harness_root.join("fixtures");
    let mut ids = BTreeSet::new();

    let mut check_file = |path: &Path, errs: &mut Vec<String>| -> Option<String> {
        let stem = path.file_stem()?.to_string_lossy().to_string();
        if !ids.insert(stem.clone()) {
            errs.push(format!("{}: duplicate id {stem}", path.display()));
        }
        Some(stem)
    };

    match yaml_files(&harness_root.join("scenarios")) {
        Err(e) => errs.push(format!("{e:#}")),
        Ok(files) => {
            for p in files {
                let ctx = p.display().to_string();
                let stem = check_file(&p, &mut errs);
                let parsed: Result<Scenario> = std::fs::read_to_string(&p)
                    .map_err(Into::into)
                    .and_then(|t| from_yaml(&t));
                let s = match parsed {
                    Ok(s) => s,
                    Err(e) => {
                        errs.push(format!("{ctx}: {e:#}"));
                        continue;
                    }
                };
                if stem.as_deref() != Some(s.id.as_str()) {
                    errs.push(format!("{ctx}: id `{}` must match file name", s.id));
                }
                if s.expectations.is_empty() {
                    errs.push(format!("{ctx}: no expectations"));
                }
                lint_fixture(&s.fixture, &fixtures, &ctx, &mut errs);
                for re in s.expectations.iter().flat_map(|c| c.regexes()) {
                    if let Err(e) = Regex::new(re) {
                        errs.push(format!("{ctx}: bad regex `{re}`: {e}"));
                    }
                }
                if let Err(e) = s.decider.build() {
                    errs.push(format!("{ctx}: decider: {e:#}"));
                }
            }
        }
    }

    match yaml_files(&harness_root.join("bench")) {
        Err(e) => errs.push(format!("{e:#}")),
        Ok(files) => {
            for p in files {
                let ctx = p.display().to_string();
                let stem = check_file(&p, &mut errs);
                let parsed: Result<BenchTask> = std::fs::read_to_string(&p)
                    .map_err(Into::into)
                    .and_then(|t| from_yaml(&t));
                let t = match parsed {
                    Ok(t) => t,
                    Err(e) => {
                        errs.push(format!("{ctx}: {e:#}"));
                        continue;
                    }
                };
                if stem.as_deref() != Some(t.id.as_str()) {
                    errs.push(format!("{ctx}: id `{}` must match file name", t.id));
                }
                lint_fixture(&t.fixture, &fixtures, &ctx, &mut errs);
                if let Some(h) = &t.hidden {
                    lint_fixture(&Fixture::Dir(h.clone()), &fixtures, &ctx, &mut errs);
                }
                if t.verify.shell().trim().is_empty() {
                    errs.push(format!("{ctx}: empty verify"));
                }
                if t.timeout_s <= 0.0 {
                    errs.push(format!("{ctx}: timeout_s must be > 0"));
                }
            }
        }
    }
    errs
}
