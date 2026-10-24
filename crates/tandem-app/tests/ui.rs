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
        unsafe {
            std::env::set_var("TANDEM_HOME", base.join("home"));
            // Quota providers stay in-memory (no app-server spawn, no log scans).
            std::env::set_var("TANDEM_QUOTA_OFFLINE", "1");
            std::env::set_var("TANDEM_QUOTA_DIR", base.join("quota"));
            std::env::set_var(tandem_db::STORE_ENV, base.join("db/connections.json"));
        };
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
    // The file watcher, LSP bridge and PTY reader wake gpui tasks from their own threads.
    cx.executor().allow_parking();
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

/// Agent cockpit → Review flow against the scripted ACP agent (`tandem-acp-fake-agent`):
/// it streams text + a plan, asks permission for `rm -rf build` (Destructive), writes
/// `out.txt` in its worktree, reports usage and ends the turn.
mod cockpit {
    use super::*;
    use std::time::{Duration, Instant};
    use tandem_app::model::agents::TaskStatus;
    use tandem_app::panels::agents::AgentsPanel;
    use tandem_core::{AgentEvent, RiskClass, TaskId};

    /// Path of the fake agent binary, built on first use into this test's target dir.
    fn fake_agent() -> PathBuf {
        static BUILD: Once = Once::new();
        // target/<dir>/debug/deps/ui-<hash> → target/<dir>/debug/tandem-acp-fake-agent
        let exe = std::env::current_exe().unwrap();
        let debug = exe.parent().and_then(|d| d.parent()).unwrap().to_path_buf();
        let bin = debug.join("tandem-acp-fake-agent");
        BUILD.call_once(|| {
            if bin.is_file() {
                return;
            }
            let target = debug.parent().unwrap();
            let st = std::process::Command::new(env!("CARGO"))
                .args(["build", "-p", "tandem-acp", "--bin", "tandem-acp-fake-agent"])
                .env("CARGO_TARGET_DIR", target)
                .status()
                .expect("run cargo");
            assert!(st.success(), "building tandem-acp-fake-agent failed");
        });
        assert!(bin.is_file(), "missing {}", bin.display());
        bin
    }

