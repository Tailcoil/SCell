use std::collections::HashMap;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Global, Result, SharedString, WeakEntity,
    Window, WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use scell_config::SavedWindow;
use scell_session::{LocalShell, SessionEvent, SessionId, SessionService};
use scell_term_view::TerminalView;

use crate::appearance::Appearance;
use crate::config::ConfigState;
use crate::workspace::{Start, Workspace};
use crate::{actions, appearance, config, shells};

/// 应用级全局状态：会话服务与“会话 → 视图”的路由表。
pub(crate) struct AppState {
    pub(crate) sessions: SessionService,
    views: HashMap<SessionId, WeakEntity<TerminalView>>,
    workspaces: Vec<(AnyWindowHandle, WeakEntity<Workspace>)>,
    /// 最后关闭的窗口的布局：关掉最后一个窗口即退出，退出时它已不在 `workspaces` 里。
    pub(crate) last_closed: Option<SavedWindow>,
}

impl Global for AppState {}

impl AppState {
    pub(crate) fn register_view(&mut self, id: SessionId, view: WeakEntity<TerminalView>) {
        self.views.insert(id, view);
    }

    pub(crate) fn unregister_view(&mut self, id: SessionId) {
        self.views.remove(&id);
    }

    pub(crate) fn views(&self) -> Vec<Entity<TerminalView>> {
        self.views
            .values()
            .filter_map(WeakEntity::upgrade)
            .collect()
    }
}

pub fn init(
    cx: &mut App,
    sessions: SessionService,
    events: flume::Receiver<SessionEvent>,
    font: crate::BundledFont,
) {
    // 先按系统语言，读取配置时产生的提示也能本地化；设置里另选了语言时由外观应用时切换
    apply_locale(None);
    config::init(cx);
    crate::fonts::init(cx, font);
    appearance::init(cx);
    crate::fonts::prewarm(cx);
    shells::init(cx);
    let settings = ConfigState::settings(cx);
    sessions.set_scrollback(settings.terminal.scrollback);
    sessions.set_auto_reconnect(settings.ssh.auto_reconnect);
    scell_term_view::init(cx);
    actions::bind_default_keys(cx);
    cx.set_global(AppState {
        sessions,
        views: HashMap::new(),
        workspaces: Vec::new(),
        last_closed: None,
    });
    cx.on_app_quit(|cx| {
        save_layout(cx);
        async {}
    })
    .detach();
    crate::files::init(cx);
    let keys = ConfigState::settings(cx).keys.clone();
    let errors = actions::apply_user_keys(&keys, cx);
    config::report_errors(errors, cx);

    cx.on_window_closed(|cx, window| {
        crate::blur::forget(window);
        cx.global_mut::<AppState>()
            .workspaces
            .retain(|(_, ws)| ws.upgrade().is_some());
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv_async().await {
            cx.update(|cx| route_event(event, cx));
        }
    })
    .detach();
}

/// 界面提供的语言，及各自以本语言写的名称。
pub(crate) const LANGUAGES: [(&str, &str); 2] = [("zh-CN", "简体中文"), ("en", "English")];

/// 系统语言对应的界面语言：中文环境用 zh-CN，其余用英文。
pub(crate) fn system_locale() -> &'static str {
    let zh = sys_locale::get_locale().is_some_and(|l| l.to_ascii_lowercase().starts_with("zh"));
    if zh { "zh-CN" } else { "en" }
}

/// 切换界面语言，`None` 跟随系统；组件库与本 crate 的文案同时切换。返回是否有变化。
pub(crate) fn apply_locale(choice: Option<&str>) -> bool {
    let locale = choice
        .and_then(|c| LANGUAGES.iter().find(|(id, _)| *id == c))
        .map_or_else(system_locale, |(id, _)| *id);
    if &*rust_i18n::locale() == locale {
        return false;
    }
    rust_i18n::set_locale(locale);
    gpui_kit::component::set_locale(locale);
    true
}

fn route_event(event: SessionEvent, cx: &mut App) {
    if let SessionEvent::Wakeup(id) = event {
        let state = cx.global::<AppState>();
        // 视图已释放时也要确认，否则该会话不会再产生唤醒。
        // 先确认再重绘：确认之后到达的输出会触发新的唤醒
        if let Some(handle) = state.sessions.get(id) {
            handle.ack_wakeup();
        }
        if let Some(view) = state.views.get(&id).and_then(WeakEntity::upgrade) {
            view.update(cx, |_, cx| cx.notify());
        }
        return;
    }
    let workspaces = cx.global::<AppState>().workspaces.clone();
    for (window, ws) in workspaces {
        let _ = window.update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| ws.on_session_event(&event, window, cx))
        });
    }
}

