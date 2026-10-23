//! “状态”面板：会话所在机器的 CPU、内存、负载、网络、磁盘与占用最高的进程。
//!
//! 数据与走势按机器保留：本地会话共用本机的一份，同一主机的远程会话共用一份，在窗格间切换时走势连续。
//! 窗口里有活动会话的机器都持续采集，与面板是否可见无关，切过去时走势已经就绪；主机设置里关闭监控的除外。
//! 远程采集借用其中一个已连接会话的连接，该会话关闭、断开或重连后，下次同步时改由同机器的其他会话重新开始。
//! 版式按面板自身的尺寸调整：宽时进程列表加命令行一列，高时 CPU、内存多一条当前值指示条并列出更多进程。

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
    SharedString, Size, StatefulInteractiveElement, Styled, Task, Window, canvas, div,
    prelude::FluentBuilder, px, size,
};
use rust_i18n::t;
use scell_config::HostId;
use scell_session::{Metrics, MetricsUpdate, SessionEvent, SessionHandle, SessionId, Usage};

use super::{ContextItem, PanelTarget, Source};
use crate::config::ConfigState;
use crate::files::format_size;

/// 走势保留的采样数（每 2 秒一次，约两分钟）。
const HISTORY: usize = 60;
/// 面板宽于此值时进程列表显示命令行。
const WIDE: Pixels = px(340.);
/// 面板高于此值时多显示当前值指示条与更多进程。
const TALL: Pixels = px(720.);
/// 进程列表的行数：默认与最多。
const PROCESSES_MIN: usize = 5;
const PROCESSES_MAX: usize = 15;
const PROCESS_ROW: Pixels = px(22.);

#[derive(Default)]
enum State {
    #[default]
    Waiting,
    Sample(Box<Metrics>),
    Unsupported(String),
    Failed(String),
    /// 主机设置里关闭了监控。
    Disabled,
}

/// 被采集的机器。
#[derive(Clone, PartialEq, Eq, Hash)]
enum Machine {
    Local,
    Host(HostId),
}

impl Machine {
    fn of_host(host: Option<&HostId>) -> Self {
        host.cloned().map_or(Self::Local, Self::Host)
    }
}

/// 一台机器的采集与历史。历史中的 `None` 标记采集中断过（断线、会话全部关闭后重新开始）。
#[derive(Default)]
struct Watched {
    state: State,
    /// 采集所借用的会话。
    via: Option<SessionId>,
    cpu: VecDeque<Option<f32>>,
    memory: VecDeque<Option<f32>>,
    /// （下行，上行）字节每秒。
    net: VecDeque<Option<(f64, f64)>>,
    /// 持有采集；释放即停止。
    watch: Option<Task<()>>,
}

impl Watched {
    fn record(&mut self, m: &Metrics) {
        if let Some(cpu) = m.cpu {
            push(&mut self.cpu, Some(cpu));
        }
        push(&mut self.memory, Some(m.memory.ratio()));
        if let (Some(rx), Some(tx)) = (m.net_rx, m.net_tx) {
            push(&mut self.net, Some((rx, tx)));
        }
    }

    /// 重新开始采集前在已有走势末尾记一个中断。
    fn mark_gap(&mut self) {
        fn gap<T>(history: &mut VecDeque<Option<T>>) {
            if history.back().is_some_and(Option::is_some) {
                push(history, None);
            }
        }
        gap(&mut self.cpu);
        gap(&mut self.memory);
        gap(&mut self.net);
    }
}

fn push<T>(history: &mut VecDeque<T>, value: T) {
    if history.len() == HISTORY {
        history.pop_front();
    }
    history.push_back(value);
}

#[derive(Default)]
pub(crate) struct StatusPanel {
    current: Option<Machine>,
    machines: HashMap<Machine, Watched>,
    /// 上一帧面板内容区的尺寸，决定版式。
    size: Rc<Cell<Size<Pixels>>>,
}

impl StatusPanel {
    fn start(&mut self, machine: Machine, session: &SessionHandle, cx: &mut Context<Self>) {
        let watch = session.watch_metrics();
        let key = machine.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Some(update) = watch.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(w) = this.machines.get_mut(&key) else {
                        return;
                    };
                    w.state = match update {
                        // 重新开始采集后的第一帧还算不出 CPU 与网速，先留着上一帧，不闪成“—”
                        MetricsUpdate::Sample(m)
                            if m.cpu.is_none()
                                && matches!(&w.state, State::Sample(prev) if prev.cpu.is_some()) =>
                        {
                            return;
                        }
                        MetricsUpdate::Sample(m) => {
                            w.record(&m);
                            State::Sample(Box::new(m))
                        }
                        MetricsUpdate::Unsupported(os) => State::Unsupported(os),
                        MetricsUpdate::Failed(e) => State::Failed(e),
                        _ => return,
                    };
                    if this.current.as_ref() == Some(&key) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let w = self.machines.entry(machine).or_default();
        if !matches!(w.state, State::Sample(_)) {
            w.state = State::Waiting;
        }
        w.mark_gap();
        w.via = Some(session.id());
        w.watch = Some(task);
    }
}

