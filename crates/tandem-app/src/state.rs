//! Persisted app state under `~/.tandem/`: settings, window bounds, tool-window layout.

use crate::stripes::{Side, ToolWindowId};
use crate::theme::ThemeKind;
use gpui_kit::Global;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub use tandem_core::fs::tandem_home;

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(path, json);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: ThemeKind,
    pub editor_font_size: f32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { theme: ThemeKind::Darcula, editor_font_size: 13.0 }
    }
}

impl Global for AppSettings {}

impl AppSettings {
    pub fn load() -> Self {
        read_json(&tandem_home().join("settings.json")).unwrap_or_default()
    }
    pub fn save(&self) {
        write_json(&tandem_home().join("settings.json"), self);
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowGeometry {
    pub fn load() -> Option<Self> {
        read_json(&tandem_home().join("window.json"))
    }
    pub fn save(&self) {
        write_json(&tandem_home().join("window.json"), self);
    }
}

/// What one side dock shows, whether it is open, and its size.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SideLayout {
    pub id: ToolWindowId,
    pub open: bool,
    pub size: f32,
}

/// Tool-window layout, one entry per [`Side`] (indexed by `Side::index`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutState {
    pub sides: [SideLayout; 3],
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            sides: [
                SideLayout { id: ToolWindowId::Project, open: true, size: 280.0 },
                SideLayout { id: ToolWindowId::Agents, open: false, size: 520.0 },
                SideLayout { id: ToolWindowId::Terminal, open: false, size: 260.0 },
            ],
        }
    }
}

impl LayoutState {
    pub fn load() -> Self {
        read_json(&tandem_home().join("layout.json")).unwrap_or_default()
    }
    pub fn save(&self) {
        write_json(&tandem_home().join("layout.json"), self);
    }
    pub fn side(&self, side: Side) -> &SideLayout {
        &self.sides[side.index()]
    }
    pub fn side_mut(&mut self, side: Side) -> &mut SideLayout {
        &mut self.sides[side.index()]
    }
}

/// The user's policy, `~/.tandem/policy.json` (default: strict).
pub fn load_user_policy() -> tandem_core::Policy {
    read_json(&tandem_home().join("policy.json")).unwrap_or_default()
}

/// Writes the user's policy (`~/.tandem/policy.json`, never the project file) off the UI
/// thread.
pub fn save_user_policy(policy: &tandem_core::Policy) {
    match serde_json::to_vec_pretty(policy) {
        Ok(json) => persist(tandem_home().join("policy.json"), json, true),
        Err(e) => tracing::warn!("policy: {e}"),
    }
}

/// The effective policy for `project`: the user's policy, tightened by the project's
/// `.tandem/policy.json` ([`tandem_core::Policy::merge_project`]). The second value is a
/// one-line notification when the project file tried to loosen anything (ignored).
pub fn load_policy(project: &Path) -> (tandem_core::Policy, Option<String>) {
    merge_policy(load_user_policy(), &project.join(".tandem/policy.json"))
}

fn merge_policy(user: tandem_core::Policy, project_file: &Path) -> (tandem_core::Policy, Option<String>) {
    let Some(project) = read_json::<tandem_core::ProjectPolicy>(project_file) else { return (user, None) };
    let loose = project.loosening(&user);
    let merged = tandem_core::Policy::merge_project(&user, &project);
    if loose.is_empty() {
        return (merged, None);
    }
    tracing::warn!(file = %project_file.display(), fields = ?loose, "project policy tried to loosen the user policy; ignored");
    let what: Vec<String> = loose
        .iter()
        .map(|f| match *f {
            "sandbox" => "disable sandbox".to_string(),
            "strict_permissions" => "disable strict permissions".to_string(),
            "auto_allow_safe" => "enable auto-allow".to_string(),
            other => format!("set {other}"),
        })
        .collect();
    (merged, Some(format!("Project policy tried to {}; ignored", what.join(", "))))
}

enum WriteJob {
    Write { path: std::path::PathBuf, bytes: Vec<u8>, sync: bool },
    Flush(std::sync::mpsc::Sender<()>),
}

/// One background writer thread, so writes to the same file land in the order they were
/// queued.
fn writer() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<WriteJob>> {
    static W: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<WriteJob>>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<WriteJob>();
        let spawned = std::thread::Builder::new().name("tandem-persist".into()).spawn(move || {
            for job in rx {
                match job {
                    WriteJob::Write { path, bytes, sync } => {
                        if let Err(e) = tandem_core::fs::atomic_write(&path, &bytes, sync) {
                            tracing::warn!("saving {}: {e}", path.display());
                        }
                    }
                    WriteJob::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("persist thread: {e}");
        }
        std::sync::Mutex::new(tx)
    })
}

/// Write `bytes` to `path` off the UI thread (callers serialize on the UI thread). With
/// `sync` the file is fsynced. Falls back to a direct write if the writer thread is gone.
pub fn persist(path: std::path::PathBuf, bytes: Vec<u8>, sync: bool) {
    let tx = writer().lock().unwrap_or_else(|e| e.into_inner());
    if let Err(std::sync::mpsc::SendError(WriteJob::Write { path, bytes, sync })) = tx.send(WriteJob::Write { path, bytes, sync }) {
        let _ = tandem_core::fs::atomic_write(&path, &bytes, sync);
    }
}

/// Block until every write queued with [`persist`] so far is on disk (at most 2 s).
pub fn flush_writes() {
    let (done, wait) = std::sync::mpsc::channel();
    let sent = writer().lock().unwrap_or_else(|e| e.into_inner()).send(WriteJob::Flush(done));
    if sent.is_ok() {
        let _ = wait.recv_timeout(std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_writes_in_order_and_flushes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a/b.json");
        for i in 0..50 {
            persist(f.clone(), i.to_string().into_bytes(), i % 2 == 0);
        }
        flush_writes();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "49");
    }

    #[test]
    fn project_policy_cannot_loosen() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("policy.json");
        // Missing project file: the user policy as is.
        assert_eq!(merge_policy(tandem_core::Policy::default(), &f), (tandem_core::Policy::default(), None));
        std::fs::write(&f, r#"{"sandbox":false,"trusted_commands":["rm -rf /"],"protected_paths":["secrets"]}"#).unwrap();
        let (p, note) = merge_policy(tandem_core::Policy::default(), &f);
        assert!(p.sandbox && p.trusted_commands.is_empty());
        assert_eq!(p.protected_paths, ["secrets"]);
        assert_eq!(note.as_deref(), Some("Project policy tried to disable sandbox, set trusted_commands; ignored"));
        // A full legacy file (every field) that only restates defaults is not a warning.
        std::fs::write(&f, serde_json::to_string(&tandem_core::Policy::default()).unwrap()).unwrap();
        assert_eq!(merge_policy(tandem_core::Policy::default(), &f).1, None);
    }
}
