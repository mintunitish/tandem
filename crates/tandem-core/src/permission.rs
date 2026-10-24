use crate::{RequestId, ToolCall};
use serde::{Deserialize, Serialize};

/// Risk class assigned by `tandem-sandbox` before the UI sees a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Reads inside the project, writes inside the task worktree. Auto-allowed by default policy.
    Safe,
    /// Unknown shell commands, writes outside the worktree, network fetches. Ask.
    Review,
    /// rm -rf, force push, DROP/TRUNCATE, secret paths, curl|sh. Red card; never auto.
    Destructive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionOption { pub id: String, pub label: String, pub kind: PermissionOptionKind }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: RequestId,
    pub tool_call: ToolCall,
    pub options: Vec<PermissionOption>,
    pub risk: RiskClass,
    /// Human-readable reason from the classifier, e.g. "recursive delete outside worktree".
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionDecision { pub request: RequestId, pub option_id: String }

impl PermissionOptionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

impl PermissionRequest {
    /// The option to pick for `kind`: that exact kind, else its sibling with the same
    /// polarity ("once" ⇄ "always").
    pub fn option_for(&self, kind: PermissionOptionKind) -> Option<&PermissionOption> {
        use PermissionOptionKind::*;
        let sibling = match kind {
            AllowOnce => AllowAlways,
            AllowAlways => AllowOnce,
            RejectOnce => RejectAlways,
            RejectAlways => RejectOnce,
        };
        [kind, sibling].into_iter().find_map(|k| self.options.iter().find(|o| o.kind == k))
    }
}

/// Assigns a risk class to a permission request before the UI sees it.
/// Implemented by `tandem_sandbox::Classifier`; the default marks everything `Review`.
pub trait PermissionClassifier: Send + Sync {
    fn classify(&self, _call: &ToolCall) -> (RiskClass, String) {
        (RiskClass::Review, String::new())
    }
}

impl std::fmt::Display for RiskClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { RiskClass::Safe => "safe", RiskClass::Review => "review", RiskClass::Destructive => "destructive" })
    }
}

/// Project-level policy. Serialized into `.tandem/policy.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    pub auto_allow_safe: bool,
    pub sandbox: bool,
    /// Extra shell command prefixes the user trusts, e.g. ["npm test", "cargo check"].
    pub trusted_commands: Vec<String>,
    /// Extra paths agents may never read, in addition to built-ins.
    pub protected_paths: Vec<String>,
    /// Env var names passed through to agents beyond the default allow-list.
    pub env_passthrough: Vec<String>,
    /// Hosts agents may fetch without asking (docs sites etc.).
    #[serde(default)]
    pub fetch_allow_hosts: Vec<String>,
    /// Launch agents in their "ask before acting" mode (see `AgentDescriptor::strict_args`).
    #[serde(default = "default_true")]
    pub strict_permissions: bool,
}
fn default_true() -> bool {
    true
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_allow_safe: true,
            sandbox: true,
            trusted_commands: vec![],
            protected_paths: vec![],
            env_passthrough: vec![],
            fetch_allow_hosts: vec![],
            strict_permissions: true,
        }
    }
}