impl StatusPanel {
    /// 按窗口里的全部会话补齐采集：没在采的机器选一个已连接的会话开始，来源会话不再可用的换一个，
    /// 没有会话的机器停止（数据保留）。由工作区定时调用，只在有变化时做事。
    pub(crate) fn sync(&mut self, sources: &[Source], cx: &mut Context<Self>) {
        let mut seen = Vec::new();
        for source in sources {
            let machine = Machine::of_host(source.host.as_ref());
            if seen.contains(&machine) {
                continue;
            }
            seen.push(machine.clone());
            let enabled = source
                .host
                .as_ref()
                .and_then(|h| ConfigState::hosts(cx).get(h))
                .is_none_or(|h| h.monitor);
            let w = self.machines.entry(machine.clone()).or_default();
            if !enabled {
                if !matches!(w.state, State::Disabled) {
                    w.via = None;
                    w.watch = None;
                    w.state = State::Disabled;
                    cx.notify();
                }
                continue;
            }
            if matches!(w.state, State::Disabled) {
                w.state = State::Waiting;
                cx.notify();
            }
            let live = |s: &&Source| s.live && Machine::of_host(s.host.as_ref()) == machine;
            if w.watch.is_some()
                && sources
                    .iter()
                    .filter(live)
                    .any(|s| Some(s.session.id()) == w.via)
            {
                continue;
            }
            match sources.iter().find(live) {
                Some(s) => {
                    self.start(machine, &s.session, cx);
                    cx.notify();
                }
                None if w.watch.is_some() => {
                    w.via = None;
                    w.watch = None;
                }
                None => {}
            }
        }
        for (machine, w) in &mut self.machines {
            if !seen.contains(machine) {
                w.via = None;
                w.watch = None;
            }
        }
    }

    fn stop_via(&mut self, session: SessionId) {
        for w in self.machines.values_mut() {
            if w.via == Some(session) {
                w.via = None;
                w.watch = None;
            }
        }
    }
}

impl ContextItem for StatusPanel {
    fn title(&self) -> SharedString {
        t!("panel.status").into()
    }

    fn follow(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>) {
        let machine = target.map(|t| Machine::of_host(t.host.as_ref()));
        if self.current != machine {
            self.current = machine;
            cx.notify();
        }
    }

    /// 远程会话连上前无法采集，连上（或重连）后在下次同步时重新开始。
    fn session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        if let SessionEvent::Connected(id) = event {
            self.stop_via(*id);
            cx.notify();
        }
    }

    /// 机器的数据留着，其他会话还可能在看；借用这个会话的采集停下，下次同步时换会话重新开始。
    fn release(&mut self, session: SessionId, _: &mut Context<Self>) {
        self.stop_via(session);
    }
}

// ---- 渲染 ----

fn percent(ratio: f32) -> String {
    format!("{:.0}%", ratio * 100.)
}

fn uptime(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    let (days, hours, mins) = (mins / 1440, mins / 60 % 24, mins % 60);
    if days > 0 {
        t!("status.uptime_days", days = days, hours = hours).into_owned()
    } else {
        t!("status.uptime_hours", hours = hours, mins = mins).into_owned()
    }
}

fn rate(bytes: Option<f64>) -> String {
    match bytes {
        Some(b) => format!("{}/s", format_size(b as u64)),
        None => "—".into(),
    }
}

fn usage(u: Usage) -> String {
    format!("{} / {}", format_size(u.used), format_size(u.total))
}

/// 各项指标的颜色，占用高时改为警示色。
struct Palette {
    cpu: Hsla,
    memory: Hsla,
    swap: Hsla,
    down: Hsla,
    up: Hsla,
    disk: Hsla,
    warning: Hsla,
    danger: Hsla,
    muted: Hsla,
    track: Hsla,
}

impl Palette {
    fn new(cx: &App) -> Self {
        let t = cx.theme();
        Self {
            cpu: t.blue,
            memory: t.magenta,
            swap: t.magenta_light,
            down: t.green,
            up: t.yellow,
            disk: t.cyan,
            warning: t.warning,
            danger: t.danger,
            muted: t.muted_foreground,
            track: t.secondary,
        }
    }

    fn level(&self, base: Hsla, ratio: f32) -> Hsla {
        if ratio >= 0.9 {
            self.danger
        } else if ratio >= 0.8 {
            self.warning
        } else {
            base
        }
    }
}

