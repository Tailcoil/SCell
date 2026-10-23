//! 右侧上下文面板：顶部分页，每页是一个实现了 [`ContextItem`] 的面板，按注册顺序排列。
//!
//! 面板只在自己可见时收到 [`ContextItem::follow`]（当前窗格的会话，每次重绘与定时检查时都会调用，
//! 实现应只在目标变化时做事）；切到别的页或面板收起时收到 [`ContextItem::hide`]，应停止后台工作
//! （轮询等）。会话事件广播给全部面板。状态面板的采集不随可见性启停，由 [`ContextPanel::sync_status`] 驱动。

mod automations;
mod forwards;
mod status;

use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, v_flex};
use gpui_kit::{
    AnyView, App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div,
};
use scell_config::HostId;
use scell_session::{Encoding, SessionEvent, SessionHandle, SessionId};
use scell_vfs::VfsPath;

use crate::files::FilesPanel;
use automations::AutomationsPanel;
use forwards::ForwardsPanel;
use status::StatusPanel;

/// 面板所跟随的窗格。
#[derive(Clone)]
pub(crate) struct PanelTarget {
    pub(crate) session: SessionHandle,
    /// SSH 会话的主机；本地会话为空。
    pub(crate) host: Option<HostId>,
    pub(crate) encoding: Encoding,
    /// 终端的当前目录：OSC 7 上报的，或本地进程的工作目录。
    pub(crate) cwd: Option<VfsPath>,
}

/// 窗口里的一个会话，供状态面板选择采集来源。
pub(crate) struct Source {
    pub(crate) session: SessionHandle,
    /// SSH 会话的主机；本地会话为空。
    pub(crate) host: Option<HostId>,
    /// 本地进程在运行，或远程会话已连上。
    pub(crate) live: bool,
}

/// 上下文面板中的一页。
pub(crate) trait ContextItem: Render {
    fn title(&self) -> SharedString;
    /// 可见期间反复调用；`None` 表示当前标签没有会话（如设置页）。
    fn follow(&mut self, target: Option<&PanelTarget>, cx: &mut Context<Self>);
    /// 不再可见。
    fn hide(&mut self, _cx: &mut Context<Self>) {}
    fn session_event(&mut self, _event: &SessionEvent, _cx: &mut Context<Self>) {}
    /// 会话已关闭，释放为它保留的资源。
    fn release(&mut self, _session: SessionId, _cx: &mut Context<Self>) {}
}

trait AnyItem {
    fn title(&self, cx: &App) -> SharedString;
    fn view(&self) -> AnyView;
    fn follow(&self, target: Option<&PanelTarget>, cx: &mut App);
    fn hide(&self, cx: &mut App);
    fn session_event(&self, event: &SessionEvent, cx: &mut App);
    fn release(&self, session: SessionId, cx: &mut App);
}

impl<T: ContextItem + 'static> AnyItem for Entity<T> {
    fn title(&self, cx: &App) -> SharedString {
        self.read(cx).title()
    }

    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn follow(&self, target: Option<&PanelTarget>, cx: &mut App) {
        self.update(cx, |item, cx| item.follow(target, cx));
    }

    fn hide(&self, cx: &mut App) {
        self.update(cx, |item, cx| item.hide(cx));
    }

    fn session_event(&self, event: &SessionEvent, cx: &mut App) {
        self.update(cx, |item, cx| item.session_event(event, cx));
    }

    fn release(&self, session: SessionId, cx: &mut App) {
        self.update(cx, |item, cx| item.release(session, cx));
    }
}

pub(crate) struct ContextPanel {
    items: Vec<Box<dyn AnyItem>>,
    active: usize,
    /// 当前可见（面板展开）。
    visible: bool,
    target: Option<PanelTarget>,
    /// 文件面板会发出“在标签页中打开”，工作区需要订阅它。
    pub(crate) files: Entity<FilesPanel>,
    status: Entity<StatusPanel>,
}

impl ContextPanel {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let files = cx.new(|_| FilesPanel::default());
        let status = cx.new(|_| StatusPanel::default());
        let items: Vec<Box<dyn AnyItem>> = vec![
            Box::new(status.clone()),
            Box::new(files.clone()),
            Box::new(cx.new(ForwardsPanel::new)),
            Box::new(cx.new(|_| AutomationsPanel::default())),
        ];
        Self {
            items,
            active: 0,
            visible: false,
            target: None,
            files,
            status,
        }
    }

    /// 由工作区定时调用，传入窗口里的全部会话。
    pub(crate) fn sync_status(&self, sources: &[Source], cx: &mut App) {
        self.status.update(cx, |s, cx| s.sync(sources, cx));
    }

    /// 由工作区在重绘与定时检查时调用。
    pub(crate) fn set_target(
        &mut self,
        target: Option<PanelTarget>,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        self.target = target;
        if !visible {
            if self.visible {
                self.visible = false;
                self.items[self.active].hide(cx);
            }
            return;
        }
        self.visible = true;
        self.items[self.active].follow(self.target.as_ref(), cx);
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.active || index >= self.items.len() {
            return;
        }
        if self.visible {
            self.items[self.active].hide(cx);
            self.items[index].follow(self.target.as_ref(), cx);
        }
        self.active = index;
        cx.notify();
    }

    pub(crate) fn session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        for item in &self.items {
            item.session_event(event, cx);
        }
    }

    pub(crate) fn release(&mut self, session: SessionId, cx: &mut Context<Self>) {
        for item in &self.items {
            item.release(session, cx);
        }
    }
}

impl Render for ContextPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let titles: Vec<SharedString> = self.items.iter().map(|i| i.title(cx)).collect();
        let tabs = TabBar::new("context-tabs")
            .underline()
            .small()
            .px_2()
            .selected_index(self.active)
            .children(titles.into_iter().map(|t| Tab::new().label(t).mr_4()))
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.select(*ix, cx)));
        let body = match self.target {
            Some(_) => self.items[self.active].view().into_any_element(),
            // 当前标签没有会话（设置页、SFTP 标签）
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::assets::prompt_icon()
                        .size(crate::ui::metrics::EMPTY_MARK)
                        .text_color(cx.theme().muted_foreground.opacity(0.2)),
                )
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .gap_2()
            .pt_1()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}
