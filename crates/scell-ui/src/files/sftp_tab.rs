//! SFTP 双栏标签：左侧本机、右侧远程，可互相拖放或经菜单发送到对侧。
//!
//! 远程一侧有自己的文件会话（不开 shell 的 SSH 会话），认证提示显示在本标签内；
//! 同一主机已有连接时经连接池复用，不再认证。连接断开后按会话的重连流程恢复，回到断开前的目录。

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Task, Window, div,
};
use rust_i18n::t;
use scell_config::HostId;
use scell_session::{Encoding, SessionHandle};
use scell_vfs::VfsPath;

use super::browser::{BrowserEvent, Dropped, FileBrowser};
use super::transfers;
use crate::app::AppState;
use crate::ui::metrics::PANE_GAP;

enum Remote {
    /// 文件会话正在连接。
    Connecting,
    Opening(#[allow(dead_code)] Task<()>),
    Ready(Entity<FileBrowser>, #[allow(dead_code)] Subscription),
    Failed(SharedString),
}

pub(crate) struct SftpView {
    session: SessionHandle,
    host: HostId,
    title: SharedString,
    /// 远程一侧下次打开时的目录：首次为调用方指定的目录，断开重连后为断开前的目录。
    start: Option<VfsPath>,
    encoding: Encoding,
    local: Entity<FileBrowser>,
    remote: Remote,
    focus: FocusHandle,
    _local_events: Subscription,
}

impl SftpView {
    /// `session` 为该标签专用的文件会话，标签关闭时由宿主关闭。
    pub(crate) fn new(
        session: SessionHandle,
        host: HostId,
        title: SharedString,
        encoding: Encoding,
        start: Option<VfsPath>,
        cx: &mut Context<Self>,
    ) -> Self {
        let local_vfs = cx.global::<AppState>().sessions.local_vfs();
        let local = cx.new(|cx| {
            FileBrowser::new(local_vfs, Encoding::UTF_8, None, cx).send_action(
                t!("files.upload").into(),
                Icon::new(gpui_kit::assets::IconName::Upload),
            )
        });
        let local_events = cx.subscribe(&local, Self::on_local_event);
        Self {
            session,
            host,
            title,
            start,
            encoding,
            local,
            remote: Remote::Connecting,
            focus: cx.focus_handle(),
            _local_events: local_events,
        }
    }

    pub(crate) fn session(&self) -> &SessionHandle {
        &self.session
    }

    pub(crate) fn host(&self) -> &HostId {
        &self.host
    }

    pub(crate) fn connecting(&mut self, cx: &mut Context<Self>) {
        if self.remote().is_none() {
            self.remote = Remote::Connecting;
            cx.notify();
        }
    }

    /// 文件会话连上后打开远程一侧。
    pub(crate) fn connected(&mut self, cx: &mut Context<Self>) {
        if self.remote().is_none() {
            self.open(cx);
        }
    }

    /// 连接断开：SFTP 通道随之失效，记下当前目录以便重连后回到这里。
    pub(crate) fn disconnected(&mut self, reason: String, cx: &mut Context<Self>) {
        if let Some(cwd) = self.remote().and_then(|b| b.read(cx).cwd().cloned()) {
            self.start = Some(cwd);
        }
        self.remote = Remote::Failed(reason.into());
        cx.notify();
    }

    /// （重新）打开远程一侧。
    fn open(&mut self, cx: &mut Context<Self>) {
        let open = self.session.open_vfs();
        let task = cx.spawn(async move |this, cx| {
            let result = open.await;
            let _ = this.update(cx, |this, cx| this.opened(result, cx));
        });
        self.remote = Remote::Opening(task);
        cx.notify();
    }

    fn opened(
        &mut self,
        result: Result<std::sync::Arc<dyn scell_vfs::Vfs>, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(vfs) => {
                let encoding = self.encoding;
                let start = self.start.clone();
                let browser = cx.new(|cx| {
                    FileBrowser::new(vfs, encoding, start, cx).send_action(
                        t!("files.download").into(),
                        Icon::new(gpui_kit::assets::IconName::Download),
                    )
                });
                let events = cx.subscribe(&browser, Self::on_remote_event);
                self.remote = Remote::Ready(browser, events);
            }
            Err(e) => self.remote = Remote::Failed(e.into()),
        }
        cx.notify();
    }

    fn remote(&self) -> Option<&Entity<FileBrowser>> {
        match &self.remote {
            Remote::Ready(browser, _) => Some(browser),
            _ => None,
        }
    }

    /// 把 `from` 一侧的事件落到 `to` 一侧：发送到对侧当前目录，或接收拖放。
    fn route(
        from: &Entity<FileBrowser>,
        to: Option<&Entity<FileBrowser>>,
        event: &BrowserEvent,
        cx: &mut App,
    ) {
        let (vfs, encoding) = {
            let b = from.read(cx);
            (b.vfs().clone(), b.encoding())
        };
        match event {
            BrowserEvent::Send(entries) => {
                let Some(to) = to else {
                    return;
                };
                let (to_vfs, to_encoding, dir) = {
                    let b = to.read(cx);
                    (b.vfs().clone(), b.encoding(), b.cwd().cloned())
                };
                if let Some(dir) = dir {
                    transfers::send(entries, (&vfs, encoding), (&to_vfs, to_encoding, &dir), cx);
                }
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
            BrowserEvent::Expand | BrowserEvent::SendTo(_) => {}
        }
    }

    fn on_local_event(
        &mut self,
        local: Entity<FileBrowser>,
        event: &BrowserEvent,
        cx: &mut Context<Self>,
    ) {
        let remote = self.remote().cloned();
        Self::route(&local, remote.as_ref(), event, cx);
    }

    fn on_remote_event(
        &mut self,
        remote: Entity<FileBrowser>,
        event: &BrowserEvent,
        cx: &mut Context<Self>,
    ) {
        let local = self.local.clone();
        Self::route(&remote, Some(&local), event, cx);
    }

    fn side(&self, title: SharedString, body: AnyElement, cx: &App) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Focusable for SftpView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.remote() {
            Some(remote) => remote.focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

impl Render for SftpView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let remote = match &self.remote {
            Remote::Ready(browser, _) => browser.clone().into_any_element(),
            Remote::Connecting | Remote::Opening(_) => div()
                .p_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!("files.connecting").to_string())
                .into_any_element(),
            Remote::Failed(message) => v_flex()
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
                    Button::new("sftp-retry")
                        .label(t!("files.retry").to_string())
                        .small()
                        .outline()
                        .on_click(cx.listener(|this, _, _, cx| {
                            // 连接还在时只需重开 SFTP，否则让会话重连，连上后自动打开
                            if this.session.is_connected() {
                                this.open(cx);
                            } else {
                                this.remote = Remote::Connecting;
                                this.session.reconnect();
                                cx.notify();
                            }
                        })),
                )
                .into_any_element(),
        };
        h_flex()
            .size_full()
            .p_3()
            .gap(PANE_GAP * 2.)
            .child(self.side(
                t!("files.local").into(),
                self.local.clone().into_any_element(),
                cx,
            ))
            .child(self.side(self.title.clone(), remote, cx))
    }
}
