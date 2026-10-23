mod chrome;
mod pane_tree;
mod panes;
mod sidebar;
mod tab;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::input::{self, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::resizable_panel;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div,
};
use rust_i18n::t;
use scell_config::{HostId, SavedLayout, SavedTab, SavedWindow};
use scell_session::{
    LocalShell, Prompt, PromptAnswer, PromptKind, SessionEvent, SessionId, SshTarget,
};
use scell_term::TermSize;
use scell_term_view::TerminalView;

use crate::actions::{
    ActivateTab, ClosePane, CloseTab, CloseWindow, DecreaseFontSize, Find, FocusDown, FocusLeft,
    FocusRight, FocusUp, IncreaseFontSize, MoveToNewWindow, NewTab, NewWindow, NextTab,
    OpenSettings, OpenSftp, PrevTab, QuickConnect, RenameTab, ResetFontSize, RestartTab, SplitDown,
    SplitRight, ToggleContextPanel, TogglePalette, ToggleSidebar, ToggleTransfers, ToggleZoom,
    WORKSPACE_CONTEXT,
};
use crate::app::AppState;
use crate::appearance::{self, Appearance};
use crate::config::{self, ConfigState};
use crate::files::{FilesPanelEvent, SftpView};
use crate::overlays::{FindBar, FindEvent, Palette, PaletteEvent, PaletteMode};
use crate::panels::{ContextPanel, PanelTarget, Source};
use crate::tabs::SettingsView;
use crate::ui::{metrics, motion};
use pane_tree::{Axis, Direction, PaneId};
use sidebar::SidebarView;
pub(crate) use tab::TabId;
use tab::{Launch, Pane, PanePrompt, PaneStatus, SessionIo, Tab, TabContent, Terminals};

/// 右侧面板跟随窗格的检查间隔（本地会话的工作目录、转发流量等）。
const FOLLOW_POLL: Duration = Duration::from_secs(1);
/// SSH 窗格测连接延迟的间隔。
const LATENCY_POLL: Duration = Duration::from_secs(2);

/// 标签、窗格与分屏的 id 全应用唯一，标签移到别的窗口后不必重新编号。
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 新窗口的初始内容。
pub(crate) enum Start {
    Shell(LocalShell),
    Tab(MovedTab),
    Restore(SavedWindow),
}

/// 从窗口中取出、准备放进另一个窗口的标签。提示卡片的输入框和订阅绑定在原窗口上，
/// 只带走问题本身，到新窗口后重建卡片。
pub(crate) struct MovedTab {
    tab: Tab,
    prompts: Vec<(SessionId, Prompt)>,
}

/// 一个主窗口的工作区：竖排标签、当前标签的分屏与右侧上下文面板。
pub(crate) struct Workspace {
    focus: FocusHandle,
    tabs: Vec<Tab>,
    active: usize,
    sidebar_collapsed: bool,
    sidebar_view: SidebarView,
    /// 侧栏主机视图的搜索框。
    host_search: Entity<InputState>,
    /// 用户的开合选择；窗口太窄时面板仍会临时收起，见 [`Self::context_shown`]。
    context_open: bool,
    /// 上一帧面板是否实际显示。
    context_shown: bool,
    context: Entity<ContextPanel>,
    /// 底部传输面板。
    transfers_open: bool,
    /// 当前标签的查找条，切换标签时关闭。
    find: Option<(Entity<FindBar>, Subscription)>,
    palette: Option<(Entity<Palette>, Subscription)>,
    /// 正在行内重命名的标签。
    renaming: Option<Rename>,
    /// 最近一次绘制时的窗口尺寸，退出时随布局保存。
    size: Option<gpui_kit::Size<gpui_kit::Pixels>>,
    /// 正在测延迟、还没回复的主机，网络卡住时不重复发。
    pinging: Vec<HostId>,
    _subscriptions: Vec<Subscription>,
}

struct Rename {
    tab: TabId,
    input: Entity<InputState>,
    _events: Subscription,
}

