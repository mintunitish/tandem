//! Harness tool window: run conformance scenarios / bench tasks against the installed agents
//! in-process (`tandem_harness` + `AcpRunner` on the tokio runtime), live progress, result
//! matrix, repo leaderboard with its SVG scorecard, "Copy markdown", "Open report folder".

use super::database::StringTable;
use crate::assets::icon;
use crate::model::agents::agent_badge;
use crate::runtime;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::AgentId;
use tandem_harness::{AcpRunner, AgentRunner, BenchTask, Report, RunKind, RunOptions, RunResult, Scenario, Status};

enum Progress {
    Started(String),
    Result(RunResult),
    Finished(Result<(Report, PathBuf), String>),
}

pub struct HarnessPanel {
    pub harness_root: Option<PathBuf>,
    pub agents: Vec<AgentId>,
    pub selected_agents: HashSet<AgentId>,
    pub scenarios: Vec<Scenario>,
    pub selected_scenarios: HashSet<String>,
    pub bench: Vec<BenchTask>,
    pub selected_bench: HashSet<String>,
    pub running: Option<RunKind>,
    pub current: Option<String>,
    pub total: usize,
    pub results: Vec<RunResult>,
    pub report: Option<Report>,
    pub report_dir: Option<PathBuf>,
    load_error: Option<String>,
    matrix: Entity<TableState<StringTable>>,
    board: Entity<TableState<StringTable>>,
    svg: Option<Arc<Image>>,
}

/// `<root>/harness`, else `$TANDEM_HARNESS_DIR`.
pub fn find_harness_root(root: &Path) -> Option<PathBuf> {
    let local = root.join("harness");
    if local.join("scenarios").is_dir() {
        return Some(local);
    }
    std::env::var_os("TANDEM_HARNESS_DIR").map(PathBuf::from).filter(|p| p.join("scenarios").is_dir())
}