/// 退出时记下仍打开的窗口；全部关掉后退出的，记最后关闭的那个。
fn save_layout(cx: &mut App) {
    let state = cx.global::<AppState>();
    let mut windows: Vec<SavedWindow> = state
        .workspaces
        .iter()
        .filter_map(|(_, ws)| ws.upgrade())
        .map(|ws| ws.read(cx).snapshot(cx))
        .filter(|w| !w.tabs.is_empty())
        .collect();
    if windows.is_empty() {
        windows.extend(state.last_closed.clone());
    }
    config::save_windows(windows, cx);
}

/// 指定了 `first_shell` 时只开一个运行它的窗口；否则按设置恢复上次的窗口，没有可恢复的时用默认 shell。
pub fn open_main_window(first_shell: Option<LocalShell>, cx: &mut App) -> Result<()> {
    let result = open_startup_windows(first_shell, cx);
    // 窗口在 open_window 返回前已画完首帧，此时再加载回退字体不会拖慢它
    crate::fonts::prewarm_fallbacks(cx);
    result
}

fn open_startup_windows(first_shell: Option<LocalShell>, cx: &mut App) -> Result<()> {
    if let Some(shell) = first_shell {
        return open_window(Start::Shell(shell), cx);
    }
    let saved = match ConfigState::settings(cx).shell.restore_session {
        true => cx.global::<ConfigState>().saved.windows.clone(),
        false => Vec::new(),
    };
    let mut opened = false;
    for window in saved {
        match open_window(Start::Restore(window), cx) {
            Ok(()) => opened = true,
            Err(err) => tracing::error!(%err, "无法恢复窗口"),
        }
    }
    if opened {
        return Ok(());
    }
    open_window(Start::Shell(config::local_shell(cx)), cx)
}

/// 除 `current` 外的主窗口，附显示名称。
pub(crate) fn other_windows(
    current: AnyWindowHandle,
    cx: &App,
) -> Vec<(AnyWindowHandle, WeakEntity<Workspace>, SharedString)> {
    cx.global::<AppState>()
        .workspaces
        .iter()
        .filter(|(window, _)| *window != current)
        .filter_map(|(window, ws)| {
            let label = ws.upgrade()?.read(cx).window_label(cx);
            Some((*window, ws.clone(), label))
        })
        .collect()
}

/// 把 `source` 中的标签移到 `target` 窗口；`target` 为空时新开一个窗口。
pub(crate) fn move_tab(
    source: &WeakEntity<Workspace>,
    tab: crate::workspace::TabId,
    target: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(Some(moved)) = source.update(cx, |ws, cx| ws.take_tab(tab, window, cx)) else {
        return;
    };
    let result = match target {
        Some((handle, ws)) => handle
            .update(cx, |_, window, cx| {
                ws.update(cx, |ws, cx| ws.attach_tab(moved, window, cx))
            })
            .and_then(|r| r),
        None => open_window(Start::Tab(moved), cx),
    };
    if let Err(err) = result {
        tracing::error!(%err, "无法移动标签");
    }
}

pub(crate) fn open_window(start: Start, cx: &mut App) -> Result<()> {
    let (width, height) = match &start {
        Start::Restore(saved) => saved.size.unwrap_or((1100., 700.)),
        _ => (1100., 700.),
    };
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // 侧栏展开时内容区至少能完整放下设置页的标题栏与表单
        window_min_size: Some(crate::ui::metrics::WINDOW_MIN_SIZE),
        app_id: Some("scell".into()),
        // 标题栏由 TitleBar 自绘；不显式请求时 KDE 会叠加一条服务端标题栏
        window_decorations: Some(WindowDecorations::Client),
        window_background: Appearance::window_background(cx),
        ..TitleBar::window_options()
    };
    let (window, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(start, window, cx))
    })?;
    cx.global_mut::<AppState>()
        .workspaces
        .push((window, workspace.downgrade()));
    // 部分 Linux 桌面上应用级的明暗查询不准确，以窗口为准
    if let Ok(dark) = window.update(cx, |_, window, _| appearance::is_dark(window.appearance())) {
        appearance::set_system_dark(dark, cx);
    }
    config::flush_pending_errors(cx);
    Ok(())
}
