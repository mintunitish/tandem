//! Database tool window: connections tree, SQL console (gpui-kit Editor, SQL highlighting) and a
//! results grid (gpui-kit DataTable). Integration points: `tandem_db::connections()` →
//! `DatabasePanel::connections`; `tandem_db::query(conn, sql)` rows → `StringTable::set_rows`.

use crate::assets::icon;
use crate::keymap::NotImplemented;
use crate::model::{DbConnection, fake_db_connections, fake_query_rows};
use crate::theme::{MONO_FONT, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

/// Read-only grid of strings; shared by Database results and the Harness leaderboard.
pub struct StringTable {
    pub cols: Vec<(SharedString, f32)>,
    pub rows: Vec<Vec<SharedString>>,
    pub mono: bool,
}

impl TableDelegate for StringTable {
    fn columns_count(&self, _: &App) -> usize {
        self.cols.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (name, w) = &self.cols[col_ix];
        Column::new(name.clone(), name.clone()).width(px(*w))
    }
    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let text = self.rows.get(row_ix).and_then(|r| r.get(col_ix)).cloned().unwrap_or_default();
        div().text_size(px(12.)).text_color(pal(cx).text).when(self.mono, |d| d.font_family(MONO_FONT)).child(text)
    }
}

pub struct DatabasePanel {
    pub connections: Vec<DbConnection>,
    expanded: Vec<bool>,
    console: Entity<EditorState>,
    results: Entity<TableState<StringTable>>,
    status: SharedString,
}

impl DatabasePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let console = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .default_value("SELECT id, email, plan, created_at, orders\nFROM users\nWHERE plan <> 'free'\nORDER BY created_at DESC\nLIMIT 100;")
        });
        let (cols, rows) = fake_query_rows();
        let widths = [60.0, 220.0, 80.0, 170.0, 80.0];
        let table = StringTable {
            cols: cols.iter().zip(widths).map(|(c, w)| (SharedString::from(*c), w)).collect(),
            rows: rows.into_iter().map(|r| r.into_iter().map(SharedString::from).collect()).collect(),
            mono: true,
        };
        let n = table.rows.len();
        let results = cx.new(|cx| TableState::new(table, window, cx));
        Self {
            connections: fake_db_connections(),
            expanded: vec![true, false],
            console,
            results,
            status: format!("{n} rows retrieved in 38 ms").into(),
        }
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let mut tree: Vec<AnyElement> = Vec::new();
        for (i, c) in self.connections.iter().enumerate() {
            let open = self.expanded[i];
            tree.push(
                h_flex()
                    .id(("db-conn", i))
                    .h(px(24.))
                    .px(px(8.))
                    .gap(px(4.))
                    .hover(|s| s.bg(p.hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded[i] = !this.expanded[i];
                        cx.notify();
                    }))
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14., p.muted))
                    .child(icon(IconName::Database, 16., p.accent))
                    .child(div().text_color(p.text).child(c.name.clone()))
                    .child(div().text_color(p.muted).text_size(px(12.)).child(c.driver))
                    .into_any_element(),
            );
            if open {
                for (j, t) in c.tables.iter().enumerate() {
                    tree.push(
                        h_flex()
                            .id(("db-table", i * 100 + j))
                            .h(px(24.))
                            .pl(px(44.))
                            .gap(px(6.))
                            .hover(|s| s.bg(p.hover))
                            .child(icon(IconName::Table, 14., p.muted))
                            .child(div().text_color(p.text).child(*t))
                            .into_any_element(),
                    );
                }
            }
        }
        h_flex()
            .size_full()
            .items_start()
            .text_size(px(13.))
            .child(
                v_flex()
                    .w(px(220.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(p.border)
                    .child(
                        h_flex()
                            .h(px(30.))
                            .px(px(8.))
                            .gap(px(2.))
                            .child(crate::panels::header_icon("db-add", IconName::Plus, &p))
                            .child(crate::panels::header_icon("db-refresh", IconName::RefreshCw, &p))
                            .child(div().flex_1())
                            .child(div().text_size(px(11.)).text_color(p.ok).child("read-only for agents")),
                    )
                    .child(v_flex().id("db-tree").flex_1().overflow_y_scroll().children(tree)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .h(px(30.))
                            .flex_none()
                            .px(px(8.))
                            .gap(px(6.))
                            .border_b_1()
                            .border_color(p.border)
                            .child(Button::new("db-run").xsmall().ghost().icon(gpui_kit::component::IconName::Play).on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NotImplemented("Execute query".into())), cx)
                            }))
                            .child(div().text_size(px(12.)).text_color(p.muted).child("console · local-postgres")),
                    )
                    .child(
                        div().h(px(110.)).flex_none().bg(p.editor_bg).border_b_1().border_color(p.border).child(
                            Editor::new(&self.console).bordered(false).h_full().font_family(MONO_FONT).text_size(px(13.)),
                        ),
                    )
                    .child(
                        h_flex()
                            .h(px(26.))
                            .flex_none()
                            .px(px(10.))
                            .text_size(px(12.))
                            .text_color(p.muted)
                            .child(format!("Result · {}", self.status)),
                    )
                    .child(div().flex_1().min_h_0().child(DataTable::new(&self.results).stripe(false).bordered(false))),
            )
    }
}
