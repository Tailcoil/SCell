//! 设置的加载、热重载与错误提示。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::component::WindowExt;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Global};
use scell_config::settings::{ThemeChoice, Value, WindowMaterial, spelling, write_value};
use scell_config::{
    ConfigChange, ConfigPaths, ConfigWatcher, HostBook, HostId, SavedState, Settings,
};
use scell_session::LocalShell;

use crate::appearance;

/// 一次保存会触发多个文件事件，合并这段时间内的事件后再重载。
const DEBOUNCE: Duration = Duration::from_millis(150);

pub(crate) struct ConfigState {
    pub(crate) paths: Option<ConfigPaths>,
    pub(crate) settings: Settings,
    pub(crate) hosts: HostBook,
    pub(crate) saved: SavedState,
    /// 窗口打开前产生的错误，等有窗口时再显示。
    pending_errors: Vec<String>,
    _watcher: Option<ConfigWatcher>,
}

impl Global for ConfigState {}

impl ConfigState {
    pub(crate) fn settings(cx: &App) -> &Settings {
        &cx.global::<Self>().settings
    }

    pub(crate) fn hosts(cx: &App) -> &HostBook {
        &cx.global::<Self>().hosts
    }

    /// 最近连接的主机，最近的在前；可能含已删除的主机。
    pub(crate) fn recent_hosts(cx: &App) -> &[HostId] {
        &cx.global::<Self>().saved.recent_hosts
    }

    /// 主机密钥记录文件。
    /// 设置中的路径可以用 `~/` 开头。
    pub(crate) fn known_hosts(cx: &App) -> PathBuf {
        match &Self::settings(cx).ssh.known_hosts {
            Some(path) => scell_config::expand_tilde(path),
            None => default_known_hosts(),
        }
    }

    pub(crate) fn themes_dir(cx: &App) -> Option<PathBuf> {
        cx.global::<Self>()
            .paths
            .as_ref()
            .map(ConfigPaths::themes_dir)
    }
}

pub(crate) fn init(cx: &mut App) {
    let paths = ConfigPaths::discover();
    let mut errors = Vec::new();
    let settings = match &paths {
        Some(p) => Settings::load(&p.settings()).unwrap_or_else(|e| {
            errors.push(e.to_string());
            Settings::default()
        }),
        None => Settings::default(),
    };
    let hosts = match &paths {
        Some(p) => HostBook::load(&p.hosts()).unwrap_or_else(|e| {
            errors.push(e.to_string());
            HostBook::default()
        }),
        None => HostBook::default(),
    };
    let saved = paths
        .as_ref()
        .map(|p| SavedState::load(p.state()))
        .unwrap_or_default();

    let watcher = paths.as_ref().and_then(|p| match ConfigWatcher::start(p) {
        Ok((watcher, rx)) => {
            spawn_reloader(rx, cx);
            Some(watcher)
        }
        Err(e) => {
            tracing::warn!(%e, "配置热重载不可用");
            None
        }
    });

    for e in &errors {
        tracing::warn!("{e}");
    }
    cx.set_global(ConfigState {
        paths,
        settings,
        hosts,
        saved,
        pending_errors: errors,
        _watcher: watcher,
    });
}

fn spawn_reloader(rx: flume::Receiver<ConfigChange>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(first) = rx.recv_async().await {
            cx.background_executor().timer(DEBOUNCE).await;
            let mut changes = vec![first];
            changes.extend(rx.try_iter());
            cx.update(|cx| {
                reload(
                    changes.contains(&ConfigChange::Settings),
                    changes.contains(&ConfigChange::Themes),
                    cx,
                );
                if changes.contains(&ConfigChange::Hosts) {
                    reload_hosts(cx);
                }
            });
        }
    })
    .detach();
}

