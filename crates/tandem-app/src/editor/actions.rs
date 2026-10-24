//! Editor/LSP/VCS actions and their IntelliJ (macOS) bindings. Bound after `keymap::bind_keys`
//! so the editor-scoped ones win over gpui-kit's `Input` defaults.

use crate::keymap::{CODE_EDITOR, FindInFiles, ToggleCommit, ToggleStructure};
use gpui_kit::component::input::{GoToDefinition, ShowCompletions, ToggleCodeActions};
use gpui_kit::*;

actions!(
    tandem_editor,
    [
        /// ⇧F6: rename the symbol at the caret across the project (LSP).
        RenameSymbol,
        /// ⌥F7: find usages of the symbol at the caret (LSP references).
        FindUsages,
        /// ⌘⌥L: format the current file (LSP).
        ReformatCode,
        /// Title-bar / status-bar branch widget: list branches, checkout.
        ShowBranches,
        /// ⌘T inside the terminal: new terminal tab.
        NewTerminalTab,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", GoToDefinition, Some(CODE_EDITOR)),
        KeyBinding::new("alt-enter", ToggleCodeActions, Some(CODE_EDITOR)),
        KeyBinding::new("ctrl-space", ShowCompletions, Some(CODE_EDITOR)),
        KeyBinding::new("shift-f6", RenameSymbol, Some(CODE_EDITOR)),
        KeyBinding::new("alt-f7", FindUsages, Some(CODE_EDITOR)),
        KeyBinding::new("cmd-alt-l", ReformatCode, Some(CODE_EDITOR)),
        // gpui-kit binds ⌘⇧F to in-editor Replace; IntelliJ uses it for Find in Files.
        KeyBinding::new("cmd-shift-f", FindInFiles, Some(CODE_EDITOR)),
        KeyBinding::new("cmd-f12", ToggleStructure, None),
        KeyBinding::new("cmd-k", ToggleCommit, None),
    ]);
}
