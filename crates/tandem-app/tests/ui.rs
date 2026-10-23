//! UI tests: drive the real Workspace in a headless window through clicks, actions and keys.
//! Run with `cargo test -p tandem-app --test ui`.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions, point, px, size};
use std::path::PathBuf;
use std::sync::Once;
use tandem_app::keymap::GotoFile;
use tandem_app::stripes::ToolWindowId;
use tandem_app::theme::ThemeKind;
use tandem_app::{Workspace, app};

/// A tiny project on disk plus an isolated `TANDEM_HOME`, one per test.
fn fixture(name: &str) -> PathBuf {
    static HOME: Once = Once::new();
    let base = std::env::temp_dir().join(format!("tandem-ui-{}", std::process::id()));
    HOME.call_once(|| {
        // SAFETY: set once, before any test thread reads it.
        unsafe { std::env::set_var("TANDEM_HOME", base.join("home")) };
    });
    let root = base.join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
    std::fs::write(root.join("README.md"), "# fixture\n").unwrap();
    root
}

fn open(name: &str, cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    let root = fixture(name);
    cx.update(|cx| app::init(Some(ThemeKind::Darcula), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(1280.), px(800.)) })),
        ..Default::default()
    };
    let opened = cx.update(|cx| app::open_workspace(root, options, cx)).expect("open workspace");
    cx.run_until_parked();
    opened
}

#[gpui_kit::test]
fn stripe_button_toggles_terminal(cx: &mut TestAppContext) {
    let (window, ws) = open("stripes", cx);
    let before = ws.read_with(cx, |ws, _| ws.is_open(ToolWindowId::Terminal));

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("stripe-tandem.terminal", cx);
    })
    .unwrap();
    assert_eq!(ws.read_with(cx, |ws, _| ws.is_open(ToolWindowId::Terminal)), !before);

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("stripe-tandem.terminal", cx);
    })
    .unwrap();
    assert_eq!(ws.read_with(cx, |ws, _| ws.is_open(ToolWindowId::Terminal)), before);
}

#[gpui_kit::test]
fn goto_file_opens_typed_match(cx: &mut TestAppContext) {
    let (window, ws) = open("search", cx);
    cx.update_window(window, |_, window, cx| window.dispatch_action(Box::new(GotoFile), cx)).unwrap();
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.search.is_some()), "popup opens");

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.input("lib", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();

    let (search_open, active) = ws.read_with(cx, |ws, cx| (ws.search.is_some(), ws.editor.read(cx).active_path()));
    assert!(!search_open, "popup closes after confirming");
    assert!(active.is_some_and(|p| p.ends_with("src/lib.rs")), "lib.rs is open in the editor");
}

#[gpui_kit::test]
fn cmd_d_duplicates_line_in_editor(cx: &mut TestAppContext) {
    let (window, ws) = open("editor", cx);
    cx.update_window(window, |_, window, cx| {
        ws.update(cx, |ws, cx| ws.open_file(PathBuf::from("src/main.rs"), window, cx));
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-d", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let (text, dirty) = ws.read_with(cx, |ws, cx| {
        let f = ws.editor.read(cx).active_file().expect("a file is open");
        (f.state.read(cx).text().to_string(), f.dirty)
    });
    assert_eq!(text, "fn main() {}\nfn main() {}\n");
    assert!(dirty, "tab shows the modified dot");
}
