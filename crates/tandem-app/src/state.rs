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

/// `.tandem/policy.json` inside the project (spec: project-level policy).
pub fn load_policy(project: &Path) -> tandem_core::Policy {
    read_json(&project.join(".tandem/policy.json")).unwrap_or_default()
}

pub fn save_policy(project: &Path, policy: &tandem_core::Policy) {
    write_json(&project.join(".tandem/policy.json"), policy);
}
