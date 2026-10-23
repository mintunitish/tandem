//! Smaller tool windows rendered from static rows: Problems, Run, Structure, Bookmarks,
//! Notifications, Services. Integration points noted per constructor.

use crate::assets::icon;
use crate::theme::{MONO_FONT, Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

#[derive(Clone, Copy)]
pub enum Tone {
    Normal,
    Muted,
    Error,
    Warning,
    Ok,
    Accent,
}

fn tone(t: Tone, p: &Palette) -> Hsla {
    match t {
        Tone::Normal => p.text,
        Tone::Muted => p.muted,
        Tone::Error => p.error,
        Tone::Warning => p.warning,
        Tone::Ok => p.ok,
        Tone::Accent => p.accent,
    }
}

pub struct ListRow {
    pub indent: u8,
    pub icon: Option<(IconName, Tone)>,
    pub text: SharedString,
    pub secondary: SharedString,
    pub tone: Tone,
}

fn row(indent: u8, icon: Option<(IconName, Tone)>, text: &str, secondary: &str, tone: Tone) -> ListRow {
    ListRow { indent, icon, text: text.to_string().into(), secondary: secondary.to_string().into(), tone }
}

pub struct ListPanel {
    rows: Vec<ListRow>,
    mono: bool,
    toolbar: Vec<(IconName, &'static str)>,
}

impl ListPanel {
    /// Integration point: `tandem_lsp` diagnostics → rows.
    pub fn problems() -> Self {
        Self {
            rows: vec![
                row(0, Some((IconName::FileCode, Tone::Accent)), "client.ts", "src/http  2 problems", Tone::Normal),
                row(1, Some((IconName::CircleAlert, Tone::Error)), "TS2345: Argument of type 'string | undefined' is not assignable to parameter of type 'string'.", ":31", Tone::Normal),
                row(1, Some((IconName::TriangleAlert, Tone::Warning)), "'attempt' is declared but its value is never read.", ":44", Tone::Normal),
                row(0, Some((IconName::FileCode, Tone::Accent)), "retry.ts", "src/http  1 problem", Tone::Normal),
                row(1, Some((IconName::TriangleAlert, Tone::Warning)), "Unexpected any. Specify a different type.", ":9", Tone::Normal),
            ],
            mono: false,
            toolbar: vec![(IconName::CircleAlert, "Errors"), (IconName::TriangleAlert, "Warnings")],
        }
    }

    /// Integration point: run configurations' process output.
    pub fn run() -> Self {
        Self {
            rows: vec![
                row(0, None, "/usr/local/bin/npm run test -- src/http", "", Tone::Muted),
                row(0, None, "", "", Tone::Normal),
                row(0, None, " RUN  v2.1.4 /Users/dev/acme-web", "", Tone::Normal),
                row(0, None, " ✓ src/http/retry.test.ts (4 tests) 12ms", "", Tone::Ok),
                row(0, None, " ✓ src/http/client.test.ts (9 tests) 31ms", "", Tone::Ok),
                row(0, None, "", "", Tone::Normal),
                row(0, None, " Test Files  2 passed (2)", "", Tone::Normal),
                row(0, None, "      Tests  13 passed (13)", "", Tone::Normal),
                row(0, None, "", "", Tone::Normal),
                row(0, None, "Process finished with exit code 0", "", Tone::Muted),
            ],
            mono: true,
            toolbar: vec![(IconName::RotateCcw, "Rerun"), (IconName::Square, "Stop")],
        }
    }

    /// Integration point: `tandem_project::symbols` outline of the active file.
    pub fn structure() -> Self {
        Self {
            rows: vec![
                row(0, Some((IconName::Braces, Tone::Accent)), "Workspace", "struct", Tone::Normal),
                row(1, Some((IconName::Code, Tone::Warning)), "new(project, window, cx)", "fn", Tone::Normal),
                row(1, Some((IconName::Code, Tone::Warning)), "toggle_tool_window(id, window, cx)", "fn", Tone::Normal),
                row(1, Some((IconName::Code, Tone::Warning)), "open_file(path, window, cx)", "fn", Tone::Normal),
                row(0, Some((IconName::Braces, Tone::Accent)), "impl Render for Workspace", "impl", Tone::Normal),
            ],
            mono: false,
            toolbar: vec![],
        }
    }

    pub fn bookmarks() -> Self {
        Self {
            rows: vec![
                row(0, Some((IconName::Bookmark, Tone::Accent)), "tandem", "3 bookmarks", Tone::Normal),
                row(1, Some((IconName::FileCode, Tone::Accent)), "workspace.rs:142", "toggle_tool_window", Tone::Normal),
                row(1, Some((IconName::FileCode, Tone::Accent)), "agents.rs:88", "permission_card", Tone::Normal),
                row(1, Some((IconName::FileText, Tone::Accent)), "tandem-design.md:73", "UI spec", Tone::Normal),
            ],
            mono: false,
            toolbar: vec![],
        }
    }

    pub fn notifications() -> Self {
        Self {
            rows: vec![
                row(0, Some((IconName::ShieldAlert, Tone::Error)), "Claude Code is waiting for permission", "rm -rf ./dist ./node_modules/.cache · just now", Tone::Normal),
                row(0, Some((IconName::CircleCheck, Tone::Ok)), "OpenCode finished “Fix flaky date formatting test”", "2 files changed · 6 min ago", Tone::Normal),
                row(0, Some((IconName::Gauge, Tone::Warning)), "Claude Max weekly window at 61%", "Router will prefer Codex for Implement · 20 min ago", Tone::Normal),
                row(0, Some((IconName::CircleAlert, Tone::Muted)), "Goose exited with code 1", "eslint flat config migration · 1 h ago", Tone::Normal),
            ],
            mono: false,
            toolbar: vec![],
        }
    }

    pub fn services() -> Self {
        Self {
            rows: vec![
                row(0, Some((IconName::Server, Tone::Accent)), "Docker", "", Tone::Normal),
                row(1, Some((IconName::Package, Tone::Ok)), "postgres:16", "Up 3 hours · 5432", Tone::Normal),
                row(1, Some((IconName::Package, Tone::Muted)), "redis:7", "Exited (0)", Tone::Normal),
                row(0, Some((IconName::Bot, Tone::Accent)), "Agents (ACP)", "", Tone::Normal),
                row(1, Some((IconName::Cpu, Tone::Ok)), "claude-agent-acp", "pid 48211 · task-1", Tone::Normal),
                row(1, Some((IconName::Cpu, Tone::Ok)), "codex-acp", "pid 48230 · task-2", Tone::Normal),
            ],
            mono: false,
            toolbar: vec![],
        }
    }
}

impl Render for ListPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let mono = self.mono;
        h_flex()
            .size_full()
            .items_start()
            .text_size(px(13.))
            .when(!self.toolbar.is_empty(), |d| {
                d.child(
                    v_flex()
                        .w(px(32.))
                        .h_full()
                        .flex_none()
                        .items_center()
                        .pt(px(4.))
                        .gap(px(2.))
                        .border_r_1()
                        .border_color(p.border)
                        .children(self.toolbar.iter().enumerate().map(|(i, (g, _))| {
                            div().id(("misc-tb", i)).size(px(24.)).flex().items_center().justify_center().rounded(px(4.)).hover(|s| s.bg(p.hover)).child(icon(*g, 14., p.muted))
                        })),
                )
            })
            .child(
                v_flex()
                    .id("misc-list")
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .when(mono, |d| d.font_family(MONO_FONT).bg(p.editor_bg).px(px(10.)))
                    .children(self.rows.iter().map(|r| {
                        h_flex()
                            .min_h(px(if mono { 18. } else { 24. }))
                            .pl(px(8. + r.indent as f32 * 18.))
                            .pr(px(8.))
                            .gap(px(6.))
                            .when(!mono, |d| d.hover(|s| s.bg(p.hover)))
                            .children(r.icon.as_ref().map(|(g, t)| icon(*g, 16., tone(*t, &p))))
                            .child(div().text_color(tone(r.tone, &p)).whitespace_nowrap().child(r.text.clone()))
                            .child(div().text_color(p.muted).whitespace_nowrap().child(r.secondary.clone()))
                    })),
            )
    }
}