    fn git(root: &std::path::Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    /// `fixture` as a committed git repo; agents run without Seatbelt (CI may forbid nesting)
    /// and the quota pool stays offline.
    fn open_repo(name: &str, cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>, PathBuf) {
        // Agent events arrive from tokio threads: real I/O, not the deterministic scheduler.
        cx.executor().allow_parking();
        let root = fixture(name);
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "init"]);
        std::fs::create_dir_all(root.join(".tandem")).unwrap();
        std::fs::write(
            root.join(".tandem/policy.json"),
            r#"{"auto_allow_safe":true,"sandbox":false,"trusted_commands":[],"protected_paths":[],"env_passthrough":[]}"#,
        )
        .unwrap();
        cx.update(|cx| app::init(Some(ThemeKind::Darcula), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(1400.), px(900.)) })),
            ..Default::default()
        };
        let (w, ws) = cx.update(|cx| app::open_workspace(root.clone(), options, cx)).expect("open workspace");
        cx.run_until_parked();
        let root = ws.read_with(cx, |ws, _| ws.root.clone());
        (w, ws, root)
    }

    /// Pump the foreground executor until `cond` holds (agent events arrive from tokio threads).
    fn wait(cx: &mut TestAppContext, what: &str, mut cond: impl FnMut(&mut TestAppContext) -> bool) {
        let start = Instant::now();
        loop {
            cx.run_until_parked();
            if cond(cx) {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn start_fake_task(window: AnyWindowHandle, ws: &Entity<Workspace>, cx: &mut TestAppContext) -> (Entity<AgentsPanel>, TaskId) {
        let bin = fake_agent();
        cx.update_window(window, |_, window, cx| ws.update(cx, |ws, cx| ws.show(ToolWindowId::Agents, window, cx))).unwrap();
        let agents = ws.read_with(cx, |ws, _| ws.agents.clone());
        let id = agents.update(cx, |a, cx| {
            a.add_agent(
                tandem_core::AgentDescriptor {
                    id: "fake".into(),
                    display_name: "Fake".into(),
                    vendor: String::new(),
                    command: bin.to_string_lossy().into_owned(),
                    args: vec![],
                    env: Default::default(),
                    install_hint: String::new(),
                    acp: true,
                    auth_note: String::new(),
                    ..Default::default()
                },
                cx,
            );
            a.pinned = Some("fake".into());
            a.start_task("Clean the build directory".into(), cx).expect("task starts")
        });
        wait(cx, "the permission card", |cx| agents.read_with(cx, |a, _| a.model.task(&id).is_some_and(|t| t.status == TaskStatus::NeedsApproval)));
        (agents, id)
    }

    /// Event index of the open permission request (the card's element ids use it).
    fn perm_index(agents: &Entity<AgentsPanel>, id: &TaskId, cx: &mut TestAppContext) -> usize {
        agents.read_with(cx, |a, _| {
            let t = a.model.task(id).unwrap();
            t.events.iter().position(|e| matches!(e, AgentEvent::PermissionRequested(r) if !t.is_resolved(&r.id))).expect("open request")
        })
    }

    fn click(window: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    #[gpui_kit::test]
    fn task_creates_worktree_and_streams_events(cx: &mut TestAppContext) {
        let (window, ws, root) = open_repo("cockpit-stream", cx);
        let (agents, id) = start_fake_task(window, &ws, cx);
        let worktree = root.join(".tandem/worktrees").join(id.as_str());
        assert!(worktree.join("README.md").is_file(), "worktree checked out at {}", worktree.display());
        assert!(worktree.join(".agents/skills").is_dir(), "skill presets materialized");
        agents.read_with(cx, |a, _| {
            let t = a.model.task(&id).unwrap();
            assert!(t.events.iter().any(|e| matches!(e, AgentEvent::TextDelta { text, .. } if text == "Hello")), "agent text streamed");
            assert!(t.events.iter().any(|e| matches!(e, AgentEvent::ToolCall(tc) if tc.title == "Remove build dir")), "tool call card");
        });
        let log = std::fs::read_to_string(root.join(".tandem/tasks").join(id.as_str()).join("log.jsonl")).unwrap();
        assert!(log.contains("permission_requested"), "replay log records the request");
        assert!(ws.read_with(cx, |ws, cx| ws.agents.read(cx).model.running_count()) == 1);
        agents.update(cx, |a, cx| a.cancel_task(&id, cx));
    }

    #[gpui_kit::test]
    fn destructive_card_reject_sends_decision(cx: &mut TestAppContext) {
        let (window, ws, _) = open_repo("cockpit-reject", cx);
        let (agents, id) = start_fake_task(window, &ws, cx);
        let i = perm_index(&agents, &id, cx);
        let risk = agents.read_with(cx, |a, _| match &a.model.task(&id).unwrap().events[i] {
            AgentEvent::PermissionRequested(r) => r.risk,
            _ => unreachable!(),
        });
        assert_eq!(risk, RiskClass::Destructive, "rm -rf is classified destructive");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("perm-reject", i)).is_some(), "red card rendered with its buttons");
            assert!(window.try_find(("perm-always", i)).is_none(), "no Allow always on destructive cards");
        })
        .unwrap();
        click(window, ("perm-reject", i), cx);
        wait(cx, "the turn to end", |cx| agents.read_with(cx, |a, _| a.model.task(&id).is_some_and(|t| !t.status.is_active())));
        agents.read_with(cx, |a, _| {
            let t = a.model.task(&id).unwrap();
            assert!(t.events.iter().any(|e| matches!(e, AgentEvent::PermissionResolved { option, by: tandem_core::Decider::User, .. } if option == "reject")));
            let tool = t.merged_tool(&"t1".into()).cloned().unwrap();
            assert!(tool.content.unwrap_or_default().contains("permission=reject"), "the agent saw the rejection");
            assert_eq!(t.status, TaskStatus::Done);
        });
    }

    #[gpui_kit::test]
    fn review_accept_and_apply_writes_main_tree(cx: &mut TestAppContext) {
        let (window, ws, root) = open_repo("cockpit-review", cx);
        let (agents, id) = start_fake_task(window, &ws, cx);
        let i = perm_index(&agents, &id, cx);
        click(window, ("perm-allow", i), cx);
        wait(cx, "the diff", |cx| agents.read_with(cx, |a, _| a.model.task(&id).is_some_and(|t| t.diff.is_some())));
        // The finished task lands in Review on its own; open the tool window to click in it.
        wait(cx, "review to load", |cx| ws.read_with(cx, |ws, cx| ws.review.read(cx).task() == Some(&id)));
        let files: Vec<String> =
            ws.read_with(cx, |ws, cx| ws.review.read(cx).state().unwrap().diff().files.iter().map(|f| f.path.display().to_string()).collect());
        assert_eq!(files, vec!["out.txt"], "skill files are filtered out of the review");
        cx.update_window(window, |_, window, cx| ws.update(cx, |ws, cx| ws.show(ToolWindowId::Review, window, cx))).unwrap();
        assert!(!root.join("out.txt").exists());
        click(window, ("accept", 0usize), cx);
        click(window, "apply-accepted", cx);
        assert_eq!(std::fs::read_to_string(root.join("out.txt")).unwrap(), "a\nb\nc\n");
        let blame = tandem_app::tasks::blame::lookup(&root, "out.txt", 2).expect("agent blame recorded");
        assert_eq!((blame.task, blame.agent.as_str()), (id.clone(), "fake"));
        assert!(root.join(".tandem/tasks").join(id.as_str()).join("review.json").is_file(), "decisions persisted");
    }
}

