//! 检查更新，只由关于页手动触发，不在后台联网。
//! 经 Velopack 安装（Setup 或便携包）的 Windows 版在程序内下载、重启安装；其他安装方式（Linux 的各种包、
//! 直接运行的构建产物）只比较 GitHub 上最新 Release 的版本号，有新版时打开它的页面。

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Global};
use rust_i18n::t;

const REPO: &str = env!("CARGO_PKG_REPOSITORY");
/// 检查只请求一个很小的文件或一次跳转。
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// 关于页更新按钮所处的步骤。
#[derive(Clone, Default)]
pub(crate) enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Arc<Update>),
    /// 下载进度，0–100。
    Downloading(u8),
    /// 已下载，重启后安装。
    Ready(Arc<Update>),
}

impl Global for UpdateState {}

pub(crate) struct Update {
    pub(crate) version: String,
    target: Target,
}

enum Target {
    /// Velopack 的更新包，在程序内下载安装。
    #[cfg_attr(not(windows), expect(dead_code, reason = "只有 Windows 版有更新包"))]
    Package(Box<platform::Package>),
    /// Release 页面，交给浏览器。
    Page(String),
}

impl Update {
    /// 是否在程序内安装；否则打开 Release 页面。
    pub(crate) fn in_app(&self) -> bool {
        matches!(self.target, Target::Package(_))
    }
}

/// Velopack 安装、更新、卸载时会带专用参数启动程序，须在解析命令行之前调用；遇到这些参数时处理完直接退出。
pub fn run_installer_hooks() {
    platform::run_hooks();
}

pub(crate) fn state(cx: &App) -> UpdateState {
    cx.try_global::<UpdateState>().cloned().unwrap_or_default()
}

/// 点击按钮：按当前步骤检查、下载、重启安装，或打开 Release 页面。
pub(crate) fn advance(cx: &mut App) {
    match state(cx) {
        UpdateState::Idle | UpdateState::UpToDate => check(cx),
        UpdateState::Available(update) => match &update.target {
            Target::Package(_) => download(update.clone(), cx),
            Target::Page(url) => cx.open_url(url),
        },
        UpdateState::Ready(update) => {
            // 成功时进程随即退出，由 Velopack 安装后重新启动
            if let Target::Package(package) = &update.target
                && let Err(err) = platform::apply_and_restart(package)
            {
                fail(&err, UpdateState::Ready(update.clone()), cx);
            }
        }
        UpdateState::Checking | UpdateState::Downloading(_) => {}
    }
}

fn set(state: UpdateState, cx: &mut App) {
    cx.set_global(state);
    cx.refresh_windows();
}

fn check(cx: &mut App) {
    set(UpdateState::Checking, cx);
    let task = cx.background_executor().spawn(async {
        if platform::installed() {
            platform::check()
        } else {
            latest_release()
        }
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(Some(update)) => set(UpdateState::Available(Arc::new(update)), cx),
            Ok(None) => set(UpdateState::UpToDate, cx),
            Err(err) => fail(&err, UpdateState::Idle, cx),
        });
    })
    .detach();
}

fn download(update: Arc<Update>, cx: &mut App) {
    set(UpdateState::Downloading(0), cx);
    let (progress, percents) = flume::unbounded();
    let task = cx.background_executor().spawn({
        let update = update.clone();
        async move {
            match &update.target {
                Target::Package(package) => platform::download(package, progress),
                Target::Page(_) => Ok(()),
            }
        }
    });
    cx.spawn(async move |cx| {
        while let Ok(percent) = percents.recv_async().await {
            cx.update(|cx| {
                if matches!(state(cx), UpdateState::Downloading(_)) {
                    set(UpdateState::Downloading(percent), cx);
                }
            });
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(()) => set(UpdateState::Ready(update), cx),
            Err(err) => fail(&err, UpdateState::Available(update), cx),
        });
    })
    .detach();
}

fn fail(err: &str, back: UpdateState, cx: &mut App) {
    tracing::warn!("更新失败：{err}");
    let text = t!("settings.update_failed", err = err).into_owned();
    crate::overlays::show_notification(Notification::error(text), cx);
    set(back, cx);
}

