//! "Expose to agents": which saved database connections a task's agent gets as a
//! `tandem-db-mcp` MCP server.
//!
//! Two sources:
//! - the user's store `~/.tandem/db/agents-<project-slug>.json` (written by the Database
//!   panel), the only place `allow_write` / `allow_dangerous` are honoured;
//! - the project's `.tandem/db-agents.json`, which can only list connections to expose
//!   read-only (anyone who controls the repo controls that file).
//!
//! Default is read-only (`tandem-db-mcp --connection X`). With `allow_dangerous`, destructive
//! statements are possible too, each only once the user approved it in the panel within the
//! last 15 minutes: approvals are recorded in `~/.tandem/db/confirm/<project-slug>.json`
//! (outside the repo) and passed as `--allow-dangerous --confirm-file`. The tool definitions
//! are hash-pinned in `.tandem/mcp-pins.json` at first exposure; a changed server is not
//! attached until re-approved.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Connections listed only by the project file (read-only, not saved to the user store).
    #[serde(skip)]
    pub from_project: BTreeSet<String>,
}

fn project_path(root: &Path) -> PathBuf {
    root.join(".tandem").join("db-agents.json")
}

pub use tandem_db::server_name;

/// `~/.tandem/db/confirm/<project-slug>.json`: the user's approvals of destructive
/// statements ([`tandem_db::ConfirmFile`]). Outside the repo, so the project cannot
/// pre-approve anything.
pub fn confirm_file(root: &Path) -> PathBuf {
    tandem_db::confirm_path(root)
}

/// Record the user's approval of one destructive statement on `connection` for agents'
/// MCP servers (valid for [`tandem_db::APPROVAL_TTL`], usable once per server).
pub fn approve_statement(root: &Path, connection: &str, sql: &str) -> std::io::Result<()> {
    tandem_db::ConfirmFile::approve(&confirm_file(root), connection, sql, chrono::Utc::now())
}

/// Policy for statements the user runs in the Database panel: everything allowed, dangerous
/// statements only after the panel's red-card dialog. `confirmed` is that dialog's answer
/// for this statement; the closure is the panel's interactive confirmer.
pub fn panel_policy(confirmed: bool) -> tandem_db::McpPolicy {
    tandem_db::McpPolicy::new(true, true, true, Box::new(move |_: &str, _: &str| confirmed))
}

fn read<T: for<'de> Deserialize<'de> + Default>(p: &Path) -> T {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

impl DbAgents {
    /// The user's store, plus the project file's connections as read-only exposures.
    pub fn load(root: &Path) -> Self {
        Self::load_from(&tandem_db::user_agents_path(root), &project_path(root))
    }

    fn load_from(user: &Path, project: &Path) -> Self {
        let mut me: DbAgents = read(user);
        let project: DbAgents = read(project);
        for (name, exp) in project.connections {
            if exp.allow_write || exp.allow_dangerous {
                tracing::warn!(connection = %name, "ignoring allow_write/allow_dangerous in the project's .tandem/db-agents.json");
            }
            if !me.connections.contains_key(&name) {
                me.connections.insert(name.clone(), Exposure::default());
                me.from_project.insert(name);
            }
        }
        me
    }

    /// Writes the user's store (never the project file). Project-listed connections the
    /// user did not change are left out.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        self.save_to(&tandem_db::user_agents_path(root))
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let mut user = self.clone();
        user.connections.retain(|name, exp| !(self.from_project.contains(name) && *exp == Exposure::default()));
        tandem_core::fs::atomic_write_mode(path, serde_json::to_string_pretty(&user).unwrap_or_default().as_bytes(), 0o600)
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

/// What a new task gets: MCP servers, transcript notes, and the connections it can write to.
#[derive(Default)]
pub struct DbAttach {
    pub specs: Vec<McpServerSpec>,
    pub notes: Vec<String>,
    /// Connections attached with write (or approved-destructive) access.
    pub writable: Vec<String>,
}

/// MCP servers for a new task plus human notes for the transcript.
pub fn mcp_specs(root: &Path) -> DbAttach {
    let cfg = DbAgents::load(root);
    if cfg.connections.is_empty() {
        return DbAttach::default();
    }
    if tandem_db::mcp_binary().is_none() {
        let notes = vec![format!("database not attached: `{MCP_BIN}` not found (build it with `cargo build -p tandem-db`)")];
        return DbAttach { notes, ..Default::default() };
    }
    let pins = PinStore::open(root).ok();
    let tools = tandem_db::tool_definitions_json();
    let mut out = DbAttach::default();
    for (name, exp) in cfg.connections {
        let server = server_name(&name);
        if let Some(p) = &pins
            && matches!(p.check(&server, &tools), PinStatus::Changed { .. })
        {
            out.notes.push(format!("database `{name}` not attached: its MCP tool definitions changed since you approved them"));
            continue;
        }
        out.notes.push(format!("database `{name}` attached via MCP ({})", if exp.allow_write { "read + write" } else { "read-only" }));
        if exp.allow_write || exp.allow_dangerous {
            out.writable.push(name.clone());
        }
        let mut spec = tandem_db::server_spec(&name, exp.allow_write, false);
        if exp.allow_dangerous {
            spec.args.extend(["--allow-dangerous".to_string(), "--confirm-file".to_string(), confirm_file(root).to_string_lossy().into_owned()]);
        }
        out.specs.push(spec);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_file_only_exposes_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let (user, project) = (dir.path().join("user.json"), dir.path().join("project.json"));
        std::fs::write(&project, r#"{"connections":{"prod":{"allow_write":true,"allow_dangerous":true},"mine":{}}}"#).unwrap();
        let a = DbAgents::load_from(&user, &project);
        assert_eq!(a.connections["prod"], Exposure::default());
        assert_eq!(a.from_project.len(), 2);
        // The user grants write on one; only that is saved to the user store.
        let mut a = a;
        a.connections.get_mut("mine").unwrap().allow_write = true;
        a.save_to(&user).unwrap();
        let saved: DbAgents = read(&user);
        assert_eq!(saved.connections.keys().collect::<Vec<_>>(), ["mine"]);
        let b = DbAgents::load_from(&user, &project);
        assert!(b.connections["mine"].allow_write && !b.connections["prod"].allow_write);
        // User-level flags are honoured.
        std::fs::write(&user, r#"{"connections":{"prod":{"allow_dangerous":true}}}"#).unwrap();
        assert!(DbAgents::load_from(&user, &project).connections["prod"].allow_dangerous);
    }
}
