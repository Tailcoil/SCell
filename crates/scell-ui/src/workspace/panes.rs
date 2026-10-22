//! 当前标签的分屏渲染。每个窗格是一张圆角卡片，卡片之间留白，不画分隔线；多窗格时非焦点窗格略微淡化。

use std::rc::Rc;
use std::time::Instant;

use gpui_kit::base::{ResizablePanelGroup, ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::resizable::resizable_panel;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, IntoElement, ParentElement, Styled, div,
    prelude::FluentBuilder, px,
};
use rust_i18n::t;
use scell_session::{PromptAnswer, PromptKind, SessionId};

use super::Workspace;
use super::pane_tree::{Axis, PaneTree};
use super::tab::{Launch, Pane, PanePrompt, PaneStatus, TabContent, Terminals};
use crate::appearance::Appearance;
use crate::ui::metrics::{ENTER_OFFSET, PANE_GAP, PANE_MIN, PANE_PADDING, PROMPT_WIDTH};
use crate::ui::motion;

/// 多窗格时非焦点卡片的不透明度系数。
const INACTIVE_ALPHA: f32 = 0.6;

pub(super) fn render_active_tab(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(tab) = ws.tabs.get(ws.active) else {
        return div().into_any_element();
    };
    let body = match &tab.content {
        TabContent::Terminals(tab) => match tab.zoomed.and_then(|id| tab.pane(id)) {
            Some(pane) => render_pane(tab, pane, cx),
            None => render_node(tab, &tab.tree, cx),
        },
        TabContent::Settings(view) => card(cx).child(view.clone()).into_any_element(),
        TabContent::Sftp(sftp) => {
            let session = sftp.view.read(cx).session().id();
            div()
                .size_full()
                .p(PANE_GAP / 2.)
                .child(
                    card(cx)
                        .child(sftp.view.clone())
                        .children(sftp.prompt.as_ref().map(|p| prompt_card(session, p, cx))),
                )
                .into_any_element()
        }
    };
    // 叶子四周各留半个间距；外层再补半个，使卡片到窗口边缘的留白与卡片间距一致。左侧挨着侧栏，不补
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .pt(PANE_GAP / 2.)
        .pr(PANE_GAP / 2.)
        .pb(PANE_GAP / 2.)
        .child(body)
        .children(ws.find.as_ref().map(|(find, _)| {
            motion::rise_in(
                div().absolute().right(PANE_GAP * 2.5).child(find.clone()),
                "find-bar",
                -ENTER_OFFSET,
                |el, offset| el.top(PANE_GAP * 1.5 + offset),
                cx,
            )
        }))
        .into_any_element()
}

fn render_node(tab: &Terminals, node: &PaneTree, cx: &mut Context<Workspace>) -> AnyElement {
    match node {
        PaneTree::Leaf(id) => match tab.pane(*id) {
            Some(pane) => render_pane(tab, pane, cx),
            None => div().into_any_element(),
        },
        PaneTree::Split { id, axis, children } => {
            let element_id = ("pane-split", *id as usize);
            let group = match axis {
                Axis::Row => row(element_id),
                Axis::Column => column(element_id),
            };
            // 组件默认的最小尺寸（100px）在窄窗口里多层分屏时会撑出内容区
            children
                .iter()
                .fold(group, |group, child| {
                    group.child(
                        resizable_panel()
                            .size_range(PANE_MIN..gpui_kit::Pixels::MAX)
                            .child(render_node(tab, child, cx)),
                    )
                })
                .into_any_element()
        }
    }
}

fn render_pane(tab: &Terminals, pane: &Pane, cx: &mut Context<Workspace>) -> AnyElement {
    let focused = tab.focused == pane.id || tab.panes.len() == 1 || tab.zoomed.is_some();
    let mut bg = Appearance::card(cx);
    if !focused {
        bg.a *= INACTIVE_ALPHA;
    }
    div()
        .size_full()
        .p(PANE_GAP / 2.)
        .child(
            card(cx)
                .bg(bg)
                .p(PANE_PADDING)
                .child(pane.view.clone())
                .when(pane.status.ended(), |this| {
                    this.child(exit_banner(pane, cx))
                })
                .children(
                    pane.prompt
                        .as_ref()
                        .map(|p| prompt_card(pane.session.id(), p, cx)),
                ),
        )
        .into_any_element()
}