impl Workspace {
    pub(crate) fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            appearance::set_system_dark(appearance::is_dark(window.appearance()), cx);
        });
        // 窗口关闭时工作区随之释放，其中的会话要一并结束，否则进程会留在后台
        cx.on_release(|ws: &mut Self, cx| {
            // 退出应用时全局状态可能先于实体释放
            if !cx.has_global::<AppState>() {
                return;
            }
            if !ws.tabs.is_empty() {
                let layout = ws.snapshot(cx);
                cx.global_mut::<AppState>().last_closed = Some(layout);
            }
            for pane in ws
                .tabs
                .iter()
                .filter_map(Tab::terminals)
                .flat_map(|t| &t.panes)
            {
                let state = cx.global_mut::<AppState>();
                state.unregister_view(pane.session.id());
                state.sessions.close(pane.session.id());
            }
            for tab in &ws.tabs {
                if let TabContent::Sftp(sftp) = &tab.content {
                    let id = sftp.view.read(cx).session().id();
                    cx.global::<AppState>().sessions.close(id);
                }
            }
        })
        .detach();
        let host_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("hosts.search").to_string()));
        let search_changed = cx.subscribe(&host_search, |_, _, _: &InputEvent, cx| cx.notify());
        let mut ws = Self {
            focus: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            sidebar_collapsed: false,
            sidebar_view: SidebarView::Sessions,
            host_search,
            context_open: true,
            context_shown: true,
            context: cx.new(ContextPanel::new),
            transfers_open: false,
            find: None,
            palette: None,
            renaming: None,
            size: None,
            pinging: Vec::new(),
            _subscriptions: vec![appearance, search_changed],
        };
        let files = ws.context.read(cx).files.clone();
        let open_tab = cx.subscribe_in(&files, window, |ws, _, event, window, cx| match event {
            FilesPanelEvent::OpenTab(session, start) => {
                ws.open_sftp(*session, start.clone(), window, cx)
            }
        });
        ws._subscriptions.push(open_tab);
        // 点在侧栏空白、标题栏、状态面板等不可聚焦处时，焦点会落到工作区自身；菜单关闭后也会把焦点还到这里。
        // 一律转给当前标签，窗口在前台时总能直接输入
        let reclaim = cx.on_focus(&ws.focus, window, Self::restore_focus);
        let lost = cx.on_focus_lost(window, Self::restore_focus);
        ws._subscriptions.extend([reclaim, lost]);
        // 窗口管理器发起的关闭（Alt+F4、任务栏、Windows 的系统按钮）同样先确认
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |ws, cx| ws.confirm_close(window, cx))
                .unwrap_or(true)
        });
        // 多数 shell 不发 OSC 7，cd 也不会让工作区重绘；文件面板可见时定期读取本地进程的工作目录
        cx.spawn(async move |ws, cx| {
            loop {
                cx.background_executor().timer(FOLLOW_POLL).await;
                if ws.update(cx, |ws, cx| ws.sync_context(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |ws, cx| {
            loop {
                cx.background_executor().timer(LATENCY_POLL).await;
                if ws.update(cx, |ws, cx| ws.measure_latency(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        match start {
            Start::Shell(shell) => ws.open_local_tab(shell, window, cx),
            Start::Tab(moved) => ws.attach_tab(moved, window, cx),
            Start::Restore(saved) => ws.restore(saved, window, cx),
        }
        ws
    }

    /// 本窗口中活跃的远程连接数：已连上或正在连的 SSH 窗格，以及 SFTP 标签。
    fn active_connections(&self) -> usize {
        self.tabs
            .iter()
            .map(|tab| match &tab.content {
                TabContent::Terminals(t) => t
                    .panes
                    .iter()
                    .filter(|p| {
                        matches!(p.launch, Launch::Ssh { .. })
                            && matches!(p.status, PaneStatus::Running | PaneStatus::Connecting)
                    })
                    .count(),
                TabContent::Sftp(_) => 1,
                TabContent::Settings(_) => 0,
            })
            .sum()
    }

    /// 能否直接关闭窗口。有两条以上活跃连接时弹出确认并返回 `false`，确认后由对话框关窗。
    fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let count = self.active_connections();
        if count < 2 {
            return true;
        }
        if !window.has_active_dialog(cx) {
            window.open_alert_dialog(cx, move |alert, _, cx| {
                alert
                    // 主题背景在毛玻璃材质下是半透明的，对话框要用不透明的表面色
                    .bg(Appearance::surface(cx))
                    .title(t!("workspace.close_window_title").to_string())
                    .description(t!("workspace.close_window_detail", count = count).to_string())
                    .ok_text(t!("workspace.close_window").to_string())
                    .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                    .cancel_text(t!("transfers.cancel").to_string())
                    .show_cancel(true)
                    .on_ok(|_, window, _| {
                        window.remove_window();
                        true
                    })
            });
        }
        false
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_close(window, cx) {
            window.remove_window();
        }
    }

    /// 启动会话并创建窗格；失败时在窗口内提示。
    fn spawn_pane(
        &mut self,
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Pane> {
        self.spawn_pane_with(launch, false, window, cx)
    }

    /// `idle` 时 SSH 会话先不连接，窗格显示“未连接”，按 Enter 再连。
    fn spawn_pane_with(
        &mut self,
        launch: Launch,
        idle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Pane> {
        let sessions = cx.global::<AppState>().sessions.clone();
        let result = match &launch {
            Launch::Local(shell) => sessions
                .spawn_local(shell.clone(), TermSize::default())
                .map_err(|e| e.to_string()),
            Launch::Ssh { host, .. } => ssh_target(host, cx).map(|target| match idle {
                true => sessions.spawn_ssh_idle(target, TermSize::default()),
                false => sessions.spawn_ssh(target, TermSize::default()),
            }),
        };
        let handle = match result {
            Ok(handle) => handle,
            Err(err) => {
                let message = t!("workspace.spawn_failed", error = err);
                window.push_notification(Notification::error(message.to_string()), cx);
                return None;
            }
        };
        let terminal = handle.terminal().clone();
        let io = SessionIo(handle.clone());
        let style = Appearance::terminal_style(cx);
        let copy_on_select = ConfigState::settings(cx).terminal.copy_on_select;
        let paste_guard = crate::overlays::paste::guard(cx);
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(terminal, io, style, cx);
            view.set_copy_on_select(copy_on_select);
            view.set_paste_guard(paste_guard);
            view
        });
        cx.global_mut::<AppState>()
            .register_view(handle.id(), view.downgrade());
        let mut pane = Pane::new(PaneId(next_id()), handle, view, launch);
        if idle && matches!(pane.launch, Launch::Ssh { .. }) {
            pane.status = PaneStatus::Idle;
            pane.view.update(cx, |view, cx| view.set_exited(true, cx));
        }
        Some(pane)
    }

    fn active_terminals(&self) -> Option<&Terminals> {
        self.tabs.get(self.active).and_then(Tab::terminals)
    }

    fn active_terminals_mut(&mut self) -> Option<&mut Terminals> {
        self.tabs.get_mut(self.active).and_then(Tab::terminals_mut)
    }

    fn pane_mut(&mut self, tab: usize, pane: PaneId) -> Option<&mut Pane> {
        self.tabs
            .get_mut(tab)
            .and_then(Tab::terminals_mut)
            .and_then(|t| t.pane_mut(pane))
    }

    fn release_pane(context: &Entity<ContextPanel>, pane: &Pane, cx: &mut Context<Self>) {
        let id = pane.session.id();
        let state = cx.global_mut::<AppState>();
        state.unregister_view(id);
        state.sessions.close(id);
        // 面板持有会话句柄（如文件面板的 SFTP 通道），会让连接一直保留
        context.update(cx, |c, cx| c.release(id, cx));
    }

    /// 让右侧面板跟随当前标签。面板收起时只通知它隐藏，不读取工作目录等信息；
    /// 状态面板的采集不论面板是否可见都按全部会话同步。
    fn sync_context(&self, cx: &mut Context<Self>) {
        let sources = self.status_sources(cx);
        let target = if self.context_shown {
            self.context_target(cx)
        } else {
            None
        };
        let visible = self.context_shown;
        self.context.update(cx, |c, cx| {
            c.sync_status(&sources, cx);
            c.set_target(target, visible, cx);
        });
    }

    /// 给当前标签里已连上的 SSH 窗格测延迟。同一主机的会话共用连接，每台主机只测一次。
    fn measure_latency(&mut self, cx: &mut Context<Self>) {
        if !ConfigState::settings(cx).ssh.show_latency {
            return;
        }
        let Some(t) = self.active_terminals() else {
            return;
        };
        let mut pings = Vec::new();
        for p in &t.panes {
            if let Launch::Ssh { host, .. } = &p.launch
                && p.status == PaneStatus::Running
                && !self.pinging.contains(host)
                && !pings.iter().any(|(h, _)| h == host)
            {
                pings.push((host.clone(), p.session.ping()));
            }
        }
        for (host, ping) in pings {
            self.pinging.push(host.clone());
            cx.spawn(async move |ws, cx| {
                let rtt = ping.await;
                let _ = ws.update(cx, |ws, cx| ws.set_latency(&host, rtt, cx));
            })
            .detach();
        }
    }

    fn set_latency(&mut self, host: &HostId, rtt: Option<Duration>, cx: &mut Context<Self>) {
        self.pinging.retain(|h| h != host);
        for t in self.tabs.iter_mut().filter_map(Tab::terminals_mut) {
            for p in &mut t.panes {
                if matches!(&p.launch, Launch::Ssh { host: h, .. } if h == host) {
                    p.latency = rtt;
                }
            }
        }
        cx.notify();
    }

    /// 窗口里全部终端窗格与 SFTP 标签的会话。
    fn status_sources(&self, cx: &gpui_kit::App) -> Vec<Source> {
        let mut sources = Vec::new();
        for tab in &self.tabs {
            match &tab.content {
                TabContent::Terminals(t) => {
                    sources.extend(t.panes.iter().map(|p| Source {
                        session: p.session.clone(),
                        host: match &p.launch {
                            Launch::Ssh { host, .. } => Some(host.clone()),
                            Launch::Local(_) => None,
                        },
                        live: p.status == PaneStatus::Running,
                    }));
                }
                TabContent::Sftp(sftp) => {
                    let view = sftp.view.read(cx);
                    sources.push(Source {
                        session: view.session().clone(),
                        host: Some(view.host().clone()),
                        live: view.is_connected(),
                    });
                }
                TabContent::Settings(_) => {}
            }
        }
        sources
    }

    /// 终端标签跟随焦点窗格，SFTP 标签跟随它连接的主机。
    fn context_target(&self, cx: &gpui_kit::App) -> Option<PanelTarget> {
        let encoding = |host: &HostId| {
            scell_session::Encoding::of_label(
                ConfigState::hosts(cx)
                    .get(host)
                    .and_then(|h| h.encoding.as_deref()),
            )
        };
        match &self.tabs.get(self.active)?.content {
            TabContent::Terminals(t) => t.focused_pane().map(|p| match &p.launch {
                Launch::Ssh { host, .. } => PanelTarget {
                    session: p.session.clone(),
                    host: Some(host.clone()),
                    encoding: encoding(host),
                    cwd: p.osc_cwd.clone(),
                },
                Launch::Local(_) => PanelTarget {
                    session: p.session.clone(),
                    host: None,
                    encoding: scell_session::Encoding::UTF_8,
                    cwd: p
                        .cwd
                        .clone()
                        .or_else(|| p.session.process_cwd())
                        .map(|c| scell_vfs::LocalFs::from_path(&c)),
                },
            }),
            TabContent::Sftp(sftp) => {
                let view = sftp.view.read(cx);
                Some(PanelTarget {
                    session: view.session().clone(),
                    host: Some(view.host().clone()),
                    encoding: encoding(view.host()),
                    cwd: None,
                })
            }
            TabContent::Settings(_) => None,
        }
    }

    fn open_local_tab(&mut self, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(Launch::Local(shell), window, cx);
    }

    /// 中间内容区的大致宽度（窗口减去侧栏与右侧面板），供需要按宽度切换版式的页面使用。
    pub(crate) fn content_width(&self, window: &Window) -> gpui_kit::Pixels {
        let panel = if self.context_fits(window) && self.context_open {
            metrics::CONTEXT_PANEL_WIDTH
        } else {
            gpui_kit::px(0.)
        };
        self.width_beside_sidebar(window) - panel - metrics::PANE_GAP * 2.
    }

    fn width_beside_sidebar(&self, window: &Window) -> gpui_kit::Pixels {
        let sidebar = if self.sidebar_collapsed {
            metrics::SIDEBAR_COLLAPSED_WIDTH
        } else {
            metrics::SIDEBAR_WIDTH
        };
        window.viewport_size().width - sidebar
    }

    /// 放下右侧面板后内容区仍有 [`metrics::CONTENT_MIN_WIDTH`]。
    fn context_fits(&self, window: &Window) -> bool {
        self.width_beside_sidebar(window) - metrics::CONTEXT_PANEL_WIDTH
            >= metrics::CONTENT_MIN_WIDTH
    }

    /// 连接主机库中的主机，在新标签中打开。
    pub(crate) fn open_host(&mut self, host: &HostId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = ConfigState::hosts(cx).get(host).map(|h| h.name.clone()) else {
            return;
        };
        let launch = Launch::Ssh {
            host: host.clone(),
            name: name.into(),
        };
        config::touch_recent_host(host, cx);
        self.open_tab(launch, window, cx);
    }

    fn open_tab(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.spawn_pane(launch, window, cx) else {
            return;
        };
        // 新标签只在会话视图里看得到；从主机视图发起时切回去，方便直接操作
        self.sidebar_view = SidebarView::Sessions;
        self.tabs.push(Tab::new(
            TabId(next_id()),
            TabContent::Terminals(Terminals::new(pane)),
        ));
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// 打开设置页；已打开时切换过去。
    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(Tab::is_settings) {
            self.activate(index, window, cx);
            return;
        }
        let tab = Self::settings_tab(window, cx);
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// 设置页持有本窗口工作区的引用，换窗口时重建。
    fn settings_tab(window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let workspace = cx.entity().downgrade();
        let view = cx.new(|cx| SettingsView::new(workspace, window, cx));
        Tab::new(TabId(next_id()), TabContent::Settings(view))
    }

    /// 为窗格会话 `session` 所在的主机打开 SFTP 标签，远程一侧从 `start` 开始。
    pub(crate) fn open_sftp(
        &mut self,
        session: SessionId,
        start: Option<scell_vfs::VfsPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host = self
            .find_session(session)
            .and_then(|(tab, pane)| self.tabs[tab].terminals()?.pane(pane))
            .and_then(|p| match &p.launch {
                Launch::Ssh { host, .. } => Some(host.clone()),
                Launch::Local(_) => None,
            });
        if let Some(host) = host {
            self.open_host_sftp(&host, start, window, cx);
        }
    }

    /// 打开主机的 SFTP 标签；已打开时切换过去。标签有自己的文件会话，主机已有连接时复用。
    pub(crate) fn open_host_sftp(
        &mut self,
        host: &HostId,
        start: Option<scell_vfs::VfsPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(
            |t| matches!(&t.content, TabContent::Sftp(s) if s.view.read(cx).host() == host),
        ) {
            self.activate(index, window, cx);
            return;
        }
        let tab = match Self::sftp_tab_for(host, start, false, cx) {
            Ok(tab) => tab,
            Err(err) => {
                let message = t!("workspace.spawn_failed", error = err);
                window.push_notification(Notification::error(message.to_string()), cx);
                return;
            }
        };
        config::touch_recent_host(host, cx);
        self.tabs.push(tab);
        self.sidebar_view = SidebarView::Sessions;
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// `idle` 时文件会话先不连接，标签里显示重连按钮。
    fn sftp_tab_for(
        host: &HostId,
        start: Option<scell_vfs::VfsPath>,
        idle: bool,
        cx: &mut Context<Self>,
    ) -> Result<Tab, String> {
        let target = ssh_target(host, cx)?;
        let title: gpui_kit::SharedString = target.host.name.clone().into();
        let encoding = scell_session::Encoding::of_label(target.host.encoding.as_deref());
        let handle = cx.global::<AppState>().sessions.spawn_files(target, idle);
        let host = host.clone();
        let view = cx.new(|cx| {
            let mut view = SftpView::new(handle, host, title.clone(), encoding, start, cx);
            if idle {
                view.disconnected(String::new(), cx);
            }
            view
        });
        Ok(Tab::new(
            TabId(next_id()),
            TabContent::Sftp(tab::SftpTab {
                view,
                title,
                prompt: None,
            }),
        ))
    }

    /// 标签当前窗格是 SSH 会话时，为它的主机打开 SFTP 标签。
    fn open_tab_sftp(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let session = self
            .tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(Tab::terminals)
            .and_then(Terminals::focused_pane)
            .map(|p| p.session.id());
        if let Some(session) = session {
            self.open_sftp(session, None, window, cx);
        }
    }

    /// 文件会话 `session` 所属的 SFTP 标签。
    fn sftp_tab(&self, session: SessionId, cx: &gpui_kit::App) -> Option<usize> {
        self.tabs.iter().position(|t| {
            matches!(&t.content, TabContent::Sftp(s) if s.view.read(cx).session().id() == session)
        })
    }

    fn on_sftp_event(
        &mut self,
        index: usize,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TabContent::Sftp(sftp) = &mut self.tabs[index].content else {
            return;
        };
        let view = sftp.view.clone();
        match event {
            SessionEvent::Connecting(_) => view.update(cx, |v, cx| v.connecting(cx)),
            SessionEvent::Connected(_) => view.update(cx, |v, cx| v.connected(cx)),
            SessionEvent::Disconnected { reason, .. } => {
                sftp.prompt = None;
                let reason = reason.clone();
                view.update(cx, |v, cx| v.disconnected(reason, cx));
            }
            SessionEvent::Prompt(id, prompt) => {
                let card = self.build_prompt(*id, prompt.clone(), window, cx);
                if let TabContent::Sftp(sftp) = &mut self.tabs[index].content {
                    sftp.prompt = Some(card);
                }
                if index == self.active {
                    self.activate(index, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index != self.active
            && let Some((find, _)) = self.find.take()
        {
            find.update(cx, |find, cx| find.close(cx));
        }
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        self.active = index;
        match &tab.content {
            TabContent::Terminals(t) => {
                let focused = t.focused;
                self.focus_pane(focused, window, cx);
            }
            TabContent::Settings(view) => {
                window.focus(&view.focus_handle(cx), cx);
                cx.notify();
            }
            TabContent::Sftp(sftp) => {
                let view = sftp.view.clone();
                let session = view.read(cx).session().id();
                if !self.focus_prompt_input(session, window, cx) {
                    window.focus(&view.focus_handle(cx), cx);
                }
                cx.notify();
            }
        }
    }

    fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_terminals_mut() else {
            return;
        };
        let Some(p) = tab.pane_mut(pane) else {
            return;
        };
        p.bell = false;
        let view = p.view.clone();
        let session = p.session.id();
        tab.focused = pane;
        // 有待回答的输入类问题时聚焦输入框
        if !self.focus_prompt_input(session, window, cx) {
            window.focus(&view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let TabContent::Sftp(sftp) = &tab.content {
            let id = sftp.view.read(cx).session().id();
            cx.global::<AppState>().sessions.close(id);
            self.context.update(cx, |c, cx| c.release(id, cx));
        }
        for pane in tab.terminals().into_iter().flat_map(|t| &t.panes) {
            Self::release_pane(&self.context, pane, cx);
        }
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        let next = if index <= self.active {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        // 被关闭的标签就是当前标签时，强制重新激活以转移焦点
        self.active = usize::MAX;
        self.activate(next.min(self.tabs.len() - 1), window, cx);
    }

    /// 从本窗口取出标签，会话保持运行。取走最后一个标签时关闭窗口。
    pub(crate) fn take_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<MovedTab> {
        let index = self.tab_index(id)?;
        if self.renaming.as_ref().is_some_and(|r| r.tab == id) {
            self.renaming = None;
        }
        if index == self.active
            && let Some((find, _)) = self.find.take()
        {
            find.update(cx, |find, cx| find.close(cx));
        }
        let mut tab = self.tabs.remove(index);
        let mut prompts = Vec::new();
        match &mut tab.content {
            TabContent::Terminals(t) => {
                for pane in &mut t.panes {
                    let id = pane.session.id();
                    if let Some(card) = pane.prompt.take() {
                        prompts.push((id, card.prompt));
                    }
                    // 状态等面板为会话保留的数据随会话去新窗口重新开始
                    self.context.update(cx, |c, cx| c.release(id, cx));
                }
            }
            TabContent::Sftp(sftp) => {
                let id = sftp.view.read(cx).session().id();
                if let Some(card) = sftp.prompt.take() {
                    prompts.push((id, card.prompt));
                }
                self.context.update(cx, |c, cx| c.release(id, cx));
            }
            TabContent::Settings(_) => {}
        }
        if self.tabs.is_empty() {
            window.remove_window();
        } else {
            let next = if index <= self.active {
                self.active.saturating_sub(1)
            } else {
                self.active
            };
            self.active = usize::MAX;
            self.activate(next.min(self.tabs.len() - 1), window, cx);
        }
        Some(MovedTab { tab, prompts })
    }

    /// 放入从别的窗口移来的标签并切换过去。
    pub(crate) fn attach_tab(
        &mut self,
        moved: MovedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let MovedTab { tab, prompts } = moved;
        let tab = match tab.content {
            TabContent::Settings(_) => match self.tabs.iter().position(Tab::is_settings) {
                Some(index) => {
                    self.activate(index, window, cx);
                    return;
                }
                None => Self::settings_tab(window, cx),
            },
            _ => tab,
        };
        let countdowns: Vec<Duration> = tab
            .terminals()
            .into_iter()
            .flat_map(|t| &t.panes)
            .filter_map(|p| match p.status {
                PaneStatus::Disconnected { retry_at: Some(at) } => {
                    Some(at.saturating_duration_since(Instant::now()))
                }
                _ => None,
            })
            .collect();
        self.tabs.push(tab);
        for (session, prompt) in prompts {
            let card = self.build_prompt(session, prompt, window, cx);
            match self.prompt_slot(session, cx) {
                Some(slot) => *slot = Some(card),
                None => card.prompt.answer(PromptAnswer::Cancel),
            }
        }
        for delay in countdowns {
            Self::tick_countdown(delay, cx);
        }
        self.active = usize::MAX;
        self.activate(self.tabs.len() - 1, window, cx);
        window.activate_window();
    }

    /// 当前布局，退出时保存，下次启动时由 [`Self::restore`] 恢复。
    pub(crate) fn snapshot(&self, cx: &gpui_kit::App) -> SavedWindow {
        let tabs = self
            .tabs
            .iter()
            .map(|tab| match &tab.content {
                TabContent::Terminals(t) => SavedTab::Terminals {
                    title: tab.custom_title.as_ref().map(ToString::to_string),
                    layout: save_layout(&t.tree, t),
                    focused: t
                        .tree
                        .leaves()
                        .iter()
                        .position(|p| *p == t.focused)
                        .unwrap_or(0),
                },
                TabContent::Sftp(sftp) => SavedTab::Sftp {
                    host: sftp.view.read(cx).host().clone(),
                },
                TabContent::Settings(_) => SavedTab::Settings,
            })
            .collect();
        SavedWindow {
            tabs,
            active: self.active,
            size: self.size.map(|s| (s.width.into(), s.height.into())),
            sidebar_collapsed: self.sidebar_collapsed,
            context_open: self.context_open,
            transfers_open: self.transfers_open,
        }
    }

    /// 按保存的布局重建标签。本地窗格在原目录重新启动 shell；远程窗格与 SFTP 标签先不连接；
    /// 主机已被删除的窗格略过。什么都没恢复出来时开一个本地标签。
    fn restore(&mut self, saved: SavedWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_collapsed = saved.sidebar_collapsed;
        self.context_open = saved.context_open;
        self.transfers_open = saved.transfers_open;
        for tab in saved.tabs {
            let restored = match tab {
                SavedTab::Terminals {
                    title,
                    layout,
                    focused,
                } => {
                    let mut panes = Vec::new();
                    self.restore_layout(&layout, &mut panes, window, cx)
                        .map(|tree| {
                            let leaves = tree.leaves();
                            let focused = leaves.get(focused).unwrap_or(&leaves[0]);
                            let mut tab = Tab::new(
                                TabId(next_id()),
                                TabContent::Terminals(Terminals {
                                    focused: *focused,
                                    tree,
                                    panes,
                                    zoomed: None,
                                }),
                            );
                            tab.custom_title = title.map(Into::into);
                            tab
                        })
                }
                SavedTab::Sftp { host } => Self::sftp_tab_for(&host, None, true, cx).ok(),
                SavedTab::Settings => Some(Self::settings_tab(window, cx)),
                _ => None,
            };
            self.tabs.extend(restored);
        }
        if self.tabs.is_empty() {
            self.open_local_tab(config::local_shell(cx), window, cx);
            return;
        }
        self.active = usize::MAX;
        self.activate(saved.active.min(self.tabs.len() - 1), window, cx);
    }

    fn restore_layout(
        &mut self,
        layout: &SavedLayout,
        panes: &mut Vec<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<pane_tree::PaneTree> {
        let launch = match layout {
            SavedLayout::Split { row, children } => {
                let children = children
                    .iter()
                    .filter_map(|c| self.restore_layout(c, panes, window, cx))
                    .collect();
                let axis = if *row { Axis::Row } else { Axis::Column };
                return pane_tree::PaneTree::join(axis, children);
            }
            SavedLayout::Local { program, args, cwd } => Launch::Local(LocalShell {
                program: program.clone(),
                args: args.clone(),
                cwd: cwd.clone().filter(|d| d.is_dir()),
                ..LocalShell::default()
            }),
            SavedLayout::Ssh { host } => Launch::Ssh {
                name: ConfigState::hosts(cx).get(host)?.name.clone().into(),
                host: host.clone(),
            },
            _ => return None,
        };
        let pane = self.spawn_pane_with(launch, true, window, cx)?;
        let id = pane.id;
        panes.push(pane);
        Some(pane_tree::PaneTree::Leaf(id))
    }

    /// 窗口在“移到窗口”菜单里的名称：当前标签的标题。
    pub(crate) fn window_label(&self) -> gpui_kit::SharedString {
        self.tabs
            .get(self.active)
            .map(Tab::display_title)
            .unwrap_or_default()
    }

    /// 关闭窗格；标签只剩这一个窗格时关闭整个标签。
    fn close_pane(
        &mut self,
        tab: usize,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context = self.context.clone();
        let Some(t) = self.tabs.get_mut(tab).and_then(Tab::terminals_mut) else {
            return;
        };
        if !t.tree.remove(pane) {
            self.close_tab_at(tab, window, cx);
            return;
        }
        if let Some(i) = t.panes.iter().position(|p| p.id == pane) {
            let removed = t.panes.remove(i);
            Self::release_pane(&context, &removed, cx);
        }
        if t.zoomed == Some(pane) {
            t.zoomed = None;
        }
        if t.focused == pane {
            t.focused = t.tree.leaves()[0];
        }
        if tab == self.active {
            let focused = t.focused;
            self.focus_pane(focused, window, cx);
        }
        cx.notify();
    }

    /// 在原位置重新启动已退出的窗格。远程会话在原终端上重连，回滚内容保留。
    fn restart_pane(
        &mut self,
        tab: usize,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self
            .tabs
            .get(tab)
            .and_then(Tab::terminals)
            .and_then(|t| t.pane(pane))
        else {
            return;
        };
        if let Launch::Ssh { .. } = p.launch {
            p.session.reconnect();
            return;
        }
        let launch = p.launch.clone();
        let Some(mut fresh) = self.spawn_pane(launch, window, cx) else {
            return;
        };
        let Some(slot) = self.tabs[tab]
            .terminals_mut()
            .and_then(|t| t.pane_mut(pane))
        else {
            return;
        };
        // 沿用原窗格 id，分屏树无需变动
        fresh.id = pane;
        let old = std::mem::replace(slot, fresh);
        Self::release_pane(&self.context, &old, cx);
        if tab == self.active {
            self.focus_pane(pane, window, cx);
        }
    }

    pub(crate) fn on_session_event(
        &mut self,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = match event {
            SessionEvent::Title(id, _)
            | SessionEvent::Prompt(id, _)
            | SessionEvent::Connecting(id)
            | SessionEvent::Connected(id)
            | SessionEvent::Disconnected { id, .. }
            | SessionEvent::Exited { id, .. }
            | SessionEvent::Bell(id)
            | SessionEvent::WorkingDirectory(id, _)
            | SessionEvent::ClipboardStore(id, _) => *id,
            _ => return,
        };
        let sftp = self.sftp_tab(session, cx);
        let pane = self.find_session(session);
        if (sftp.is_some() || pane.is_some())
            && matches!(
                event,
                SessionEvent::Connected(_) | SessionEvent::Disconnected { .. }
            )
        {
            self.context.update(cx, |c, cx| c.session_event(event, cx));
        }
        if let Some(index) = sftp {
            self.on_sftp_event(index, event, window, cx);
            return;
        }
        let Some((tab, pane)) = pane else {
            return;
        };
        match event {
            SessionEvent::Title(_, title) => {
                if let Some(p) = self.pane_mut(tab, pane) {
                    p.title = title.clone();
                }
                cx.notify();
            }
            SessionEvent::Exited { code, .. } => {
                // 正常退出（如输入 exit）直接关闭；异常退出保留屏幕以便查看输出
                if *code == Some(0) {
                    self.close_pane(tab, pane, window, cx);
                    return;
                }
                self.set_status(tab, pane, PaneStatus::Exited(*code), cx);
            }
            SessionEvent::Connecting(_) => {
                self.set_status(tab, pane, PaneStatus::Connecting, cx);
            }
            SessionEvent::Connected(_) => {
                self.set_status(tab, pane, PaneStatus::Running, cx);
            }
            SessionEvent::Disconnected { retry_in, .. } => {
                let retry_at = retry_in.map(|d| Instant::now() + d);
                self.set_status(tab, pane, PaneStatus::Disconnected { retry_at }, cx);
                if let Some(delay) = *retry_in {
                    Self::tick_countdown(delay, cx);
                }
            }
            SessionEvent::WorkingDirectory(_, url) => {
                if let Some(p) = self.pane_mut(tab, pane) {
                    p.cwd = scell_session::local_cwd(url);
                    p.osc_cwd = scell_session::osc7_path(url).map(scell_vfs::VfsPath::new);
                }
                cx.notify();
            }
            SessionEvent::Bell(_) => {
                let visible = tab == self.active
                    && self.tabs[tab]
                        .terminals()
                        .is_some_and(|t| t.focused == pane)
                    && window.is_window_active();
                if !visible && let Some(p) = self.pane_mut(tab, pane) {
                    p.bell = true;
                    cx.notify();
                }
            }
            SessionEvent::Prompt(_, prompt) => {
                self.show_prompt(tab, pane, session, prompt.clone(), window, cx);
            }
            // 程序通过 OSC 52 写剪贴板；读取请求在终端层就已拒绝
            SessionEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()));
            }
            _ => {}
        }
    }

    /// 横幅上的重连倒计时每秒刷新一次。
    fn tick_countdown(delay: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            for _ in 0..delay.as_secs() {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if ws.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// 更新窗格状态；会话不再交互时视图停止接受输入，Enter 用于重启或重连。
    fn set_status(&mut self, tab: usize, pane: PaneId, status: PaneStatus, cx: &mut Context<Self>) {
        let Some(p) = self.pane_mut(tab, pane) else {
            return;
        };
        if status.ended() {
            p.prompt = None;
        }
        p.status = status;
        p.view
            .update(cx, |view, cx| view.set_exited(status.ended(), cx));
        cx.notify();
    }

    /// 为问题建卡片：每个需要输入的问题一个输入框，Enter 跳到下一个，最后一个提交。
    fn build_prompt(
        &mut self,
        session: SessionId,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PanePrompt {
        let fields: Vec<bool> = match &prompt.kind {
            PromptKind::Password { .. } | PromptKind::Passphrase { .. } => vec![false],
            PromptKind::Interactive { prompts, .. } => prompts.iter().map(|p| p.echo).collect(),
            _ => Vec::new(),
        };
        let inputs: Vec<Entity<InputState>> = fields
            .iter()
            .map(|&echo| cx.new(|cx| InputState::new(window, cx).masked(!echo)))
            .collect();
        let events = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| {
                let next = inputs.get(i + 1).cloned();
                cx.subscribe_in(input, window, move |ws, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        match &next {
                            Some(next) => next.update(cx, |n, cx| n.focus(window, cx)),
                            None => ws.submit_prompt(session, window, cx),
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        // 卡片在时下层视图拿到焦点（窗口重新激活、外部恢复焦点等）就转交给输入框
        let redirect = self
            .session_focus(session, cx)
            .filter(|_| !inputs.is_empty())
            .map(|focus| {
                cx.on_focus_in(&focus, window, move |ws, window, cx| {
                    ws.focus_prompt_input(session, window, cx);
                })
            });
        PanePrompt {
            prompt,
            inputs,
            remember: false,
            _events: events.into_iter().chain(redirect).collect(),
        }
    }

    /// 会话所在的终端视图或 SFTP 视图的焦点。
    fn session_focus(&self, session: SessionId, cx: &gpui_kit::App) -> Option<FocusHandle> {
        if let Some(index) = self.sftp_tab(session, cx) {
            return match &self.tabs[index].content {
                TabContent::Sftp(sftp) => Some(sftp.view.focus_handle(cx)),
                _ => None,
            };
        }
        let (tab, pane) = self.find_session(session)?;
        let view = &self.tabs[tab].terminals()?.pane(pane)?.view;
        Some(view.focus_handle(cx))
    }

    /// 聚焦会话提示卡片的第一个输入框；没有卡片或卡片无输入时返回 `false`。
    fn focus_prompt_input(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = self
            .prompt_slot(session, cx)
            .and_then(|p| p.as_ref())
            .and_then(|p| p.inputs.first().cloned());
        let Some(input) = input else {
            return false;
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        true
    }

    fn show_prompt(
        &mut self,
        tab: usize,
        pane: PaneId,
        session: SessionId,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card = self.build_prompt(session, prompt, window, cx);
        let Some(p) = self.pane_mut(tab, pane) else {
            card.prompt.answer(PromptAnswer::Cancel);
            return;
        };
        p.prompt = Some(card);
        let focused = self.tabs[tab]
            .terminals()
            .is_some_and(|t| t.focused == pane);
        if tab == self.active && focused {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// 会话的提示卡片所在的位置：终端窗格或 SFTP 标签。
    fn prompt_slot(
        &mut self,
        session: SessionId,
        cx: &gpui_kit::App,
    ) -> Option<&mut Option<PanePrompt>> {
        if let Some(index) = self.sftp_tab(session, cx) {
            return match &mut self.tabs[index].content {
                TabContent::Sftp(sftp) => Some(&mut sftp.prompt),
                _ => None,
            };
        }
        let (tab, pane) = self.find_session(session)?;
        self.pane_mut(tab, pane).map(|p| &mut p.prompt)
    }

    pub(crate) fn set_prompt_remember(
        &mut self,
        session: SessionId,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.prompt_slot(session, cx).and_then(Option::as_mut) {
            prompt.remember = remember;
            cx.notify();
        }
    }

    /// 以卡片上的输入作答。
    pub(crate) fn submit_prompt(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = self
            .prompt_slot(session, cx)
            .and_then(|p| p.as_ref())
            .map(|p| p.submission(cx));
        if let Some(answer) = answer {
            self.answer_prompt(session, answer, window, cx);
        }
    }

    /// 回答窗格内的问题并关闭卡片，焦点回到终端。
    pub(crate) fn answer_prompt(
        &mut self,
        session: SessionId,
        answer: PromptAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = self.prompt_slot(session, cx).and_then(Option::take) {
            prompt.prompt.answer(answer);
        }
        if let Some(index) = self.sftp_tab(session, cx) {
            if index == self.active {
                self.activate(index, window, cx);
            }
        } else if let Some((tab, pane)) = self.find_session(session)
            && tab == self.active
        {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// 当前标签上待回答问题所属的会话。
    fn active_prompt_session(&self, cx: &gpui_kit::App) -> Option<SessionId> {
        match &self.tabs.get(self.active)?.content {
            TabContent::Terminals(t) => t
                .focused_pane()
                .filter(|p| p.prompt.is_some())
                .map(|p| p.session.id()),
            TabContent::Sftp(sftp) => sftp
                .prompt
                .is_some()
                .then(|| sftp.view.read(cx).session().id()),
            TabContent::Settings(_) => None,
        }
    }

    fn find_session(&self, session: SessionId) -> Option<(usize, PaneId)> {
        self.tabs.iter().enumerate().find_map(|(i, t)| {
            t.terminals()
                .and_then(|t| t.pane_of_session(session))
                .map(|p| (i, p))
        })
    }

    /// 新窗格沿用当前窗格的工作目录：优先 OSC 7，其次进程信息。
    fn shell_here(&self, cx: &Context<Self>) -> LocalShell {
        let mut shell = config::local_shell(cx);
        shell.cwd = self
            .active_terminals()
            .and_then(Terminals::focused_pane)
            .filter(|p| matches!(p.launch, Launch::Local(_)))
            .and_then(|p| p.cwd.clone().or_else(|| p.session.process_cwd()));
        shell
    }

    /// 分屏时的新会话：SSH 窗格再连同一台主机，本地窗格沿用工作目录。
    fn launch_here(&self, cx: &Context<Self>) -> Launch {
        match self
            .active_terminals()
            .and_then(Terminals::focused_pane)
            .map(|p| &p.launch)
        {
            Some(ssh @ Launch::Ssh { .. }) => ssh.clone(),
            _ => Launch::Local(self.shell_here(cx)),
        }
    }

    /// 同步焦点：鼠标点击等直接聚焦了某个终端时，记为当前窗格。
    fn sync_focused_pane(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_terminals_mut() else {
            return;
        };
        if let Some(pane) = tab
            .panes
            .iter()
            .find(|p| p.view.focus_handle(cx).is_focused(window))
        {
            tab.focused = pane.id;
        }
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(target) = self.active_terminals().map(|t| t.focused) else {
            return;
        };
        let launch = self.launch_here(cx);
        let Some(pane) = self.spawn_pane(launch, window, cx) else {
            return;
        };
        let new = pane.id;
        let Some(tab) = self.active_terminals_mut() else {
            return;
        };
        tab.zoomed = None;
        tab.tree.split(target, new, axis);
        tab.panes.push(pane);
        self.focus_pane(new, window, cx);
    }

    fn move_focus(&mut self, dir: Direction, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(tab) = self.active_terminals() else {
            return;
        };
        if tab.zoomed.is_some() {
            return;
        }
        if let Some(next) = tab.tree.neighbor(tab.focused, dir) {
            self.focus_pane(next, window, cx);
        }
    }

    /// 用探测到的第 `index` 个 shell 配置档新建标签。
    pub(crate) fn open_profile(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(profile) = crate::shells::profiles(cx).get(index) {
            let shell = profile.to_shell();
            self.open_local_tab(shell, window, cx);
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let shell = self.shell_here(cx);
        self.open_local_tab(shell, window, cx);
    }

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// 拖放排序：把 `dragged` 移到 `target` 的位置，当前标签保持不变。
    fn move_tab(&mut self, dragged: TabId, target: TabId, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(from), Some(to)) = (self.tab_index(dragged), self.tab_index(target)) else {
            return;
        };
        if from == to {
            return;
        }
        let active = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.tab_index(active).unwrap_or(0);
        cx.notify();
    }

    fn start_rename(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else {
            return;
        };
        let current = tab.display_title();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let events = cx.subscribe_in(
            &input,
            window,
            move |ws, input, event, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let name = input.read(cx).value().trim().to_owned();
                    ws.finish_rename(Some(name), window, cx);
                }
                _ => {}
            },
        );
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(Rename {
            tab: id,
            input,
            _events: events,
        });
        cx.notify();
    }

    /// 结束重命名；`None` 为取消。名称为空时恢复自动标题。
    fn finish_rename(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming.take() else {
            return;
        };
        if let Some(name) = name
            && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == rename.tab)
        {
            tab.custom_title = (!name.is_empty()).then(|| name.into());
        }
        self.restore_focus(window, cx);
    }

    fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.close_tab_at(index, window, cx);
        }
    }

    fn close_focused_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        match self.active_terminals().map(|t| t.focused) {
            Some(pane) => self.close_pane(self.active, pane, window, cx),
            None => self.close_tab_at(self.active, window, cx),
        }
    }

    fn restart_focused(&mut self, _: &RestartTab, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        let Some(tab) = self.active_terminals() else {
            return;
        };
        if let Some(pane) = tab.focused_pane().filter(|p| p.status.ended()) {
            let id = pane.id;
            self.restart_pane(self.active, id, window, cx);
        }
    }

    fn activate_nth(&mut self, action: &ActivateTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate(action.0.min(self.tabs.len() - 1), window, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate((self.active + 1) % self.tabs.len(), window, cx);
        }
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            let len = self.tabs.len();
            self.activate((self.active + len - 1) % len, window, cx);
        }
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    fn toggle_zoom(&mut self, _: &ToggleZoom, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_focused_pane(window, cx);
        if let Some(tab) = self.active_terminals_mut()
            && tab.panes.len() > 1
        {
            tab.zoomed = match tab.zoomed {
                Some(_) => None,
                None => Some(tab.focused),
            };
            let focused = tab.focused;
            self.focus_pane(focused, window, cx);
        }
    }

    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((find, _)) = &self.find {
            find.update(cx, |find, cx| find.focus(window, cx));
            return;
        }
        self.sync_focused_pane(window, cx);
        let Some(view) = self
            .active_terminals()
            .and_then(Terminals::focused_pane)
            .map(|p| p.view.clone())
        else {
            return;
        };
        let find = cx.new(|cx| FindBar::new(view, window, cx));
        let sub = cx.subscribe_in(&find, window, |ws, _, FindEvent::Close, window, cx| {
            ws.find = None;
            ws.restore_focus(window, cx);
        });
        find.update(cx, |find, cx| find.focus(window, cx));
        self.find = Some((find, sub));
        cx.notify();
    }

    /// 在指定标签的当前窗格中执行一组自动操作步骤。
    fn run_tab_steps(&self, id: TabId, steps: Vec<scell_config::hosts::Step>) {
        if let Some(pane) = self
            .tabs
            .iter()
            .find(|t| t.id == id)
            .and_then(Tab::terminals)
            .and_then(Terminals::focused_pane)
            .filter(|p| p.status == PaneStatus::Running)
        {
            pane.session.run_steps(steps);
        }
    }

    fn toggle_palette(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.restore_focus(window, cx);
            return;
        }
        self.sync_focused_pane(window, cx);
        let active_host = self
            .active_terminals()
            .and_then(Terminals::focused_pane)
            .filter(|p| p.status == PaneStatus::Running)
            .and_then(|p| match &p.launch {
                Launch::Ssh { host, .. } => Some(host.clone()),
                Launch::Local(_) => None,
            });
        let palette = cx.new(|cx| Palette::new(mode, active_host, window, cx));
        let sub = cx.subscribe_in(&palette, window, |ws, _, event, window, cx| match event {
            PaletteEvent::Close => {
                ws.palette = None;
                ws.restore_focus(window, cx);
            }
            PaletteEvent::OpenProfile(index) => ws.open_profile(*index, window, cx),
            PaletteEvent::ConnectHost(id) => ws.open_host(id, window, cx),
            PaletteEvent::RunSteps(steps) => {
                if let Some(id) = ws.tabs.get(ws.active).map(|t| t.id) {
                    ws.run_tab_steps(id, steps.clone());
                }
            }
        });
        palette.update(cx, |p, cx| p.focus(window, cx));
        self.palette = Some((palette, sub));
        cx.notify();
    }

    /// 把焦点还给当前标签的内容。
    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.active, window, cx);
        cx.notify();
    }

    fn toggle_context_panel(
        &mut self,
        _: &ToggleContextPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_open && !self.context_fits(window) && !self.sidebar_collapsed {
            // 面板因窗口太窄被临时收起，此时切换是想看它：收起侧栏腾出位置
            self.sidebar_collapsed = true;
        } else {
            self.context_open = !self.context_open;
        }
        cx.notify();
    }
}

fn save_layout(tree: &pane_tree::PaneTree, t: &Terminals) -> SavedLayout {
    match tree {
        pane_tree::PaneTree::Split { axis, children, .. } => SavedLayout::Split {
            row: *axis == Axis::Row,
            children: children.iter().map(|c| save_layout(c, t)).collect(),
        },
        pane_tree::PaneTree::Leaf(id) => match t.pane(*id).map(|p| (p, &p.launch)) {
            Some((_, Launch::Ssh { host, .. })) => SavedLayout::Ssh { host: host.clone() },
            Some((p, Launch::Local(shell))) => SavedLayout::Local {
                program: shell.program.clone(),
                args: shell.args.clone(),
                cwd: p
                    .cwd
                    .clone()
                    .or_else(|| p.session.process_cwd())
                    .or_else(|| shell.cwd.clone()),
            },
            None => SavedLayout::Local {
                program: None,
                args: Vec::new(),
                cwd: None,
            },
        },
    }
}

/// 由主机库解析出连接目标；跳板链引用缺失或成环时返回原因。
fn ssh_target(host: &HostId, cx: &gpui_kit::App) -> Result<SshTarget, String> {
    let book = ConfigState::hosts(cx);
    let profile = book
        .get(host)
        .ok_or_else(|| t!("hosts.missing").to_string())?;
    let jumps = book
        .jump_chain(profile)
        .map_err(|_| t!("hosts.bad_jump", name = profile.name.clone()).to_string())?
        .into_iter()
        .cloned()
        .collect();
    Ok(SshTarget {
        host: profile.clone(),
        jumps,
        known_hosts: ConfigState::known_hosts(cx),
    })
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.size = Some(window.viewport_size());
        crate::blur::sync(
            window,
            Appearance::material(cx) == scell_config::settings::WindowMaterial::Blur,
        );
        self.context_shown = self.context_open && self.context_fits(window);
        self.sync_focused_pane(window, cx);
        self.sync_context(cx);
        let title = self
            .tabs
            .get(self.active)
            .map(Tab::display_title)
            .unwrap_or_default();
        window.set_window_title(&title);

        // 两侧面板固定尺寸：窗口或侧栏宽度变化时只由中间内容区伸缩
        let main = panes::row("workspace-content")
            .child(resizable_panel().child(panes::render_active_tab(self, cx)))
            .child(
                resizable_panel()
                    .fixed()
                    .size(metrics::CONTEXT_PANEL_WIDTH)
                    .size_range(metrics::CONTEXT_PANEL_MIN..metrics::CONTEXT_PANEL_MAX)
                    .visible(self.context_shown)
                    .child(self.context.clone()),
            );
        let content = panes::column("workspace-bottom")
            .child(resizable_panel().child(main))
            .child(
                resizable_panel()
                    .fixed()
                    .size(metrics::BOTTOM_PANEL_HEIGHT)
                    .size_range(metrics::BOTTOM_PANEL_MIN..metrics::BOTTOM_PANEL_MAX)
                    .visible(self.transfers_open)
                    .child(panes::bottom_panel(crate::files::transfer_list(cx), cx)),
            );

        v_flex()
            .relative()
            .size_full()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::close_focused_pane))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::activate_nth))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_context_panel))
            .on_action(cx.listener(|ws, _: &ToggleTransfers, _, cx| {
                ws.transfers_open = !ws.transfers_open;
                cx.notify();
            }))
            .on_action(cx.listener(Self::toggle_zoom))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(|ws, _: &TogglePalette, window, cx| {
                ws.toggle_palette(PaletteMode::All, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &QuickConnect, window, cx| {
                ws.toggle_palette(PaletteMode::Hosts, window, cx)
            }))
            .on_action(cx.listener(Self::restart_focused))
            .on_action(cx.listener(|ws, _: &MoveToNewWindow, window, cx| {
                let Some(id) = ws.tabs.get(ws.active).map(|t| t.id) else {
                    return;
                };
                // 移动要更新本工作区，推迟到当前更新结束之后
                let me = cx.entity().downgrade();
                window.defer(cx, move |window, cx| {
                    crate::app::move_tab(&me, id, None, window, cx)
                });
            }))
            .on_action(cx.listener(|ws, _: &RenameTab, window, cx| {
                if let Some(id) = ws.tabs.get(ws.active).map(|t| t.id) {
                    // 行内输入框只在展开的侧栏里
                    ws.sidebar_collapsed = false;
                    ws.sidebar_view = SidebarView::Sessions;
                    ws.start_rename(id, window, cx);
                }
            }))
            .on_action(cx.listener(|ws, _: &OpenSftp, window, cx| {
                if let Some(id) = ws.tabs.get(ws.active).map(|t| t.id) {
                    ws.open_tab_sftp(id, window, cx);
                }
            }))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(|_, _: &NewWindow, _, cx| {
                let shell = config::local_shell(cx);
                if let Err(err) = crate::app::open_window(Start::Shell(shell), cx) {
                    tracing::error!(%err, "无法打开新窗口");
                }
            }))
            .on_action(cx.listener(|ws, _: &input::Escape, window, cx| {
                if ws.renaming.is_some() {
                    ws.finish_rename(None, window, cx);
                } else if let Some(session) = ws.active_prompt_session(cx) {
                    ws.answer_prompt(session, PromptAnswer::Cancel, window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &IncreaseFontSize, _, cx| appearance::zoom(Some(1.), cx)))
            .on_action(
                cx.listener(|_, _: &DecreaseFontSize, _, cx| appearance::zoom(Some(-1.), cx)),
            )
            .on_action(cx.listener(|_, _: &ResetFontSize, _, cx| appearance::zoom(None, cx)))
            .on_action(cx.listener(|ws, _: &SplitRight, w, cx| ws.split(Axis::Row, w, cx)))
            .on_action(cx.listener(|ws, _: &SplitDown, w, cx| ws.split(Axis::Column, w, cx)))
            .on_action(
                cx.listener(|ws, _: &FocusLeft, w, cx| ws.move_focus(Direction::Left, w, cx)),
            )
            .on_action(
                cx.listener(|ws, _: &FocusRight, w, cx| ws.move_focus(Direction::Right, w, cx)),
            )
            .on_action(cx.listener(|ws, _: &FocusUp, w, cx| ws.move_focus(Direction::Up, w, cx)))
            .on_action(
                cx.listener(|ws, _: &FocusDown, w, cx| ws.move_focus(Direction::Down, w, cx)),
            )
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .size_full()
                    .child(sidebar::render(self, window, cx))
                    .child(div().flex_1().h_full().min_w_0().child(content)),
            )
            .children(self.palette.as_ref().map(|(palette, _)| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(metrics::PALETTE_TOP)
                    .child(motion::rise_in(
                        div().relative().child(palette.clone()),
                        "palette",
                        -metrics::ENTER_OFFSET,
                        |el, offset| el.top(offset),
                        cx,
                    ))
            }))
    }
}
