//! "Expose to agents": which saved database connections a task's agent gets as a
//! `tandem-db-mcp` MCP server. Stored per project in `.tandem/db-agents.json`.
//!
//! Default is read-only (`tandem-db-mcp --connection X`); `--allow-write` only when the user
//! toggled it in the Database panel (a Review-class action with its own confirmation). The tool
//! definitions are hash-pinned in `.tandem/mcp-pins.json` at first exposure; a changed server is
//! not attached until re-approved.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tandem_acp::McpServerSpec;
use tandem_sandbox::{PinStatus, PinStore};

pub const MCP_BIN: &str = "tandem-db-mcp";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    #[serde(default)]
    pub allow_write: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbAgents {
    #[serde(default)]
    pub connections: BTreeMap<String, Exposure>,
}

fn path(root: &Path) -> PathBuf {
    root.join(".tandem").join("db-agents.json")
}

pub fn server_name(conn: &str) -> String {
    format!("tandem-db-{conn}")
}

impl DbAgents {
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(path(root)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let p = path(root);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Toggle exposure; pins the server's tool definitions the first time.
    pub fn set_exposed(&mut self, root: &Path, conn: &str, on: bool) {
        if on {
            self.connections.entry(conn.to_string()).or_default();
            if let Ok(mut pins) = PinStore::open(root)
                && pins.get(&server_name(conn)).is_none()
            {
                let _ = pins.pin(&server_name(conn), &tandem_db::tool_definitions_json());
            }
        } else {
            self.connections.remove(conn);
        }
    }
}

/// `tandem-db-mcp` next to the running binary (cargo / app bundle), else on PATH.
pub fn mcp_binary() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|e| {
        let dir = e.parent()?.to_path_buf();
        // Test binaries live in target/<profile>/deps.
        [dir.join(MCP_BIN), dir.parent()?.join(MCP_BIN)].into_iter().find(|p| p.is_file())
    });
    beside.or_else(|| tandem_acp::registry::which_in(MCP_BIN, std::env::var_os("PATH").as_deref().unwrap_or_default()))
}

/// MCP servers for a new task plus human notes for the transcript.
pub fn mcp_specs(root: &Path) -> (Vec<McpServerSpec>, Vec<String>) {
    let cfg = DbAgents::load(root);
    if cfg.connections.is_empty() {
        return (vec![], vec![]);
    }
    let Some(bin) = mcp_binary() else {
        return (vec![], vec![format!("database not attached: `{MCP_BIN}` not found (build it with `cargo build -p tandem-db`)")]);
    };
    let pins = PinStore::open(root).ok();
    let tools = tandem_db::tool_definitions_json();
    let (mut specs, mut notes) = (vec![], vec![]);
    for (name, exp) in cfg.connections {
        let server = server_name(&name);
        if let Some(p) = &pins
            && matches!(p.check(&server, &tools), PinStatus::Changed { .. })
        {
            notes.push(format!("database `{name}` not attached: its MCP tool definitions changed since you approved them"));
            continue;
        }
        let mut args = vec!["--connection".to_string(), name.clone()];
        if exp.allow_write {
            args.push("--allow-write".into());
        }
        let mut env = BTreeMap::new();
        if let Some(store) = std::env::var_os("TANDEM_DB_STORE") {
            env.insert("TANDEM_DB_STORE".to_string(), store.to_string_lossy().into_owned());
        }
        notes.push(format!("database `{name}` attached via MCP ({})", if exp.allow_write { "read + write" } else { "read-only" }));
        specs.push(McpServerSpec { name: server, command: bin.clone(), args, env });
    }
    (specs, notes)
}
