use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            pub fn new() -> Self { Self(uuid::Uuid::new_v4().to_string()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl Default for $name { fn default() -> Self { Self::new() } }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
    };
}

id!(TaskId);
id!(SessionId);
id!(RequestId);
id!(ToolCallId);

/// Stable identifier of an agent kind, e.g. "claude", "codex", "gemini", "opencode", "goose".
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);
impl AgentId {
    pub fn as_str(&self) -> &str { &self.0 }
}
impl From<&str> for AgentId { fn from(s: &str) -> Self { Self(s.to_string()) } }
impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