#[gpui_kit::test]
fn search_everywhere_symbols_tab_finds_function(cx: &mut TestAppContext) {
    use tandem_app::search_everywhere::{SearchItem, SearchTab};
    let (window, ws) = open("symbols", cx);
    cx.update_window(window, |_, window, cx| window.dispatch_action(Box::new(tandem_app::keymap::GotoSymbol), cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.input("add", cx);
    })
    .unwrap();
    // Typing re-ranks after a short debounce.
    cx.executor().advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();

    let (tab, names) = ws.read_with(cx, |ws, cx| {
        let s = ws.search.as_ref().expect("popup open").read(cx);
        let names: Vec<String> = s.results.iter().filter_map(|i| if let SearchItem::Symbol { name, .. } = i { Some(name.clone()) } else { None }).collect();
        (s.tab, names)
    });
    assert_eq!(tab, SearchTab::Symbols);
    assert!(names.iter().any(|n| n == "add"), "symbol `add` found, got {names:?}");

    cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    let active = ws.read_with(cx, |ws, cx| ws.editor.read(cx).active_path());
    assert!(active.is_some_and(|p| p.ends_with("src/lib.rs")), "lib.rs opened at the symbol");
}

#[gpui_kit::test]
fn terminal_panel_shows_echo_output(cx: &mut TestAppContext) {
    let (window, ws) = open("terminal", cx);
    cx.update_window(window, |_, window, cx| {
        ws.update(cx, |ws, cx| {
            if !ws.is_open(ToolWindowId::Terminal) {
                ws.show(ToolWindowId::Terminal, window, cx);
            }
        });
        window.render_frame(cx);
    })
    .unwrap();
    let term = ws.read_with(cx, |ws, _| ws.terminal.clone());
    assert_eq!(term.read_with(cx, |t, _| t.tab_count()), 1, "a shell session starts when the panel is shown");
    term.read_with(cx, |t, _| t.send_text("echo tandem-ok\r"));

    let mut screen = String::new();
    for _ in 0..300 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        screen = term.read_with(cx, |t, _| t.screen_text());
        if screen.lines().any(|l| l.trim() == "tandem-ok") {
            break;
        }
    }
    assert!(screen.lines().any(|l| l.trim() == "tandem-ok"), "terminal shows the echo output:\n{screen}");
}