fn reload(settings: bool, themes: bool, cx: &mut App) {
    tracing::debug!(settings, themes, "重载配置");
    let mut errors = Vec::new();
    if settings
        && let Some(path) = cx
            .global::<ConfigState>()
            .paths
            .as_ref()
            .map(|p| p.settings())
    {
        match Settings::load(&path) {
            Ok(s) => {
                let keys = s.keys.clone();
                cx.global_mut::<ConfigState>().settings = s;
                errors.extend(crate::actions::apply_user_keys(&keys, cx));
            }
            // 保留上一次有效的设置，避免编辑过程中的半成品文件打乱界面
            Err(e) => errors.push(e.to_string()),
        }
    }
    if themes {
        errors.extend(appearance::reload_themes(cx));
    }
    appearance::apply(cx);
    report_errors(errors, cx);
}

fn reload_hosts(cx: &mut App) {
    let Some(path) = cx
        .global::<ConfigState>()
        .paths
        .as_ref()
        .map(ConfigPaths::hosts)
    else {
        return;
    };
    match HostBook::load(&path) {
        Ok(book) => {
            // 自己刚写入的内容会再触发一次重载，相同时不必刷新
            if book != cx.global::<ConfigState>().hosts {
                cx.global_mut::<ConfigState>().hosts = book;
                cx.refresh_windows();
            }
        }
        Err(e) => report_errors(vec![e.to_string()], cx),
    }
}

/// 修改主机库：立即生效并写回 `hosts.toml`。
pub(crate) fn update_hosts(mutate: impl FnOnce(&mut HostBook), cx: &mut App) {
    mutate(&mut cx.global_mut::<ConfigState>().hosts);
    cx.refresh_windows();
    let state = cx.global::<ConfigState>();
    let Some(path) = state.paths.as_ref().map(ConfigPaths::hosts) else {
        return;
    };
    if let Err(e) = state.hosts.save(&path) {
        report_errors(vec![e.to_string()], cx);
    }
}

pub(crate) fn toggle_favorite(id: &HostId, cx: &mut App) {
    update_hosts(
        |book| {
            if let Some(h) = book.hosts.iter_mut().find(|h| &h.id == id) {
                h.favorite = !h.favorite;
            }
        },
        cx,
    );
}

/// 记一次主机连接，在后台写回运行状态。
pub(crate) fn touch_recent_host(id: &HostId, cx: &mut App) {
    let state = cx.global_mut::<ConfigState>();
    state.saved.touch_host(id);
    let Some(path) = state.paths.as_ref().map(|p| p.state().to_owned()) else {
        return;
    };
    let saved = state.saved.clone();
    // 后台任务的执行顺序不确定，只写最新一次的快照，免得旧快照后写覆盖新的
    static LATEST: AtomicU64 = AtomicU64::new(0);
    static WRITING: Mutex<()> = Mutex::new(());
    let seq = LATEST.fetch_add(1, Ordering::AcqRel) + 1;
    cx.background_executor()
        .spawn(async move {
            let _guard = WRITING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if LATEST.load(Ordering::Acquire) != seq {
                return;
            }
            if let Err(e) = saved.save(&path) {
                tracing::warn!("保存运行状态失败：{e}");
            }
        })
        .detach();
}

/// 同步写入：退出时调用，后台任务来不及执行。
pub(crate) fn save_windows(windows: Vec<scell_config::SavedWindow>, cx: &mut App) {
    let state = cx.global_mut::<ConfigState>();
    state.saved.windows = windows;
    let Some(path) = state.paths.as_ref().map(|p| p.state().to_owned()) else {
        return;
    };
    if let Err(e) = state.saved.save(&path) {
        tracing::warn!("保存运行状态失败：{e}");
    }
}

/// 修改一项设置：先改内存中的值并立即生效，再写回设置文件（热重载随后读到同样的值）。
/// `value` 是写入 `[table]` 下 `key` 的值，`None` 表示删除该键、恢复默认。
pub(crate) fn update(
    table: &str,
    key: &str,
    value: Option<Value>,
    mutate: impl FnOnce(&mut Settings),
    cx: &mut App,
) {
    mutate(&mut cx.global_mut::<ConfigState>().settings);
    appearance::apply(cx);
    let Some(path) = cx
        .global::<ConfigState>()
        .paths
        .as_ref()
        .map(ConfigPaths::settings)
    else {
        return;
    };
    if let Err(e) = write_value(&path, table, key, value) {
        report_errors(vec![e.to_string()], cx);
    }
}

