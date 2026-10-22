//! 右侧“文件”面板：浏览当前窗格所在机器的文件。本地窗格为本机，SSH 窗格在同一连接上开 SFTP。
//!
//! 每个会话一个浏览器，切换窗格时各自保留位置；终端经 OSC 7 上报的目录变化时跟随过去。
//! SFTP 只在面板第一次显示该会话时打开。

use std::collections::HashMap;

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Sizable, v_flex};
use gpui_kit::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div,
};
use rust_i18n::t;
use scell_session::{SessionEvent, SessionId};
use scell_vfs::VfsPath;

use super::browser::{BrowserEvent, Dropped, FileBrowser};
use super::transfers;
use crate::config::ConfigState;
use crate::panels::{ContextItem, PanelTarget};

enum State {
    Idle,
    Opening(#[allow(dead_code)] Task<()>),
    Ready(Entity<FileBrowser>, #[allow(dead_code)] Subscription),
    Failed(SharedString),
}

struct Slot {
    target: PanelTarget,
    state: State,
    /// 已跟随过的终端目录；终端目录变化时才重新导航，期间用户自己浏览到别处不受打扰。
    followed: Option<VfsPath>,
}

pub(crate) enum FilesPanelEvent {
    /// 为该会话的主机打开 SFTP 标签，远程一侧从给定目录开始。
    OpenTab(SessionId, Option<VfsPath>),
}

impl gpui_kit::EventEmitter<FilesPanelEvent> for FilesPanel {}

#[derive(Default)]
pub(crate) struct FilesPanel {
    current: Option<SessionId>,
    slots: HashMap<SessionId, Slot>,
}

impl FilesPanel {
    /// 切换到 `target` 所在的会话，或在同一会话的终端目录变化时跟随。
    fn follow_target(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>) {
        let Some(target) = target.cloned() else {
            if self.current.take().is_some() {
                cx.notify();
            }
            return;
        };
        let id = target.session.id();
        if self.current != Some(id) {
            self.current = Some(id);
            cx.notify();
        }
        let cwd = target.cwd.clone();
        let slot = self.slots.entry(id).or_insert_with(|| Slot {
            target: target.clone(),
            state: State::Idle,
            followed: None,
        });
        slot.target = target;
        if cwd.is_some() && cwd != slot.followed {
            if let State::Ready(browser, _) = &slot.state {
                let cwd = cwd.clone().unwrap_or_default();
                browser.update(cx, |b, cx| b.navigate(cwd, cx));
            }
            if !matches!(slot.state, State::Idle) {
                slot.followed = cwd;
            }
        }
    }

    /// 会话重新连上后旧的 SFTP 通道已失效，下次显示时重新打开。
    pub(crate) fn reset(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if let Some(slot) = self.slots.get_mut(&session) {
            slot.state = State::Idle;
            slot.followed = None;
            cx.notify();
        }
    }

    fn remove(&mut self, session: SessionId) {
        self.slots.remove(&session);
        if self.current == Some(session) {
            self.current = None;
        }
    }

    fn open(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let open = slot.target.session.open_vfs();
        let task = cx.spawn(async move |this, cx| {
            let result = open.await;
            let _ = this.update(cx, |this, cx| this.opened(id, result, cx));
        });
        slot.state = State::Opening(task);
    }

    fn opened(
        &mut self,
        id: SessionId,
        result: Result<std::sync::Arc<dyn scell_vfs::Vfs>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        let vfs = match result {
            Ok(vfs) => vfs,
            Err(e) => {
                slot.state = State::Failed(e.into());
                cx.notify();
                return;
            }
        };
        let PanelTarget {
            encoding,
            host,
            cwd,
            ..
        } = slot.target.clone();
        let remote = host.is_some();
        let browser = cx.new(|cx| {
            let browser = FileBrowser::new(vfs, encoding, cwd.clone(), cx)
                .uploadable(true)
                .expandable(remote);
            if remote {
                browser
                    .send_action(
                        t!("files.download").into(),
                        gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Download),
                    )
                    .send_to_action(t!("files.download_to").into())
            } else {
                browser
            }
        });
        let events = cx.subscribe(&browser, Self::on_browser_event);
        slot.followed = cwd;
        slot.state = State::Ready(browser, events);
        cx.notify();
    }

    fn on_browser_event(
        &mut self,
        browser: Entity<FileBrowser>,
        event: &BrowserEvent,
        cx: &mut Context<Self>,
    ) {
        let (vfs, encoding) = {
            let b = browser.read(cx);
            (b.vfs().clone(), b.encoding())
        };
        match event {
            BrowserEvent::Expand => {
                if let Some(id) = self.current {
                    let cwd = browser.read(cx).cwd().cloned();
                    cx.emit(FilesPanelEvent::OpenTab(id, cwd));
                }
            }
            BrowserEvent::Send(entries) => {
                let dir = ConfigState::settings(cx).files.download_dir();
                transfers::download(entries, (&vfs, encoding), &dir, cx);
            }
            BrowserEvent::SendTo(entries) => {
                let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(t!("files.download_to").into()),
                });
                let entries = entries.clone();
                cx.spawn(async move |_, cx| {
                    if let Ok(Ok(Some(paths))) = picked.await
                        && let Some(dir) = paths.into_iter().next()
                    {
                        cx.update(|cx| transfers::download(&entries, (&vfs, encoding), &dir, cx));
                    }
                })
                .detach();
            }
            BrowserEvent::Drop { target, source } => match source {
                Dropped::Paths(paths) => transfers::upload(paths, (&vfs, encoding, target), cx),
                Dropped::Files(files) => transfers::send(
                    &files.entries,
                    (&files.vfs, files.encoding),
                    (&vfs, encoding, target),
                    cx,
                ),
            },
        }
    }
}

impl ContextItem for FilesPanel {
    fn title(&self) -> SharedString {
        t!("files.title").into()
    }

    fn follow(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>) {
        self.follow_target(target, cx);
    }

    fn session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        if let SessionEvent::Connected(id) = event {
            self.reset(*id, cx);
        }
    }

    fn release(&mut self, session: SessionId, _: &mut Context<Self>) {
        self.remove(session);
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(id) = self.current else {
            return div().size_full().into_any_element();
        };
        if matches!(self.slots.get(&id).map(|s| &s.state), Some(State::Idle)) {
            self.open(id, cx);
        }
        let theme = cx.theme();
        let body = match self.slots.get(&id).map(|s| &s.state) {
            Some(State::Ready(browser, _)) => browser.clone().into_any_element(),
            Some(State::Failed(message)) => v_flex()
                .gap_2()
                .p_2()
                .items_start()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(message.clone()),
                )
                .child(
                    Button::new("files-retry")
                        .label(t!("files.retry").to_string())
                        .small()
                        .outline()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reset(id, cx);
                        })),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        };
        v_flex().size_full().px_2().child(body).into_any_element()
    }
}
