//! "Expose to agents": which saved database connections a task's agent gets as a
//! `tandem-db-mcp` MCP server. Stored per project in `.tandem/db-agents.json`.
//!
//! Default is read-only (`tandem-db-mcp --connection X`); `--allow-write` only when the user
//! toggled it in the Database panel (a Review-class action with its own confirmation). With
//! `allow_dangerous`, destructive statements are possible too, each only once its `sql_hash` is
//! listed in `.tandem/db-confirm.json`, which the panel's red confirm dialog appends to
//! (`--allow-dangerous --confirm-file`). The tool definitions are hash-pinned in
//! `.tandem/mcp-pins.json` at first exposure; a changed server is not attached until re-approved.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tandem_core::McpServerSpec;
use tandem_db::MCP_BIN;
use tandem_sandbox::{PinStatus, PinStore};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    #[serde(default)]
    pub allow_write: bool,
    /// Destructive SQL, per statement approved in the IDE (see [`confirm_file`]).
    #[serde(default)]
    pub allow_dangerous: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbAgents {
    #[serde(default)]
    pub connections: BTreeMap<String, Exposure>,
}

fn path(root: &Path) -> PathBuf {
    root.join(".tandem").join("db-agents.json")
}

pub use tandem_db::server_name;

/// `.tandem/db-confirm.json`: JSON array of approved statement hashes (`tandem_db::sql_hash`).
/// Agents cannot write it: `.tandem/` is read-only in their sandbox.
pub fn confirm_file(root: &Path) -> PathBuf {
    root.join(".tandem").join("db-confirm.json")
}

/// Record the user's approval of one destructive statement for agents' MCP servers.
pub fn approve_statement(root: &Path, sql: &str) -> std::io::Result<()> {
    let path = confirm_file(root);
    let mut set: std::collections::BTreeSet<String> =
        std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if set.insert(tandem_db::sql_hash(sql)) {
        tandem_core::fs::atomic_write(&path, serde_json::to_string_pretty(&set).unwrap_or_default().as_bytes())?;
    }
    Ok(())
}

/// Policy for statements the user runs in the Database panel: everything allowed, dangerous
/// statements only after the panel's red-card dialog. `confirmed` is that dialog's answer
/// for this statement; the closure is the panel's interactive confirmer.
pub fn panel_policy(confirmed: bool) -> tandem_db::McpPolicy {
    tandem_db::McpPolicy::new(true, true, true, Box::new(move |_: &str, _: &str| confirmed))
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

/// MCP servers for a new task plus human notes for the transcript.
pub fn mcp_specs(root: &Path) -> (Vec<McpServerSpec>, Vec<String>) {
    let cfg = DbAgents::load(root);
    if cfg.connections.is_empty() {
        return (vec![], vec![]);
    }
    if tandem_db::mcp_binary().is_none() {
        return (vec![], vec![format!("database not attached: `{MCP_BIN}` not found (build it with `cargo build -p tandem-db`)")]);
    }
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
        notes.push(format!("database `{name}` attached via MCP ({})", if exp.allow_write { "read + write" } else { "read-only" }));
        let mut spec = tandem_db::server_spec(&name, exp.allow_write, false);
        if exp.allow_dangerous {
            spec.args.extend(["--allow-dangerous".to_string(), "--confirm-file".to_string(), confirm_file(root).to_string_lossy().into_owned()]);
        }
        specs.push(spec);
    }
    (specs, notes)
}
