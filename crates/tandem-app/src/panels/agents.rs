//! Agents cockpit: quota pool, task list, streaming transcript with tool-call and permission
//! cards, prompt composer with routing and the global kill switch.
//! Integration points: `AgentsModel::apply(TaskEvent)` (tandem-acp), `AgentsModel::quota`
//! (tandem-quota), `AgentsEvent::Decide` → `tandem_acp::Session::respond(PermissionDecision)`.

use crate::assets::icon;
use crate::keymap::{NotImplemented, StopAllAgents};
use crate::model::agents::{AgentTask, AgentsModel, TaskStatus, agent_badge};
use crate::panels::chip;
use crate::theme::{MONO_FONT, Palette, hex, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use tandem_core::{AgentEvent, Decider, PermissionDecision, PermissionRequest, RiskClass, Role, TaskPhase, ToolCall, ToolKind, ToolStatus};

pub enum AgentsEvent {
    Decide(PermissionDecision),
    Toast(SharedString),
}

const SKILLS: &[&str] = &["Lazy", "Terse", "Disciplined", "Ship"];

pub struct AgentsPanel {
    pub model: AgentsModel,
    composer: Entity<TextareaState>,
    agent: usize,
    phase: TaskPhase,
    skills: [bool; 4],
    route_for_me: bool,
}

impl EventEmitter<AgentsEvent> for AgentsPanel {}

const AGENTS: &[&str] = &["claude", "codex", "gemini", "opencode", "goose"];

impl AgentsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder("Describe a task for an agent…  (@ files, # symbols)"));
        Self { model: AgentsModel::fake(), composer, agent: 0, phase: TaskPhase::Implement, skills: [true, false, true, false], route_for_me: true }
    }

    fn decide(&mut self, task_ix: usize, req: &PermissionRequest, option_id: &str, label: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.model.tasks.get_mut(task_ix) {
            t.events.retain(|e| !matches!(e, AgentEvent::PermissionRequested(r) if r.id == req.id));
            t.events.push(AgentEvent::PermissionResolved { request: req.id.clone(), option: label.to_string(), by: Decider::User });
            // Allowed or rejected, the agent resumes its turn.
            t.status = TaskStatus::Running;
        }
        cx.emit(AgentsEvent::Decide(PermissionDecision { request: req.id.clone(), option_id: option_id.into() }));
        cx.emit(AgentsEvent::Toast(format!("Permission: {label}").into()));
        cx.notify();
    }

    fn render_quota(&self, p: &Palette) -> impl IntoElement {
        let bar = |label: &str, frac: Option<f64>| {
            let pct = frac.map(|f| format!("{label} {}%", (f * 100.0).round() as i64)).unwrap_or_else(|| format!("{label} unknown"));
            let color = match frac {
                Some(f) if f > 0.8 => p.error,
                Some(f) if f > 0.5 => p.warning,
                Some(_) => p.ok,
                None => p.muted,
            };
            v_flex()
                .gap(px(3.))
                .w(px(92.))
                .child(div().text_size(px(11.)).text_color(if frac.is_some() { p.text } else { p.muted }).child(pct))
                .child(
                    div().h(px(4.)).w_full().rounded(px(2.)).bg(p.border).child(
                        div().h_full().rounded(px(2.)).bg(color).w(relative(frac.unwrap_or(0.0) as f32)),
                    ),
                )
        };
        let plan = |agent: &str, name: &str, windows: Vec<(String, Option<f64>)>| {
            let (letter, color, _) = agent_badge(&agent.into());
            h_flex()
                .gap(px(8.))
                .child(avatar(letter, color, 20.))
                .child(
                    v_flex()
                        .gap(px(2.))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name.to_string()))
                        .child(h_flex().gap(px(8.)).children(windows.into_iter().map(|(l, f)| bar(&l, f)))),
                )
        };
        let windows_for = |agent: &str| -> Vec<(String, Option<f64>)> {
            self.model.quota.iter().filter(|w| w.agent.as_str() == agent).map(|w| (w.label.clone(), w.used_fraction)).collect()
        };
        h_flex()
            .flex_none()
            .px(px(12.))
            .py(px(8.))
            .gap(px(20.))
            .border_b_1()
            .border_color(p.border)
            .child(
                v_flex()
                    .child(div().text_size(px(11.)).text_color(p.muted).child("QUOTA POOL"))
                    .child(div().text_size(px(12.)).text_color(p.text).child(format!("{} agents running", self.model.running_count()))),
            )
            .child(plan("claude", "Claude Max", windows_for("claude")))
            .child(plan("codex", "ChatGPT Pro", windows_for("codex")))
            .child(plan("gemini", "Gemini", windows_for("gemini")))
    }

    fn render_task_row(&self, ix: usize, t: &AgentTask, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (letter, color, _) = agent_badge(&t.agent);
        let selected = self.model.selected == ix;
        let (fg, bg) = status_colors(t.status, p);
        v_flex()
            .id(("task", ix))
            .px(px(10.))
            .py(px(7.))
            .gap(px(4.))
            .rounded(px(6.))
            .mx(px(6.))
            .when(selected, |d| d.bg(p.row_selected))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.selected = ix;
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(avatar(letter, color, 18.))
                    .child(div().flex_1().text_size(px(13.)).text_color(p.text).whitespace_nowrap().overflow_hidden().text_ellipsis().child(t.title.clone())),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .pl(px(26.))
                    .child(chip(t.status.label(), fg, bg))
                    .child(div().text_size(px(11.)).text_color(p.muted).child(t.elapsed.clone())),
            )
    }

    fn render_transcript(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let task_ix = self.model.selected;
        let Some(t) = self.model.tasks.get(task_ix) else { return div().into_any_element() };
        let (letter, color, agent_name) = agent_badge(&t.agent);
        let mut items: Vec<AnyElement> = Vec::new();
        for (i, ev) in t.events.iter().enumerate() {
            match ev {
                AgentEvent::TextDelta { role: Role::User, text } => items.push(
                    div()
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(p.stripe_selected)
                        .text_color(p.text)
                        .child(text.clone())
                        .into_any_element(),
                ),
                AgentEvent::TextDelta { role: Role::Thought, text } => {
                    items.push(div().italic().text_color(p.muted).child(format!("Thinking: {text}")).into_any_element())
                }
                AgentEvent::TextDelta { role: Role::Assistant, text } => items.push(div().text_color(p.text).child(text.clone()).into_any_element()),
                AgentEvent::ToolCall(tc) | AgentEvent::ToolCallUpdate(tc) => items.push(tool_card(tc, p).into_any_element()),
                AgentEvent::PermissionRequested(req) => items.push(self.permission_card(i, task_ix, req, p, cx).into_any_element()),
                AgentEvent::PermissionResolved { option, .. } => items.push(
                    h_flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(p.muted)
                        .child(icon(IconName::ShieldCheck, 14., p.ok))
                        .child(format!("You chose “{option}”"))
                        .into_any_element(),
                ),
                _ => {}
            }
        }
        if t.status == TaskStatus::Running {
            items.push(h_flex().gap(px(6.)).text_color(p.muted).child(div().size(px(6.)).rounded_full().bg(p.accent)).child("Working…").into_any_element());
        }
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .px(px(12.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(avatar(letter, color, 18.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(agent_name))
                    .child(div().text_size(px(12.)).text_color(p.muted).whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!("· {}", t.title)))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(icon(IconName::GitBranch, 12., p.muted))
                            .child(format!(".tandem/worktrees/{}", t.id)),
                    ),
            )
            .child(
                div()
                    .id("transcript")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(12.))
                    .child(v_flex().gap(px(10.)).text_size(px(13.)).children(items)),
            )
            .into_any_element()
    }

    fn permission_card(&self, i: usize, task_ix: usize, req: &PermissionRequest, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let destructive = req.risk == RiskClass::Destructive;
        let edge = if destructive { p.error } else { p.warning };
        let cmd = req.tool_call.raw_input.clone().unwrap_or_else(|| req.tool_call.title.clone());
        let mut buttons = h_flex().gap(px(8.));
        for (k, opt) in req.options.iter().enumerate() {
            let req2 = req.clone();
            let id = opt.id.clone();
            let label = opt.label.clone();
            let mut b = Button::new(("perm", i * 10 + k)).small().label(opt.label.clone());
            b = match opt.kind {
                tandem_core::PermissionOptionKind::AllowOnce => b.primary(),
                tandem_core::PermissionOptionKind::RejectOnce | tandem_core::PermissionOptionKind::RejectAlways => b.danger(),
                _ => b,
            };
            buttons = buttons.child(b.on_click(cx.listener(move |this, _, _, cx| this.decide(task_ix, &req2, &id, &label, cx))));
        }
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(edge)
            .bg(p.danger_bg)
            .overflow_hidden()
            .child(
                h_flex()
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(8.))
                    .bg(edge.opacity(0.18))
                    .child(icon(IconName::ShieldAlert, 16., edge))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(edge).child("Permission required"))
                    .child(div().flex_1())
                    .child(chip(if destructive { "DESTRUCTIVE" } else { "REVIEW" }, hex(0xFFFFFF), edge)),
            )
            .child(
                v_flex()
                    .p(px(10.))
                    .gap(px(8.))
                    .child(div().text_color(p.text).child(req.tool_call.title.clone()))
                    .child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .bg(p.editor_bg)
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .text_color(p.text_bright)
                            .child(format!("$ {cmd}")),
                    )
                    .child(div().text_size(px(12.)).text_color(p.muted).child(req.reason.clone()))
                    .child(buttons),
            )
    }

    fn render_composer(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let agent = AGENTS[self.agent];
        let (letter, color, name) = agent_badge(&agent.into());
        let phase = phase_label(self.phase);
        let skills = SKILLS.iter().enumerate().map(|(i, s)| {
            let on = self.skills[i];
            div()
                .id(("skill", i))
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
                .child(*s)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.skills[i] = !this.skills[i];
                    cx.notify();
                }))
        });
        v_flex()
            .flex_none()
            .border_t_1()
            .border_color(p.border)
            .p(px(10.))
            .gap(px(8.))
            .child(Textarea::new(&self.composer).h(px(64.)))
            .child(
                h_flex()
                    .gap(px(6.))
                    .flex_wrap()
                    .child(
                        Button::new("agent-picker")
                            .small()
                            .ghost()
                            .child(h_flex().gap(px(6.)).child(avatar(letter, color, 16.)).child(name).child(icon(IconName::ChevronDown, 12., p.muted)))
                            .dropdown_menu({
                                let view = cx.entity();
                                move |mut menu, _, _| {
                                    for (i, a) in AGENTS.iter().enumerate() {
                                        let (_, _, n) = agent_badge(&(*a).into());
                                        let view = view.clone();
                                        menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(n).on_click(move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.agent = i;
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }
                            }),
                    )
                    .child(
                        Button::new("phase-picker")
                            .small()
                            .ghost()
                            .child(h_flex().gap(px(6.)).child(div().text_color(p.muted).child("Phase:")).child(phase).child(icon(IconName::ChevronDown, 12., p.muted)))
                            .dropdown_menu({
                                let view = cx.entity();
                                move |mut menu, _, _| {
                                    for ph in [TaskPhase::Plan, TaskPhase::Implement, TaskPhase::Test, TaskPhase::Review, TaskPhase::Any] {
                                        let view = view.clone();
                                        menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(phase_label(ph)).on_click(move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.phase = ph;
                                                cx.notify();
                                            });
                                        }));
                                    }
                                    menu
                                }
                            }),
                    )
                    .children(skills),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Switch::new("route-for-me").checked(self.route_for_me).label("Route for me").on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.route_for_me = *v;
                            cx.notify();
                        })),
                    )
                    .child(div().text_size(px(11.)).text_color(p.muted).child("est. ~3% of Claude 5h"))
                    .child(div().flex_1())
                    .child(Button::new("send").small().primary().label("Start task").on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(NotImplemented("Start agent task".into())), cx)
                    }))
                    .child(
                        div()
                            .id("stop-all")
                            .h(px(28.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(5.))
                            .bg(hex(0xDB3B4B))
                            .hover(|s| s.bg(hex(0xE5505F)))
                            .text_color(hex(0xFFFFFF))
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(12.))
                            .cursor_pointer()
                            .child(icon(IconName::OctagonX, 14., hex(0xFFFFFF)))
                            .child("STOP ALL")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(StopAllAgents), cx)),
                    ),
            )
    }
}