/// 一项指标：带色点的标签与数值在一行（放不下时标签省略），下面是条、走势或细节。
fn metric(
    label: SharedString,
    color: Option<Hsla>,
    value: Option<String>,
    below: Vec<AnyElement>,
    p: &Palette,
) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_1p5()
                .text_xs()
                .children(color.map(|c| div().flex_none().size(px(8.)).rounded_full().bg(c)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(p.muted)
                        .child(label),
                )
                .children(value.map(|v| {
                    div()
                        .flex_none()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(v)
                })),
        )
        .children(below)
        .into_any_element()
}

fn bar(id: impl Into<SharedString>, ratio: f32, color: Hsla) -> AnyElement {
    let id: SharedString = id.into();
    Progress::new(id)
        .value(ratio * 100.)
        .color(color)
        .h(px(6.))
        .into_any_element()
}

fn detail(text: String, p: &Palette) -> AnyElement {
    div()
        .text_xs()
        .truncate()
        .text_color(p.muted)
        .child(text)
        .into_any_element()
}

/// 走势：每次采样一根柱，最新的在右，不足一屏时左侧留空。每根柱由若干段自下而上堆叠；
/// `None` 为采集中断处，画一条全高的红色窄条。
fn sparkline(columns: Vec<Option<Vec<(f32, Hsla)>>>, max: f32, p: &Palette) -> AnyElement {
    let max = max.max(f32::EPSILON);
    let empty = HISTORY.saturating_sub(columns.len());
    h_flex()
        .h(px(32.))
        .w_full()
        .items_end()
        .gap(px(1.))
        .children((0..empty).map(|_| div().flex_1().h_full().into_any_element()))
        .children(columns.into_iter().map(|segments| {
            let Some(segments) = segments else {
                return div()
                    .flex_none()
                    .w(px(2.))
                    .h_full()
                    .bg(p.danger)
                    .into_any_element();
            };
            let total: f32 = segments.iter().map(|(v, _)| v).sum();
            let column = v_flex().flex_1().h_full().justify_end();
            if total <= 0. {
                return column.child(div().h(px(1.)).bg(p.track)).into_any_element();
            }
            column
                .children(segments.into_iter().rev().map(|(v, color)| {
                    div()
                        .w_full()
                        .h(gpui_kit::relative((v / max).clamp(0., 1.)))
                        .min_h(px(1.))
                        .bg(color)
                }))
                .into_any_element()
        }))
        .into_any_element()
}

fn section(title: SharedString) -> AnyElement {
    div()
        .pt_2()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .child(title)
        .into_any_element()
}

