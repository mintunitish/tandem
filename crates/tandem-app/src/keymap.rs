//! macOS IntelliJ keymap. One table drives key bindings, the Search Everywhere "Actions" tab and
//! the read-only Keymap settings page.

use gpui_kit::*;
use serde::Deserialize;

actions!(
    tandem,
    [
        SearchEverywhere,
        GotoFile,
        GotoSymbol,
        GotoAction,
        FindInFiles,
        RecentFiles,
        ToggleProject,
        ToggleCommit,
        ToggleStructure,
        ToggleBookmarks,
        ToggleAgents,
        ToggleReview,
        ToggleDatabase,
        ToggleNotifications,
        ToggleTerminal,
        ToggleRun,
        ToggleProblems,
        ToggleGit,
        ToggleServices,
        ToggleHarness,
        HideToolWindows,
        CloseTab,
        NextTab,
        PrevTab,
        SaveFile,
        GoToLine,
        ToggleComment,
        DuplicateLine,
        DeleteLine,
        MoveLineUp,
        MoveLineDown,
        Find,
        OpenSettings,
        StopAllAgents,
        ToggleTheme,
        SelectInProjectView,
        Quit,
    ]
);

/// A placeholder action for menu items that exist but are not wired yet; shows a toast.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = tandem, no_json)]
pub struct NotImplemented(pub SharedString);

/// Key contexts.
pub const WORKSPACE: &str = "Workspace";
pub const CODE_EDITOR: &str = "Input && mode == editor";
pub const POPUP: &str = "TandemPopup";

pub struct KeymapEntry {
    pub name: &'static str,
    /// gpui keystroke syntax; empty when unbound.
    pub keys: &'static str,
    pub action: fn() -> Box<dyn Action>,
}

macro_rules! entries {
    ($( $name:literal, $keys:literal, $action:expr; )*) => {
        /// The keymap table, built once.
        pub fn entries() -> &'static [KeymapEntry] {
            static ENTRIES: std::sync::OnceLock<Vec<KeymapEntry>> = std::sync::OnceLock::new();
            ENTRIES.get_or_init(|| vec![$( KeymapEntry { name: $name, keys: $keys, action: || Box::new($action) } ),*])
        }
    };
}

entries! {
    "Search Everywhere", "shift shift", SearchEverywhere;
    "Go to File…", "cmd-shift-o", GotoFile;
    "Go to Symbol…", "cmd-alt-o", GotoSymbol;
    "Find Action…", "cmd-shift-a", GotoAction;
    "Find in Files…", "cmd-shift-f", FindInFiles;
    "Recent Files", "cmd-e", RecentFiles;
    "Project", "cmd-1", ToggleProject;
    "Commit", "cmd-0", ToggleCommit;
    "Structure", "cmd-7", ToggleStructure;
    "Bookmarks", "cmd-2", ToggleBookmarks;
    "Agents", "cmd-alt-a", ToggleAgents;
    "Review", "", ToggleReview;
    "Database", "", ToggleDatabase;
    "Notifications", "", ToggleNotifications;
    "Terminal", "alt-f12", ToggleTerminal;
    "Run", "cmd-4", ToggleRun;
    "Problems", "cmd-6", ToggleProblems;
    "Git", "cmd-9", ToggleGit;
    "Services", "cmd-8", ToggleServices;
    "Harness", "", ToggleHarness;
    "Hide All Tool Windows", "cmd-shift-f12", HideToolWindows;
    "Close Tab", "cmd-w", CloseTab;
    "Select Next Tab", "cmd-shift-]", NextTab;
    "Select Previous Tab", "cmd-shift-[", PrevTab;
    "Save All", "cmd-s", SaveFile;
    "Go to Line:Column…", "cmd-g", GoToLine;
    "Comment with Line Comment", "cmd-/", ToggleComment;
    "Duplicate Line or Selection", "cmd-d", DuplicateLine;
    "Delete Line", "cmd-backspace", DeleteLine;
    "Move Line Up", "alt-shift-up", MoveLineUp;
    "Move Line Down", "alt-shift-down", MoveLineDown;
    "Find…", "cmd-f", Find;
    "Settings…", "cmd-,", OpenSettings;
    "Stop All Agents", "cmd-alt-shift-k", StopAllAgents;
    "Toggle Light/Dark Theme", "", ToggleTheme;
    "Select in Project View", "alt-f1", SelectInProjectView;
    "Quit Tandem", "cmd-q", Quit;
}

/// Editor-only bindings that must beat gpui-kit's `Input` defaults (e.g. cmd-backspace).
const EDITOR_SCOPED: &[&str] = &["cmd-backspace", "cmd-d", "cmd-/", "alt-shift-up", "alt-shift-down", "cmd-g"];

pub fn bind_keys(cx: &mut App) {
    let mut bindings = Vec::new();
    for e in entries() {
        if e.keys.is_empty() {
            continue;
        }
        // `cmd-f` stays with gpui-kit's built-in in-editor search (input::Search).
        if e.keys == "cmd-f" {
            continue;
        }
        let context = EDITOR_SCOPED
            .contains(&e.keys)
            .then(|| KeyBindingContextPredicate::parse(CODE_EDITOR).ok().map(std::rc::Rc::new))
            .flatten();
        bindings.push(KeyBinding::load(e.keys, (e.action)(), context, false, None, cx.keyboard_mapper().as_ref()).ok());
    }
    // The WebStorm "move line" chord also works with plain alt-up/down in the editor.
    bindings.push(Some(KeyBinding::new("alt-up", MoveLineUp, Some(CODE_EDITOR))));
    bindings.push(Some(KeyBinding::new("alt-down", MoveLineDown, Some(CODE_EDITOR))));
    cx.bind_keys(bindings.into_iter().flatten());
}

/// `cmd-shift-o` → `⌘⇧O`, `shift shift` → `⇧⇧`.
pub fn display_keys(keys: &str) -> String {
    keys.split(' ')
        .map(|stroke| {
            let mut out = String::new();
            let parts: Vec<&str> = stroke.split('-').collect();
            let (mods, key) = match parts.as_slice() {
                [] => (&[][..], ""),
                [k] => (&[][..], *k),
                [m @ .., k] if !k.is_empty() => (m, *k),
                [m @ .., _] => (&m[..m.len().saturating_sub(1)], "-"),
            };
            for m in mods {
                out.push_str(match *m {
                    "ctrl" => "⌃",
                    "alt" => "⌥",
                    "shift" => "⇧",
                    "cmd" => "⌘",
                    other => other,
                });
            }
            let key = match key {
                "shift" => "⇧".to_string(),
                "backspace" => "⌫".to_string(),
                "enter" => "⏎".to_string(),
                "escape" => "⎋".to_string(),
                "up" => "↑".to_string(),
                "down" => "↓".to_string(),
                "left" => "←".to_string(),
                "right" => "→".to_string(),
                "space" => "Space".to_string(),
                k if k.len() == 1 => k.to_uppercase(),
                k => k.to_uppercase(),
            };
            out.push_str(&key);
            out
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::display_keys;

    #[test]
    fn mac_glyphs() {
        assert_eq!(display_keys("cmd-shift-o"), "⌘⇧O");
        assert_eq!(display_keys("shift shift"), "⇧⇧");
        assert_eq!(display_keys("alt-f12"), "⌥F12");
        assert_eq!(display_keys("cmd-backspace"), "⌘⌫");
        assert_eq!(display_keys("cmd-/"), "⌘/");
    }
}