/// 底部停靠面板：与窗格一样的卡片，上方留出窗格间距。
pub(super) fn bottom_panel(content: impl IntoElement, cx: &Context<Workspace>) -> AnyElement {
    div()
        .size_full()
        .pr(PANE_GAP / 2.)
        .pb(PANE_GAP / 2.)
        .child(
            div()
                .size_full()
                .p(PANE_GAP / 2.)
                .child(card(cx).p_3().child(content)),
        )
        .into_any_element()
}

/// 叠在外框上的圆角卡片。
fn card(cx: &Context<Workspace>) -> gpui_kit::Div {
    div()
        .relative()
        .size_full()
        .rounded(cx.theme().radius_lg)
        .bg(Appearance::card(cx))
        .overflow_hidden()
}

/// 进程已退出或连接已断开时，卡片底部显示状态与重启/重连按钮。
fn exit_banner(pane: &Pane, cx: &mut Context<Workspace>) -> AnyElement {
    let ssh = matches!(pane.launch, Launch::Ssh { .. });
    let status = match pane.status {
        PaneStatus::Exited(Some(code)) => t!("tab.exited_code", code = code),
        PaneStatus::Disconnected { retry_at: Some(at) } => {
            let secs = at.saturating_duration_since(Instant::now()).as_secs_f32();
            t!("tab.retry_in", secs = secs.ceil() as u64)
        }
        PaneStatus::Disconnected { retry_at: None } => t!("tab.disconnected"),
        PaneStatus::Idle => t!("tab.idle"),
        _ => t!("tab.exited_signal"),
    };
    let id = pane.id;
    let theme = cx.theme();
    let banner = div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            h_flex()
                .gap_2()
                .pl_3()
                .pr_1()
                .py_1()
                .rounded(theme.radius_lg)
                .bg(theme.popover)
                .shadow_md()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(status.to_string())
                .child(
                    Button::new(("restart-pane", id.0 as usize))
                        .icon(gpui_kit::assets::IconName::RotateCcw)
                        .label(match (ssh, pane.status) {
                            (true, PaneStatus::Idle) => t!("hosts.connect"),
                            (true, _) => t!("tab.reconnect"),
                            (false, _) => t!("tab.restart"),
                        })
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.restart_pane(ws.active, id, window, cx)
                        })),
                ),
        );
    motion::rise_in(
        banner,
        ("exit-banner", id.0 as usize),
        ENTER_OFFSET,
        |el, offset| el.bottom(PANE_GAP * 2. - offset),
        cx,
    )
    .into_any_element()
}