/// 最新正式 Release：`/releases/latest` 跳到 `/releases/tag/<标签>`，还没有 Release 时跳到 `/releases`。
/// 只读跳转地址，不走 GitHub API，没有频率限制。
fn latest_release() -> Result<Option<Update>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(CHECK_TIMEOUT))
        .max_redirects(0)
        .build()
        .into();
    let response = agent
        .get(format!("{REPO}/releases/latest"))
        .call()
        .map_err(|e| e.to_string())?;
    let page = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let Some((_, tag)) = page.rsplit_once("/releases/tag/") else {
        return Ok(None);
    };
    let version = tag.trim_start_matches('v');
    Ok(newer(version, env!("CARGO_PKG_VERSION")).then(|| Update {
        version: version.to_owned(),
        target: Target::Page(page.to_owned()),
    }))
}

/// 只比较主、次、修订号；预发布不会成为最新 Release。
fn newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        Some((parts.next()??, parts.next()??, parts.next()??))
    };
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

#[cfg(windows)]
mod platform {
    use std::sync::OnceLock;
    use std::time::Duration;

    use velopack::sources::HttpSource;
    use velopack::{HttpOptions, UpdateCheck, UpdateInfo, UpdateManager, VelopackApp};

    use super::{CHECK_TIMEOUT, REPO, Target, Update};

    pub(super) type Package = UpdateInfo;

    /// 超时后放开 Velopack 的更新锁，不必重启就能重试；GitHub 访问不稳时连接可能一直挂着。
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

    pub(super) fn run_hooks() {
        VelopackApp::build().run();
    }

    /// 更新源是最新 Release 的附件；GitHub 的 `latest/download/<文件名>` 会跳到对应文件。
    fn manager(timeout: Duration) -> Result<UpdateManager, String> {
        let options = HttpOptions {
            TimeoutMilliseconds: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            ..HttpOptions::default()
        };
        let source =
            HttpSource::new_with_options(format!("{REPO}/releases/latest/download"), options);
        UpdateManager::new(source, None, None).map_err(|e| e.to_string())
    }

    /// 直接运行构建产物时没有 Velopack 的安装信息。
    pub(super) fn installed() -> bool {
        static INSTALLED: OnceLock<bool> = OnceLock::new();
        *INSTALLED.get_or_init(|| manager(CHECK_TIMEOUT).is_ok())
    }

    pub(super) fn check() -> Result<Option<Update>, String> {
        let check = manager(CHECK_TIMEOUT)?
            .check_for_updates()
            .map_err(|e| e.to_string())?;
        Ok(match check {
            UpdateCheck::UpdateAvailable(package) => Some(Update {
                version: package.TargetFullRelease.Version.clone(),
                target: Target::Package(package),
            }),
            UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => None,
        })
    }

    /// 中断时只留下 `packages` 下的 `.partial` 文件，已安装的版本不受影响，下次下载会先清掉它。
    pub(super) fn download(package: &Package, progress: flume::Sender<u8>) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel::<i16>();
        std::thread::spawn(move || {
            for percent in rx {
                let _ = progress.send(u8::try_from(percent.clamp(0, 100)).unwrap_or(0));
            }
        });
        manager(DOWNLOAD_TIMEOUT)?
            .download_updates(package, Some(tx))
            .map_err(|e| e.to_string())
    }

    pub(super) fn apply_and_restart(package: &Package) -> Result<(), String> {
        manager(CHECK_TIMEOUT)?
            .apply_updates_and_restart(package)
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Update;

    /// 不会出现：这些平台没有程序内安装。
    pub(super) enum Package {}

    pub(super) fn run_hooks() {}

    pub(super) fn installed() -> bool {
        false
    }

    pub(super) fn check() -> Result<Option<Update>, String> {
        Ok(None)
    }

    pub(super) fn download(package: &Package, _: flume::Sender<u8>) -> Result<(), String> {
        match *package {}
    }

    pub(super) fn apply_and_restart(package: &Package) -> Result<(), String> {
        match *package {}
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_release_versions() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.2.0-rc.1", "0.2.0"));
        assert!(!newer("latest", "0.1.0"));
    }
}
