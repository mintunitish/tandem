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
    /// The option to pick for `kind`: that exact kind, else a narrower sibling with the
    /// same polarity. Never crosses polarity (a reject never resolves to an allow) and
    /// never widens a grant: `AllowOnce` has no fallback, `AllowAlways` falls back to
    /// `AllowOnce`. Rejections fall back either way (both refuse).
    pub fn option_for(&self, kind: PermissionOptionKind) -> Option<&PermissionOption> {
        use PermissionOptionKind::*;
        let fallback = match kind {
            AllowOnce => None,
            AllowAlways => Some(AllowOnce),
            RejectOnce => Some(RejectAlways),
            RejectAlways => Some(RejectOnce),
        };
        std::iter::once(kind).chain(fallback).find_map(|k| self.options.iter().find(|o| o.kind == k))
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

/// Security policy. The user's copy lives in `~/.tandem/policy.json`; a project's
/// `.tandem/policy.json` can only tighten it (see [`Policy::merge_project`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// A project's `.tandem/policy.json`, as written by the project (every field optional).
/// Only the tightening parts are applied; see [`Policy::merge_project`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPolicy {
    pub auto_allow_safe: Option<bool>,
    pub sandbox: Option<bool>,
    pub trusted_commands: Vec<String>,
    pub protected_paths: Vec<String>,
    pub env_passthrough: Vec<String>,
    pub fetch_allow_hosts: Vec<String>,
    pub strict_permissions: Option<bool>,
}

impl ProjectPolicy {
    /// Fields the project tried to set that would loosen `user` (ignored by
    /// [`Policy::merge_project`]), as human-readable names.
    pub fn loosening(&self, user: &Policy) -> Vec<&'static str> {
        let mut v = vec![];
        if self.sandbox == Some(false) && user.sandbox {
            v.push("sandbox");
        }
        if self.strict_permissions == Some(false) && user.strict_permissions {
            v.push("strict_permissions");
        }
        if self.auto_allow_safe == Some(true) && !user.auto_allow_safe {
            v.push("auto_allow_safe");
        }
        if !self.trusted_commands.is_empty() {
            v.push("trusted_commands");
        }
        if !self.env_passthrough.is_empty() {
            v.push("env_passthrough");
        }
        if !self.fetch_allow_hosts.is_empty() {
            v.push("fetch_allow_hosts");
        }
        v
    }
}

impl Policy {
    /// `user` tightened by a project file: the project may add `protected_paths` and turn
    /// `sandbox` / `strict_permissions` on or `auto_allow_safe` off. Anything that would
    /// loosen the user policy (trusted commands, env passthrough, fetch hosts, disabling the
    /// sandbox...) is ignored; [`ProjectPolicy::loosening`] lists what was dropped.
    pub fn merge_project(user: &Policy, project: &ProjectPolicy) -> Policy {
        let mut p = user.clone();
        for path in &project.protected_paths {
            if !p.protected_paths.contains(path) {
                p.protected_paths.push(path.clone());
            }
        }
        if project.sandbox == Some(true) {
            p.sandbox = true;
        }
        if project.strict_permissions == Some(true) {
            p.strict_permissions = true;
        }
        if project.auto_allow_safe == Some(false) {
            p.auto_allow_safe = false;
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionOptionKind::*;

    fn req(kinds: &[PermissionOptionKind]) -> PermissionRequest {
        let tool_call = ToolCall {
            id: "t".into(),
            title: String::new(),
            kind: crate::ToolKind::Execute,
            status: crate::ToolStatus::Pending,
            locations: vec![],
            raw_input: None,
            content: None,
            diff: None,
        };
        PermissionRequest {
            id: RequestId::from("r"),
            tool_call,
            options: kinds.iter().enumerate().map(|(i, k)| PermissionOption { id: format!("o{i}"), label: format!("{k:?}"), kind: *k }).collect(),
            risk: RiskClass::Review,
            reason: String::new(),
        }
    }

    #[test]
    fn option_for_never_crosses_polarity_or_widens() {
        let kind = |r: &PermissionRequest, k| r.option_for(k).map(|o| o.kind);
        let all = req(&[AllowOnce, AllowAlways, RejectOnce, RejectAlways]);
        for k in [AllowOnce, AllowAlways, RejectOnce, RejectAlways] {
            assert_eq!(kind(&all, k), Some(k));
        }
        // Always -> Once is fine; Once -> Always never.
        assert_eq!(kind(&req(&[AllowOnce, RejectOnce]), AllowAlways), Some(AllowOnce));
        assert_eq!(kind(&req(&[AllowAlways, RejectOnce]), AllowOnce), None);
        // Rejects fall back to the other reject, never to an allow.
        assert_eq!(kind(&req(&[AllowOnce, RejectAlways]), RejectOnce), Some(RejectAlways));
        assert_eq!(kind(&req(&[AllowOnce, AllowAlways]), RejectOnce), None);
        assert_eq!(kind(&req(&[AllowOnce, AllowAlways]), RejectAlways), None);
        // Allows never resolve to a reject.
        assert_eq!(kind(&req(&[RejectOnce, RejectAlways]), AllowOnce), None);
        assert_eq!(kind(&req(&[RejectOnce]), AllowAlways), None);
    }

    #[test]
    fn project_policy_only_tightens() {
        let user = Policy::default();
        let loose: ProjectPolicy = serde_json::from_str(
            r#"{"sandbox":false,"strict_permissions":false,"trusted_commands":["rm"],"env_passthrough":["AWS_*"],"fetch_allow_hosts":["evil.com"],"protected_paths":["secrets/"]}"#,
        )
        .unwrap();
        let merged = Policy::merge_project(&user, &loose);
        assert!(merged.sandbox && merged.strict_permissions);
        assert!(merged.trusted_commands.is_empty() && merged.env_passthrough.is_empty() && merged.fetch_allow_hosts.is_empty());
        assert_eq!(merged.protected_paths, ["secrets/"]);
        assert_eq!(loose.loosening(&user), ["sandbox", "strict_permissions", "trusted_commands", "env_passthrough", "fetch_allow_hosts"]);

        let user = Policy { sandbox: false, strict_permissions: false, protected_paths: vec!["a".into()], ..Policy::default() };
        let tight = ProjectPolicy { sandbox: Some(true), strict_permissions: Some(true), auto_allow_safe: Some(false), protected_paths: vec!["a".into(), "b".into()], ..Default::default() };
        let merged = Policy::merge_project(&user, &tight);
        assert!(merged.sandbox && merged.strict_permissions && !merged.auto_allow_safe);
        assert_eq!(merged.protected_paths, ["a", "b"]);
        assert!(tight.loosening(&user).is_empty());

        let user = Policy { auto_allow_safe: false, ..Policy::default() };
        let p = ProjectPolicy { auto_allow_safe: Some(true), ..Default::default() };
        assert!(!Policy::merge_project(&user, &p).auto_allow_safe);
        assert_eq!(p.loosening(&user), ["auto_allow_safe"]);
        assert_eq!(Policy::merge_project(&Policy::default(), &ProjectPolicy::default()), Policy::default());
    }
}
