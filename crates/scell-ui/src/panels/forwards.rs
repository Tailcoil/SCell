//! “转发”面板：当前连接上的端口转发（主机配置的与临时添加的），显示状态与流量，可临时增删。
//! 转发属于连接，同一主机的终端共用；临时添加的在连接断开后不再恢复，可保存到主机配置。

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use rust_i18n::t;
use scell_config::HostId;
use scell_config::hosts::{ForwardKind, ForwardRule};
use scell_session::{ForwardId, ForwardStatus, SessionEvent, SessionHandle};

use super::{ContextItem, PanelTarget};
use crate::config::{self, ConfigState};
use crate::files::format_size;
use crate::ui::form;

/// 添加规则的表单。输入框要在有窗口时创建，首次渲染时才建。
struct AddForm {
    kind: ForwardKind,
    listen: Entity<InputState>,
    target: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
}

pub(crate) struct ForwardsPanel {
    session: Option<SessionHandle>,
    host: Option<HostId>,
    list: Vec<ForwardStatus>,
    /// 各转发上次的累计流量与时刻，用于算速率。
    last: HashMap<ForwardId, (u64, u64, Instant)>,
    rates: HashMap<ForwardId, (f64, f64)>,
    form: Option<AddForm>,
}

impl ForwardsPanel {
    pub(crate) fn new(_: &mut Context<Self>) -> Self {
        Self {
            session: None,
            host: None,
            list: Vec::new(),
            last: HashMap::new(),
            rates: HashMap::new(),
            form: None,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let list = self
            .session
            .as_ref()
            .map(SessionHandle::forwards)
            .unwrap_or_default();
        let now = Instant::now();
        for f in &list {
            let entry = self.last.entry(f.id).or_insert((f.sent, f.received, now));
            let secs = now.duration_since(entry.2).as_secs_f64();
            // 刷新可能比一秒频繁（每次重绘），速率按至少一秒的间隔计算
            if secs >= 0.9 {
                let rate = (
                    f.sent.saturating_sub(entry.0) as f64 / secs,
                    f.received.saturating_sub(entry.1) as f64 / secs,
                );
                self.rates.insert(f.id, rate);
                *entry = (f.sent, f.received, now);
            }
        }
        self.last.retain(|id, _| list.iter().any(|f| f.id == *id));
        self.rates.retain(|id, _| list.iter().any(|f| f.id == *id));
        if list != self.list {
            self.list = list;
            cx.notify();
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(form)) = (self.session.clone(), self.form.as_mut()) else {
            return;
        };
        let rule = ForwardRule {
            kind: form.kind,
            listen: form.listen.read(cx).value().trim().to_owned(),
            target: if form.kind == ForwardKind::Dynamic {
                String::new()
            } else {
                form.target.read(cx).value().trim().to_owned()
            },
            note: String::new(),
        };
        if !rule.is_valid() {
            form.error = Some(t!("forwards.invalid").into());
            cx.notify();
            return;
        }
        form.error = None;
        form.busy = true;
        let pending = session.add_forward(rule);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(form) = this.form.as_mut() {
                    form.busy = false;
                    form.error = result.err().map(Into::into);
                }
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            session.remove_forward(id);
        }
        self.refresh(cx);
    }

    /// 把临时规则写进主机配置，之后每次连接都会启动。
    fn save(&mut self, rule: ForwardRule, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        config::update_hosts(
            |book| {
                if let Some(h) = book.hosts.iter_mut().find(|h| h.id == host) {
                    h.forwards.push(rule);
                }
            },
            cx,
        );
        cx.notify();
    }

