//! “自动操作”面板：列出当前 SSH 会话所属主机的自动操作，一键在该会话中执行。

use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};
use rust_i18n::t;
use scell_config::HostId;
use scell_config::hosts::{Automation, Step, SwitchMethod};
use scell_session::{SessionEvent, SessionHandle};

use super::{ContextItem, PanelTarget};
use crate::config::ConfigState;
use crate::ui::form;

#[derive(Default)]
pub(crate) struct AutomationsPanel {
    session: Option<SessionHandle>,
    host: Option<HostId>,
    connected: bool,
}

/// 步骤的一行摘要，如 `sudo -i · cd /srv`。
fn summary(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|s| match s {
            Step::Send { text } => text.clone(),
            Step::Expect { wait, send } if send.is_empty() => format!("⏳ {wait}"),
            Step::Expect { wait, send } => format!("{wait} → {send}"),
            Step::SwitchUser {
                method,
                user,
                login,
            } => {
                let cmd = match (method, login) {
                    (SwitchMethod::Su, true) => "su -",
                    (SwitchMethod::Su, false) => "su",
                    (SwitchMethod::Sudo, true) => "sudo -i",
                    (SwitchMethod::Sudo, false) => "sudo -s",
                };
                if user == "root" {
                    cmd.to_owned()
                } else {
                    format!("{cmd} {user}")
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

impl AutomationsPanel {
    fn row(&self, ix: usize, automation: &Automation, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let steps = automation.steps.clone();
        let badge = automation.on_login.then(|| {
            div()
                .flex_none()
                .px_1p5()
                .rounded_full()
                .bg(theme.secondary)
                .text_xs()
                .text_color(muted)
                .child(t!("hosts.automation_on_login").to_string())
        });
        v_flex()
            .id(("automation", ix))
            .w_full()
            .gap_0p5()
            .px_3()
            .py_1p5()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.secondary.opacity(0.5)))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(automation.name.clone()),
                    )
                    .children(badge)
                    .child(
                        form::icon_button(
                            ("automation-run", ix),
                            gpui_kit::component::IconName::Play,
                            t!("automations.run"),
                        )
                        .xsmall()
                        .disabled(!self.connected)
                        .on_click(cx.listener(move |this, _, _, _| {
                            if let Some(session) = &this.session {
                                session.run_steps(steps.clone());
                            }
                        })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(summary(&automation.steps)),
            )
            .into_any_element()
    }
}

impl ContextItem for AutomationsPanel {
    fn title(&self) -> SharedString {
        t!("hosts.automations").into()
    }

    fn follow(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>) {
        let target = target.filter(|t| t.host.is_some());
        let session = target.map(|t| t.session.clone());
        let connected = session.as_ref().is_some_and(SessionHandle::is_connected);
        if self.session.as_ref().map(SessionHandle::id) != session.as_ref().map(|s| s.id())
            || connected != self.connected
        {
            self.session = session;
            self.host = target.and_then(|t| t.host.clone());
            self.connected = connected;
            cx.notify();
        }
    }

    fn hide(&mut self, _: &mut Context<Self>) {
        self.session = None;
    }

    fn session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        if matches!(
            event,
            SessionEvent::Connected(_) | SessionEvent::Disconnected { .. }
        ) {
            self.connected = self
                .session
                .as_ref()
                .is_some_and(SessionHandle::is_connected);
            cx.notify();
        }
    }
}

impl Render for AutomationsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let placeholder = |text: String| {
            div()
                .px_3()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };
        if self.session.is_none() {
            return placeholder(t!("forwards.ssh_only").into_owned());
        }
        let automations: Vec<Automation> = self
            .host
            .as_ref()
            .and_then(|h| ConfigState::hosts(cx).get(h))
            .map(|h| h.automations.clone())
            .unwrap_or_default();
        if automations.is_empty() {
            return placeholder(t!("automations.empty").into_owned());
        }
        let rows: Vec<AnyElement> = automations
            .iter()
            .enumerate()
            .map(|(ix, a)| self.row(ix, a, cx))
            .collect();
        v_flex()
            .id("automations-panel")
            .size_full()
            .overflow_y_scroll()
            .gap_0p5()
            .pb_3()
            .children(rows)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_lists_steps_like_commands() {
        let steps = vec![
            Step::SwitchUser {
                method: SwitchMethod::Sudo,
                user: "root".into(),
                login: true,
            },
            Step::SwitchUser {
                method: SwitchMethod::Su,
                user: "postgres".into(),
                login: false,
            },
            Step::Expect {
                wait: "#".into(),
                send: String::new(),
            },
            Step::Send {
                text: "cd /srv".into(),
            },
        ];
        assert_eq!(summary(&steps), "sudo -i · su postgres · ⏳ # · cd /srv");
    }
}