impl StatusPanel {
    fn render_metrics(&self, w: &Watched, m: &Metrics, cx: &App) -> AnyElement {
        let p = Palette::new(cx);
        let area = self.size.get();
        let wide = area.width >= WIDE;
        let tall = area.height >= TALL;
        let cpu = m.cpu.unwrap_or(0.);

        let mut cpu_below = vec![sparkline(
            w.cpu
                .iter()
                .map(|v| v.map(|v| vec![(v, p.level(p.cpu, v))]))
                .collect(),
            1.,
            &p,
        )];
        if tall {
            cpu_below.push(bar("cpu", cpu, p.level(p.cpu, cpu)));
        }
        let mut mem_below = Vec::new();
        if tall {
            mem_below.push(sparkline(
                w.memory
                    .iter()
                    .map(|v| v.map(|v| vec![(v, p.level(p.memory, v))]))
                    .collect(),
                1.,
                &p,
            ));
        }
        mem_below.push(bar(
            "mem",
            m.memory.ratio(),
            p.level(p.memory, m.memory.ratio()),
        ));
        mem_below.push(detail(usage(m.memory), &p));

        let mut rows = vec![
            metric(
                "CPU".into(),
                Some(p.cpu),
                Some(m.cpu.map(percent).unwrap_or_else(|| "—".into())),
                cpu_below,
                &p,
            ),
            metric(
                t!("status.memory").into(),
                Some(p.memory),
                Some(percent(m.memory.ratio())),
                mem_below,
                &p,
            ),
        ];
        if m.swap.total > 0 {
            rows.push(metric(
                t!("status.swap").into(),
                Some(p.swap),
                Some(percent(m.swap.ratio())),
                vec![
                    bar("swap", m.swap.ratio(), p.level(p.swap, m.swap.ratio())),
                    detail(usage(m.swap), &p),
                ],
                &p,
            ));
        }
        let net_max = w
            .net
            .iter()
            .flatten()
            .map(|(r, t)| (r + t) as f32)
            .fold(0f32, f32::max);
        let rate_cell = |arrow: &str, value: Option<f64>, color: Hsla| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(div().flex_none().text_color(color).child(arrow.to_owned()))
                .child(div().min_w_0().truncate().child(rate(value)))
        };
        rows.push(metric(
            t!("status.network").into(),
            None,
            None,
            vec![
                sparkline(
                    w.net
                        .iter()
                        .map(|v| v.map(|(r, t)| vec![(r as f32, p.down), (t as f32, p.up)]))
                        .collect(),
                    net_max,
                    &p,
                ),
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(rate_cell("↓", m.net_rx, p.down))
                    .child(rate_cell("↑", m.net_tx, p.up))
                    .into_any_element(),
            ],
            &p,
        ));
        if let Some([a, b, c]) = m.load {
            rows.push(metric(
                t!("status.load").into(),
                None,
                Some(format!("{a:.2} {b:.2} {c:.2}")),
                Vec::new(),
                &p,
            ));
        }
        rows.push(metric(
            t!("status.uptime").into(),
            None,
            Some(uptime(m.uptime)),
            Vec::new(),
            &p,
        ));
        if !m.processes.is_empty() {
            rows.push(section(t!("status.processes").into()));
            rows.push(self.processes(m, wide, tall, &p));
        }
        if !m.disks.is_empty() {
            rows.push(section(t!("status.disks").into()));
        }
        for (i, d) in m.disks.iter().enumerate() {
            let ratio = d.usage.ratio();
            rows.push(metric(
                d.mount.clone().into(),
                None,
                Some(percent(ratio)),
                vec![
                    bar(format!("disk-{i}"), ratio, p.level(p.disk, ratio)),
                    detail(usage(d.usage), &p),
                ],
                &p,
            ));
        }
        v_flex().px_3().gap_3().children(rows).into_any_element()
    }

    /// 进程表：名称、（宽时）命令行、CPU、内存。高时按剩余高度多列几行。
    fn processes(&self, m: &Metrics, wide: bool, tall: bool, p: &Palette) -> AnyElement {
        let count = if tall {
            let extra = ((self.size.get().height - TALL) / PROCESS_ROW).max(0.) as usize;
            (PROCESSES_MIN + extra).min(PROCESSES_MAX)
        } else {
            PROCESSES_MIN
        };
        let cell = |w: Pixels| div().flex_none().w(w).flex().justify_end();
        let name = |wide: bool| {
            div()
                .truncate()
                .when(wide, |d| d.w(px(110.)).flex_none())
                .when(!wide, |d| d.flex_1().min_w_0())
        };
        let header = h_flex()
            .gap_2()
            .text_xs()
            .text_color(p.muted)
            .child(name(wide).child(t!("status.process_name").to_string()))
            .when(wide, |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(t!("status.command").to_string()),
                )
            })
            .child(cell(px(40.)).child("CPU"))
            .child(cell(px(56.)).child(t!("status.memory").to_string()));
        let rows = m.processes.iter().take(count).map(|proc| {
            let busy = proc.cpu >= 0.5;
            h_flex()
                .h(PROCESS_ROW)
                .gap_2()
                .text_xs()
                .child(name(wide).child(proc.name.clone()))
                .when(wide, |el| {
                    el.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .child(proc.command.clone()),
                    )
                })
                .child(
                    cell(px(40.))
                        .text_color(if busy {
                            p.level(p.cpu, proc.cpu.min(1.))
                        } else {
                            p.muted
                        })
                        .when(busy, |d| d.font_weight(gpui_kit::FontWeight::MEDIUM))
                        .child(percent(proc.cpu)),
                )
                .child(
                    cell(px(56.))
                        .text_color(p.muted)
                        .child(format_size(proc.memory)),
                )
                .into_any_element()
        });
        v_flex().child(header).children(rows).into_any_element()
    }
}

impl Render for StatusPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let note = |text: String| {
            div()
                .px_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let body = match self.current.as_ref().and_then(|m| self.machines.get(m)) {
            None => div().into_any_element(),
            Some(w) => match &w.state {
                State::Waiting => note(t!("status.waiting").into_owned()),
                State::Unsupported(os) => note(t!("status.unsupported", os = os).into_owned()),
                State::Failed(e) => note(e.clone()),
                State::Disabled => note(t!("status.disabled").into_owned()),
                State::Sample(m) => self.render_metrics(w, m, cx),
            },
        };
        // 记下可见区域的尺寸，变化时重绘一次以切换版式
        let measured = self.size.clone();
        let id = cx.entity_id();
        let probe = canvas(
            move |bounds, _, cx| {
                let s = size(bounds.size.width, bounds.size.height);
                if measured.get() != s {
                    measured.set(s);
                    cx.notify(id);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div().relative().size_full().child(probe).child(
            div()
                .id("status-panel")
                .size_full()
                .overflow_y_scroll()
                .pb_3()
                .child(body),
        )
    }
}