fn phase_label(ph: TaskPhase) -> &'static str {
    match ph {
        TaskPhase::Plan => "Plan",
        TaskPhase::Implement => "Implement",
        TaskPhase::Test => "Test",
        TaskPhase::Review => "Review",
        TaskPhase::Any => "Any",
    }
}

fn status_colors(s: TaskStatus, p: &Palette) -> (Hsla, Hsla) {
    match s {
        TaskStatus::Running => (p.accent, p.accent.opacity(0.18)),
        TaskStatus::NeedsApproval => (p.error, p.error.opacity(0.18)),
        TaskStatus::Done => (p.ok, p.ok.opacity(0.18)),
        TaskStatus::Failed => (p.muted, p.muted.opacity(0.2)),
    }
}

pub fn avatar(letter: &'static str, color: u32, size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(hex(color))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.5))
        .font_weight(FontWeight::BOLD)
        .text_color(hex(0xFFFFFF))
        .child(letter)
}

fn tool_card(tc: &ToolCall, p: &Palette) -> impl IntoElement {
    let glyph = match tc.kind {
        ToolKind::Read => IconName::FileText,
        ToolKind::Edit => IconName::FileCode,
        ToolKind::Search => IconName::Search,
        ToolKind::Execute => IconName::Terminal,
        ToolKind::Fetch => IconName::Zap,
        _ => IconName::Sparkles,
    };
    let (status_icon, status_color) = match tc.status {
        ToolStatus::Completed => (IconName::Check, p.ok),
        ToolStatus::Failed => (IconName::X, p.error),
        _ => (IconName::CircleDot, p.accent),
    };
    h_flex()
        .px(px(10.))
        .h(px(30.))
        .gap(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(p.border)
        .bg(p.card_bg)
        .child(icon(glyph, 14., p.muted))
        .child(div().flex_1().text_size(px(12.)).text_color(p.text).child(tc.title.clone()))
        .child(icon(status_icon, 14., status_color))
}

impl Render for AgentsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let tasks: Vec<AnyElement> = self
            .model
            .tasks
            .clone()
            .iter()
            .enumerate()
            .map(|(i, t)| self.render_task_row(i, t, &p, cx).into_any_element())
            .collect();
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(self.render_quota(&p))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        v_flex()
                            .id("tasks")
                            .w(px(230.))
                            .h_full()
                            .flex_none()
                            .border_r_1()
                            .border_color(p.border)
                            .py(px(6.))
                            .gap(px(2.))
                            .overflow_y_scroll()
                            .child(
                                h_flex()
                                    .px(px(12.))
                                    .h(px(22.))
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child("TASKS")
                                    .child(div().flex_1())
                                    .child(icon(IconName::Plus, 14., p.muted)),
                            )
                            .children(tasks),
                    )
                    .child(div().flex_1().h_full().min_w_0().bg(p.editor_bg).child(self.render_transcript(&p, cx))),
            )
            .child(self.render_composer(&p, cx))
    }
}