    fn ensure_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            return;
        }
        let listen = cx.new(|cx| InputState::new(window, cx).placeholder(t!("forwards.listen")));
        let target = cx.new(|cx| InputState::new(window, cx).placeholder(t!("forwards.target")));
        self.form = Some(AddForm {
            kind: ForwardKind::Local,
            listen,
            target,
            error: None,
            busy: false,
        });
    }

    fn row(&self, f: &ForwardStatus, saved: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let id = f.id;
        let ix = self.list.iter().position(|x| x.id == id).unwrap_or(0);
        let kind = match f.rule.kind {
            ForwardKind::Local => t!("hosts.forward_local"),
            ForwardKind::Remote => t!("hosts.forward_remote"),
            ForwardKind::Dynamic => t!("hosts.forward_dynamic"),
        };
        let listen = match (f.port, f.rule.listen_addr()) {
            (Some(port), Some((host, _))) if host != "127.0.0.1" => format!("{host}:{port}"),
            (Some(port), _) => port.to_string(),
            (None, _) => f.rule.listen.clone(),
        };
        let route = if f.rule.kind == ForwardKind::Dynamic {
            listen
        } else {
            format!("{listen} → {}", f.rule.target)
        };
        let muted = theme.muted_foreground;
        // 流量各项单独成块，面板窄时整块换行而不是截断
        let stats: Vec<AnyElement> = match &f.error {
            Some(e) => vec![
                div()
                    .text_color(theme.danger)
                    .child(e.clone())
                    .into_any_element(),
            ],
            None => {
                let (up, down) = self.rates.get(&id).copied().unwrap_or_default();
                let mut items = vec![
                    format!("↑ {}", format_size(f.sent)),
                    format!("↓ {}", format_size(f.received)),
                ];
                if up + down > 1. {
                    items.push(format!("{}/s", format_size((up + down) as u64)));
                }
                if f.active > 0 {
                    items.push(t!("forwards.connections", count = f.active).into_owned());
                }
                items
                    .into_iter()
                    .map(|t| {
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(t)
                            .into_any_element()
                    })
                    .collect()
            }
        };
        let rule = f.rule.clone();
        let can_save = !f.from_host && !saved && self.host.is_some();
        let badge = div()
            .flex_none()
            .px_1p5()
            .rounded_full()
            .bg(theme.secondary)
            .text_color(muted)
            .child(kind.to_string());
        v_flex()
            .id(("forward", ix))
            .w_full()
            .gap_0p5()
            .px_3()
            .py_1p5()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.secondary.opacity(0.5)))
            .child(
                h_flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(route))
                    .when(can_save, |el| {
                        el.child(
                            form::icon_button(
                                ("forward-save", ix),
                                gpui_kit::component::IconName::Star,
                                t!("forwards.save"),
                            )
                            .xsmall()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.save(rule.clone(), cx)),
                            ),
                        )
                    })
                    .child(
                        form::icon_button(
                            ("forward-remove", ix),
                            gpui_kit::component::IconName::Close,
                            t!("forwards.stop"),
                        )
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(id, cx))),
                    ),
            )
            .child(
                h_flex().gap_1p5().text_xs().child(badge).child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(muted)
                        .child(f.rule.note.clone()),
                ),
            )
            .child(h_flex().flex_wrap().gap_x_3().text_xs().children(stats))
            .into_any_element()
    }

    fn add_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = &self.form else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let entity = cx.entity().downgrade();
        let kind = form::segmented(
            "forward-kind",
            form.kind,
            vec![
                (ForwardKind::Local, t!("hosts.forward_local").into()),
                (ForwardKind::Remote, t!("hosts.forward_remote").into()),
                (ForwardKind::Dynamic, t!("hosts.forward_dynamic").into()),
            ],
            move |kind, _, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if let Some(form) = this.form.as_mut() {
                        form.kind = kind;
                        cx.notify();
                    }
                });
            },
        );
        let dynamic = form.kind == ForwardKind::Dynamic;
        v_flex()
            .px_3()
            .gap_2()
            .child(kind)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(crate::ui::metrics::FORWARD_LISTEN_WIDTH)
                            .flex_none()
                            .child(Input::new(&form.listen).small()),
                    )
                    .when(!dynamic, |el| {
                        el.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&form.target).small()),
                        )
                    })
                    .when(dynamic, |el| el.child(div().flex_1())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.danger)
                            .truncate()
                            .children(form.error.clone()),
                    )
                    .child(
                        Button::new("forward-add")
                            .label(t!("hosts.forward_add").to_string())
                            .small()
                            .primary()
                            .loading(form.busy)
                            .disabled(form.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl ContextItem for ForwardsPanel {
    fn title(&self) -> SharedString {
        t!("hosts.forwards").into()
    }

    /// 每次重绘与每秒的定时检查都会调用，顺带刷新流量。
    fn follow(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>) {
        let session = target
            .filter(|t| t.host.is_some())
            .map(|t| t.session.clone());
        let changed =
            self.session.as_ref().map(SessionHandle::id) != session.as_ref().map(|s| s.id());
        if changed {
            self.session = session;
            self.host = target.and_then(|t| t.host.clone());
            self.last.clear();
            self.rates.clear();
            if let Some(form) = self.form.as_mut() {
                form.error = None;
            }
            cx.notify();
        }
        self.refresh(cx);
    }

    fn hide(&mut self, _: &mut Context<Self>) {
        self.session = None;
    }

    fn session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        if matches!(
            event,
            SessionEvent::Connected(_) | SessionEvent::Disconnected { .. }
        ) {
            self.refresh(cx);
        }
    }
}

impl Render for ForwardsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if self.session.is_none() {
            return div()
                .px_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!("forwards.ssh_only").to_string())
                .into_any_element();
        }
        self.ensure_form(window, cx);
        let saved: Vec<ForwardRule> = self
            .host
            .as_ref()
            .and_then(|h| ConfigState::hosts(cx).get(h))
            .map(|h| h.forwards.clone())
            .unwrap_or_default();
        let list = self.list.clone();
        let rows: Vec<AnyElement> = list
            .iter()
            .map(|f| self.row(f, saved.contains(&f.rule), cx))
            .collect();
        v_flex()
            .id("forwards-panel")
            .size_full()
            .overflow_y_scroll()
            .gap_3()
            .pb_3()
            // 没有规则时不留空列表，否则表单上方多出一段间距
            .when(!rows.is_empty(), |el| {
                el.child(v_flex().gap_0p5().children(rows))
            })
            .child(self.add_form(cx))
            .into_any_element()
    }
}
