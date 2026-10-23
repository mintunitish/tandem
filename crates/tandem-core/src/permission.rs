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
}
impl Default for Policy {
    fn default() -> Self {
        Self { auto_allow_safe: true, sandbox: true, trusted_commands: vec![], protected_paths: vec![], env_passthrough: vec![] }
    }
}
