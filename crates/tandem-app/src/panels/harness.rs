//! Harness tool window: repo leaderboard + conformance runner.
//! Integration point: `tandem_harness::run_conformance(agent)` / `bench(...)` → `HarnessRow`s.

use super::database::StringTable;
use crate::assets::icon;
use crate::keymap::NotImplemented;
use crate::model::fake_leaderboard;
use crate::theme::pal;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::*;

pub struct HarnessPanel {
    table: Entity<TableState<StringTable>>,
}

impl HarnessPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rows = fake_leaderboard()
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                vec![
                    format!("#{}", i + 1).into(),
                    r.agent.into(),
                    format!("{}/{}  ({}%)", r.pass, r.total, r.pass * 100 / r.total).into(),
                    r.median_time.into(),
                    r.cost.into(),
                    r.conformance.into(),
                ]
            })
            .collect();
        let table = StringTable {
            cols: vec![("Rank".into(), 60.), ("Agent".into(), 160.), ("Tasks passed".into(), 140.), ("Median time".into(), 110.), ("Cost".into(), 120.), ("Conformance".into(), 110.)],
            rows,
            mono: false,
        };
        Self { table: cx.new(|cx| TableState::new(table, window, cx)) }
    }
}

impl Render for HarnessPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_none()
                    .px(px(10.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(icon(IconName::Trophy, 16., p.warning))
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(p.text).child("Repo leaderboard"))
                    .child(div().text_size(px(12.)).text_color(p.muted).child("20 tasks from this repo · last run today 09:30"))
                    .child(div().flex_1())
                    .child(Button::new("run-conformance").small().primary().icon(gpui_kit::component::IconName::Play).label("Run conformance").on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(NotImplemented("Run conformance".into())), cx)
                    }))
                    .child(Button::new("run-bench").small().label("Run benchmark…").on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(NotImplemented("Run benchmark".into())), cx)
                    })),
            )
            .child(div().flex_1().min_h_0().child(DataTable::new(&self.table).stripe(false).bordered(false)))
    }
}