/// 会话等待回答的问题（主机密钥确认、密码、口令、keyboard-interactive），浮在窗格中央。
fn prompt_card(session: SessionId, prompt: &PanePrompt, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = cx.theme();
    let muted = |text: String| {
        div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    // 每个输入框上方的问题文字；密码与口令只有一个输入框，标题已说明
    let mut labels: Vec<Option<String>> = vec![None; prompt.inputs.len()];
    let (title, detail): (String, Option<AnyElement>) = match &prompt.prompt.kind {
        PromptKind::HostKey {
            host,
            port,
            algorithm,
            fingerprint,
        } => {
            let addr = if *port == 22 {
                host.clone()
            } else {
                format!("{host}:{port}")
            };
            (
                t!("prompt.new_host", host = addr).to_string(),
                Some(
                    v_flex()
                        .gap_1()
                        .child(muted(algorithm.clone()))
                        .child(
                            div()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .child(fingerprint.clone()),
                        )
                        .into_any_element(),
                ),
            )
        }
        PromptKind::Password { user, host, retry } => {
            (format!("{user}@{host}"), retry.then(|| retry_hint(cx)))
        }
        PromptKind::Passphrase { path, retry } => (
            t!("prompt.passphrase", key = path.display().to_string()).to_string(),
            retry.then(|| retry_hint(cx)),
        ),
        PromptKind::Interactive {
            user,
            host,
            name,
            instructions,
            prompts,
        } => {
            labels = prompts
                .iter()
                .map(|p| Some(p.text.trim().to_owned()))
                .collect();
            let title = if name.trim().is_empty() {
                format!("{user}@{host}")
            } else {
                name.trim().to_owned()
            };
            let detail = (!instructions.trim().is_empty())
                .then(|| muted(instructions.trim().to_owned()).into_any_element());
            (title, detail)
        }
        _ => (String::new(), None),
    };
    let fields = prompt.inputs.iter().zip(labels).map(|(input, label)| {
        v_flex()
            .gap_1()
            .children(label.map(muted))
            .child(Input::new(input).small())
    });
    let confirm = Button::new("prompt-ok")
        .primary()
        .small()
        .label(if prompt.inputs.is_empty() {
            t!("prompt.trust")
        } else {
            t!("prompt.ok")
        })
        .on_click(cx.listener(move |ws, _, window, cx| ws.submit_prompt(session, window, cx)));
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(motion::rise_in(
            v_flex()
                .relative()
                .w(PROMPT_WIDTH)
                .gap_3()
                .p_4()
                .rounded(theme.radius_lg)
                .bg(theme.popover)
                .shadow_lg()
                .text_sm()
                .child(div().font_medium().child(title))
                .children(detail)
                .children(fields)
                .child(
                    h_flex()
                        .gap_2()
                        .when(prompt.rememberable(), |row| {
                            row.child(
                                Checkbox::new("prompt-remember")
                                    .label(t!("prompt.remember").to_string())
                                    .checked(prompt.remember)
                                    .on_click(cx.listener(move |ws, checked: &bool, _, cx| {
                                        ws.set_prompt_remember(session, *checked, cx)
                                    })),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new("prompt-cancel")
                                .ghost()
                                .small()
                                .label(t!("prompt.cancel"))
                                .on_click(answer(session, PromptAnswer::Cancel, cx)),
                        )
                        .child(confirm),
                ),
            ElementId::Name(format!("prompt-card-{session}").into()),
            ENTER_OFFSET,
            |el, offset| el.top(offset),
            cx,
        ))
        .into_any_element()
}

fn retry_hint(cx: &Context<Workspace>) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().danger)
        .child(t!("prompt.retry").to_string())
        .into_any_element()
}

fn answer(
    session: SessionId,
    answer: PromptAnswer,
    cx: &Context<Workspace>,
) -> impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static {
    cx.listener(move |ws, _, window, cx| ws.answer_prompt(session, answer.clone(), window, cx))
}

/// 左右分栏。分隔处静止时不画线，只在悬停、拖动时显示指示条。
pub(super) fn row(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_kit::base::h_resizable(id).with_handle_appearance(quiet_handle())
}

/// 上下分栏，外观同 [`row`]。
pub(super) fn column(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_kit::base::v_resizable(id).with_handle_appearance(quiet_handle())
}

fn quiet_handle() -> ResizeHandleRenderer {
    Rc::new(|handle, _, cx| {
        let (length, alpha) = match handle.state() {
            ResizeHandleState::Idle => return None,
            ResizeHandleState::Hovered => (px(24.), 0.4),
            ResizeHandleState::Pressed => (px(32.), 0.7),
            ResizeHandleState::Dragging => (px(44.), 0.9),
        };
        let pill = div()
            .flex_none()
            .rounded_full()
            .bg(cx.theme().drag_border.opacity(alpha));
        let pill = match handle.axis() {
            gpui_kit::Axis::Horizontal => pill.w(px(3.)).h(length),
            gpui_kit::Axis::Vertical => pill.h(px(3.)).w(length),
        };
        Some(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(pill)
                .into_any_element(),
        )
    })
}