pub(crate) fn set_theme(choice: ThemeChoice, cx: &mut App) {
    let value = match &choice {
        ThemeChoice::Fixed(name) => Value::from(name.as_str()),
        ThemeChoice::System { light, dark } => {
            let mut table = scell_config::settings::InlineTable::new();
            table.insert("light", light.as_str().into());
            table.insert("dark", dark.as_str().into());
            Value::InlineTable(table)
        }
    };
    update(
        "appearance",
        "theme",
        Some(value),
        |s| s.appearance.theme = choice,
        cx,
    );
}

pub(crate) fn set_material(material: WindowMaterial, cx: &mut App) {
    update(
        "appearance",
        "window",
        Some(spelling(&material).into()),
        |s| s.appearance.window = material,
        cx,
    );
}

/// 用系统默认程序打开设置文件；不存在时先写入默认值模板。
pub(crate) fn open_settings_file(cx: &mut App) {
    let Some(path) = cx
        .global::<ConfigState>()
        .paths
        .as_ref()
        .map(ConfigPaths::settings)
    else {
        return;
    };
    if !path.exists() {
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, Settings::template()));
        if let Err(e) = written {
            report_errors(vec![format!("{}: {e}", path.display())], cx);
            return;
        }
    }
    cx.open_with_system(&path);
}

pub(crate) fn open_themes_dir(cx: &mut App) {
    if let Some(dir) = ConfigState::themes_dir(cx) {
        let _ = std::fs::create_dir_all(&dir);
        cx.open_with_system(&dir);
    }
}

/// 设置中的默认 shell。
pub(crate) fn local_shell(cx: &App) -> LocalShell {
    let shell = &ConfigState::settings(cx).shell;
    LocalShell {
        program: shell.program.as_ref().map(PathBuf::from),
        args: shell.args.clone(),
        ..LocalShell::default()
    }
}

/// 在任一窗口显示错误；还没有窗口时暂存。
pub(crate) fn report_errors(errors: Vec<String>, cx: &mut App) {
    for e in &errors {
        tracing::warn!("{e}");
    }
    notify_errors(errors, cx);
}

/// 还没有窗口时先存着，等第一个窗口打开后由 [`flush_pending_errors`] 显示。
fn notify_errors(errors: Vec<String>, cx: &mut App) {
    if errors.is_empty() {
        return;
    }
    let Some(window) = cx.windows().into_iter().next() else {
        cx.global_mut::<ConfigState>().pending_errors.extend(errors);
        return;
    };
    let _ = window.update(cx, |_, window, cx| {
        for e in errors {
            window.push_notification(Notification::error(e), cx);
        }
    });
}

pub(crate) fn flush_pending_errors(cx: &mut App) {
    let errors = std::mem::take(&mut cx.global_mut::<ConfigState>().pending_errors);
    notify_errors(errors, cx);
}

/// 收藏在前，其次最近使用，其余按主机库顺序。
pub(crate) fn host_order(cx: &App) -> Vec<HostId> {
    let book = ConfigState::hosts(cx);
    let recent = ConfigState::recent_hosts(cx);
    let mut hosts: Vec<_> = book.hosts.iter().collect();
    hosts.sort_by_key(|h| {
        let rank = recent
            .iter()
            .position(|id| *id == h.id)
            .unwrap_or(usize::MAX);
        (!h.favorite, rank)
    });
    hosts.into_iter().map(|h| h.id.clone()).collect()
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

/// 与 OpenSSH 共用的主机密钥文件。
pub(crate) fn default_known_hosts() -> PathBuf {
    home_dir().join(".ssh").join("known_hosts")
}

/// 显示用：家目录下的路径写成 `~/…`。
pub(crate) fn tilde(path: &Path) -> String {
    let home = home_dir();
    match path.strip_prefix(&home) {
        Ok(_) if home.as_os_str().is_empty() => path.display().to_string(),
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => path.display().to_string(),
    }
}