impl HarnessPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let harness_root = find_harness_root(&root);
        let mut load_error = None;
        let (scenarios, bench) = match &harness_root {
            Some(h) => {
                let s = tandem_harness::load_scenarios(&h.join("scenarios")).unwrap_or_else(|e| {
                    load_error = Some(format!("scenarios: {e:#}"));
                    vec![]
                });
                let b = tandem_harness::load_bench(&h.join("bench")).unwrap_or_default();
                (s, b)
            }
            None => (vec![], vec![]),
        };
        let agents = AcpRunner::detect().agents();
        let matrix = cx.new(|cx| TableState::new(StringTable::new(vec![], vec![], false), window, cx));
        let board = cx.new(|cx| TableState::new(StringTable::new(vec![], vec![], false), window, cx));
        let mut this = Self {
            selected_agents: agents.iter().cloned().collect(),
            agents,
            selected_scenarios: scenarios.iter().map(|s| s.id.clone()).collect(),
            scenarios,
            selected_bench: bench.iter().map(|b| b.id.clone()).collect(),
            bench,
            harness_root,
            running: None,
            current: None,
            total: 0,
            results: vec![],
            report: None,
            report_dir: None,
            load_error,
            matrix,
            board,
            svg: None,
        };
        this.load_latest(cx);
        this
    }

    /// Show the newest report under `harness/reports/` (read-only).
    fn load_latest(&mut self, cx: &mut Context<Self>) {
        let Some(h) = &self.harness_root else { return };
        let Ok(dirs) = std::fs::read_dir(h.join("reports")) else { return };
        let mut dirs: Vec<PathBuf> = dirs.flatten().map(|d| d.path()).filter(|p| p.join("report.json").is_file()).collect();
        dirs.sort();
        let Some(dir) = dirs.pop() else { return };
        if let Some(r) = std::fs::read_to_string(dir.join("report.json")).ok().and_then(|s| serde_json::from_str::<Report>(&s).ok()) {
            self.results = r.results.clone();
            self.set_report(r, dir, cx);
        }
    }

    fn set_report(&mut self, r: Report, dir: PathBuf, cx: &mut Context<Self>) {
        let svg = tandem_harness::leaderboard_svg(&r.date, &r.leaderboard());
        self.svg = Some(Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())));
        self.report = Some(r);
        self.report_dir = Some(dir);
        self.refresh_tables(cx);
    }

    fn refresh_tables(&mut self, cx: &mut Context<Self>) {
        // Matrix: one row per scenario/task id, one column per agent.
        let mut agents: Vec<AgentId> = vec![];
        let mut ids: Vec<(String, String)> = vec![];
        for r in &self.results {
            if !agents.contains(&r.agent) {
                agents.push(r.agent.clone());
            }
            if !ids.iter().any(|(i, _)| i == &r.id) {
                ids.push((r.id.clone(), r.title.clone()));
            }
        }
        let mut headers = vec!["Scenario / task".to_string()];
        headers.extend(agents.iter().map(|a| agent_badge(a).2.to_string()));
        let rows: Vec<Vec<String>> = ids
            .iter()
            .map(|(id, _)| {
                let mut row = vec![id.clone()];
                for a in &agents {
                    let cell = self.results.iter().rfind(|r| &r.id == id && &r.agent == a).map(|r| {
                        let mark = match r.status {
                            Status::Pass => "PASS",
                            Status::Fail => "FAIL",
                            Status::Skip => "skip",
                            Status::Blocked => "blocked",
                        };
                        if r.status == Status::Pass { format!("{mark} {:.0}s", r.elapsed_s) } else { format!("{mark} {}", short(&r.reason, 60)) }
                    });
                    row.push(cell.unwrap_or_default());
                }
                row
            })
            .collect();
        let mut matrix = StringTable::auto(headers, rows, false);
        if let Some(first) = matrix.cols.first_mut() {
            first.1 = first.1.max(180.);
        }
        self.matrix.update(cx, |t, cx| {
            *t.delegate_mut() = matrix;
            t.refresh(cx);
        });
        let report = Report::new("live", "acp", self.results.clone());
        let board: Vec<Vec<String>> = report
            .leaderboard()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                vec![
                    format!("#{}", i + 1),
                    agent_badge(&r.agent).2.to_string(),
                    format!("{}/{} ({:.0}%)", r.passed, r.runs, r.pass_rate),
                    format!("{:.0}s", r.median_time_s),
                    crate::panels::agents::fmt_tokens(r.tokens),
                    r.cost_usd.map(|c| format!("${c:.2}")).unwrap_or_else(|| "—".into()),
                    format!("{:.1}", r.score),
                ]
            })
            .collect();
        let headers = ["Rank", "Agent", "Passed", "Median time", "Tokens", "Cost", "Score"].map(String::from).to_vec();
        self.board.update(cx, |t, cx| {
            *t.delegate_mut() = StringTable::auto(headers, board, false);
            t.refresh(cx);
        });
        cx.notify();
    }

    pub fn run(&mut self, kind: RunKind, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let Some(h) = self.harness_root.clone() else { return };
        let agents: Vec<AgentId> = self.agents.iter().filter(|a| self.selected_agents.contains(*a)).cloned().collect();
        let scenarios: Vec<Scenario> = self.scenarios.iter().filter(|s| self.selected_scenarios.contains(&s.id)).cloned().collect();
        let bench: Vec<BenchTask> = self.bench.iter().filter(|b| self.selected_bench.contains(&b.id)).cloned().collect();
        let n = agents.len() * if kind == RunKind::Conformance { scenarios.len() } else { bench.len() };
        if n == 0 {
            return;
        }
        self.running = Some(kind);
        self.total = n;
        self.results.retain(|r| r.kind != kind);
        self.current = None;
        let (tx, rx) = async_channel::unbounded::<Progress>();
        runtime::tokio().spawn(async move {
            let runner: Arc<dyn AgentRunner> = Arc::new(AcpRunner::detect());
            let opts = RunOptions::new(&h, "acp");
            let mut results = vec![];
            for a in &agents {
                if kind == RunKind::Conformance {
                    for s in &scenarios {
                        let _ = tx.send(Progress::Started(format!("{a} / {}", s.id))).await;
                        let r = tandem_harness::run::run_scenario(a, s, runner.as_ref(), &opts).await;
                        results.push(r.clone());
                        let _ = tx.send(Progress::Result(r)).await;
                    }
                } else {
                    for t in &bench {
                        let _ = tx.send(Progress::Started(format!("{a} / {}", t.id))).await;
                        let r = tandem_harness::run::run_bench_task(a, t, runner.as_ref(), 0, &opts).await;
                        results.push(r.clone());
                        let _ = tx.send(Progress::Result(r)).await;
                    }
                }
            }
            let report = Report::new(&opts.date, &opts.runner_name, results);
            let out = opts.out_dir.clone();
            let written = tokio::task::spawn_blocking(move || report.write(&out).map(|r| (r, out))).await;
            let done = match written {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Progress::Finished(done)).await;
        });
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let stop = matches!(msg, Progress::Finished(_));
                if this.update(cx, |this, cx| this.on_progress(msg, cx)).is_err() || stop {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn on_progress(&mut self, msg: Progress, cx: &mut Context<Self>) {
        match msg {
            Progress::Started(label) => self.current = Some(label),
            Progress::Result(r) => {
                self.results.push(r);
                self.refresh_tables(cx);
            }
            Progress::Finished(r) => {
                self.running = None;
                self.current = None;
                match r {
                    Ok((report, dir)) => {
                        self.results = report.results.clone();
                        self.set_report(report, dir, cx);
                    }
                    Err(e) => self.load_error = Some(format!("report not written: {e}")),
                }
            }
        }
        cx.notify();
    }

    fn confirm_bench(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agents = self.agents.iter().filter(|a| self.selected_agents.contains(*a)).count();
        let tasks = self.bench.iter().filter(|b| self.selected_bench.contains(&b.id)).count();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let view = view.clone();
            let p = pal(cx).clone();
            dialog
                .title("Run the repo benchmark?")
                .w(px(460.))
                .child(div().text_size(px(13.)).text_color(p.text).child(format!(
                    "{tasks} task(s) × {agents} agent(s) = {} runs on your own subscriptions. Each run is a full agent session and uses real quota.",
                    tasks * agents
                )))
                .footer(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .child(Button::new("bench-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("bench-go").small().primary().label("Run benchmark").on_click(move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| this.run(RunKind::Bench, cx));
                            window.close_dialog(cx);
                        })),
                )
        });
    }

    fn chips<'a>(&self, prefix: &'static str, items: impl Iterator<Item = (String, String, bool)> + 'a, p: &Palette, cx: &mut Context<Self>, toggle: fn(&mut Self, &str)) -> Vec<AnyElement> {
        items
            .enumerate()
            .map(|(i, (id, label, on))| {
                div()
                    .id((prefix, i))
                    .px(px(8.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .rounded(px(11.))
                    .border_1()
                    .border_color(if on { p.accent } else { p.border })
                    .bg(if on { p.accent.opacity(0.18) } else { p.panel_bg })
                    .text_size(px(12.))
                    .text_color(if on { p.text_bright } else { p.muted })
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        toggle(this, &id);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

fn toggle_set(set: &mut HashSet<String>, id: &str) {
    if !set.remove(id) {
        set.insert(id.to_string());
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

impl Render for HarnessPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let Some(h) = self.harness_root.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(p.muted)
                .child(icon(IconName::Trophy, 28., p.muted))
                .child("No harness/ folder in this project")
                .child(div().text_size(px(12.)).child("Add harness/scenarios/*.yaml (and harness/bench/*.yaml), or set TANDEM_HARNESS_DIR."))
                .into_any_element();
        };
        let agent_chips = self.chips(
            "h-agent",
            self.agents.iter().map(|a| (a.to_string(), agent_badge(a).2.to_string(), self.selected_agents.contains(a))).collect::<Vec<_>>().into_iter(),
            &p,
            cx,
            |this, id| {
                let a = AgentId::from(id);
                if !this.selected_agents.remove(&a) {
                    this.selected_agents.insert(a);
                }
            },
        );
        let scen_chips = self.chips(
            "h-scen",
            self.scenarios.iter().map(|s| (s.id.clone(), s.id.clone(), self.selected_scenarios.contains(&s.id))).collect::<Vec<_>>().into_iter(),
            &p,
            cx,
            |this, id| toggle_set(&mut this.selected_scenarios, id),
        );
        let bench_chips = self.chips(
            "h-bench",
            self.bench.iter().map(|b| (b.id.clone(), b.id.clone(), self.selected_bench.contains(&b.id))).collect::<Vec<_>>().into_iter(),
            &p,
            cx,
            |this, id| toggle_set(&mut this.selected_bench, id),
        );
        let running = self.running.is_some();
        let done = self.results.len();
        let progress = self.running.map(|k| {
            format!("{} {}/{} · {}", if k == RunKind::Conformance { "Conformance" } else { "Bench" }, done.min(self.total), self.total, self.current.clone().unwrap_or_default())
        });
        let report_md = self.report.as_ref().map(|r| r.to_markdown());
        let dir = self.report_dir.clone();
        let label_row = |label: &str, chips: Vec<AnyElement>| {
            h_flex().gap(px(6.)).flex_wrap().child(div().w(px(76.)).flex_none().text_size(px(11.)).text_color(p.muted).child(label.to_string())).children(chips)
        };
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
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(p.muted).whitespace_nowrap().overflow_hidden().text_ellipsis().child(match &self.report {
                        Some(r) => format!("last report {} · {} runs · {}", r.date, r.results.len(), h.display()),
                        None => format!("{}", h.display()),
                    }))
                    .child(
                        Button::new("run-conformance")
                            .small()
                            .primary()
                            .loading(self.running == Some(RunKind::Conformance))
                            .disabled(running)
                            .label("Run conformance")
                            .on_click(cx.listener(|this, _, _, cx| this.run(RunKind::Conformance, cx))),
                    )
                    .child(Button::new("run-bench").small().loading(self.running == Some(RunKind::Bench)).disabled(running).label("Run bench…").on_click(cx.listener(|this, _, window, cx| this.confirm_bench(window, cx))))
                    .child(Button::new("copy-md").small().ghost().label("Copy markdown").disabled(report_md.is_none()).on_click(move |_, _, cx| {
                        if let Some(md) = &report_md {
                            cx.write_to_clipboard(ClipboardItem::new_string(md.clone()));
                        }
                    }))
                    .child(Button::new("open-reports").small().ghost().label("Open report folder").disabled(dir.is_none()).on_click(move |_, _, _| {
                        if let Some(d) = &dir {
                            let _ = std::process::Command::new("open").arg(d).spawn();
                        }
                    })),
            )
            .child(
                v_flex()
                    .flex_none()
                    .px(px(10.))
                    .py(px(6.))
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(label_row("AGENTS", agent_chips))
                    .child(label_row("SCENARIOS", scen_chips))
                    .when(!bench_chips.is_empty(), |d| d.child(label_row("BENCH", bench_chips)))
                    .children(progress.map(|s| {
                        let frac = if self.total > 0 { done.min(self.total) as f32 / self.total as f32 } else { 0. };
                        v_flex()
                            .gap(px(3.))
                            .child(div().text_size(px(12.)).text_color(p.text).child(s))
                            .child(div().h(px(4.)).w_full().rounded(px(2.)).bg(p.border).child(div().h_full().rounded(px(2.)).bg(p.accent).w(relative(frac))))
                    }))
                    .children(self.load_error.clone().map(|e| div().text_size(px(12.)).text_color(p.error).child(e))),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(div().px(px(10.)).h(px(22.)).flex().items_center().text_size(px(11.)).text_color(p.muted).child("RESULTS"))
                            .child(div().flex_1().min_h_0().child(DataTable::new(&self.matrix).stripe(false).bordered(false))),
                    )
                    .child(
                        v_flex()
                            .w(px(520.))
                            .h_full()
                            .flex_none()
                            .border_l_1()
                            .border_color(p.border)
                            .child(div().px(px(10.)).h(px(22.)).flex().items_center().text_size(px(11.)).text_color(p.muted).child("LEADERBOARD"))
                            .child(div().h(px(160.)).flex_none().child(DataTable::new(&self.board).stripe(false).bordered(false)))
                            .children(self.svg.clone().map(|svg| div().flex_1().min_h_0().p(px(8.)).child(img(svg).w_full().h_full().object_fit(ObjectFit::Contain)))),
                    ),
            )
            .into_any_element()
    }
}
