//! SCell 组合根：解析参数、初始化日志与运行时，再把控制权交给 GPUI。

// Windows 上作为图形程序启动，不附带控制台窗口；日志见日志目录
#![cfg_attr(windows, windows_subsystem = "windows")]

mod gpu;

use anyhow::Context as _;
use clap::Parser;
use scell_session::{LocalShell, SessionService};
use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

/// 保留的日志文件数（按天滚动）。
const LOG_FILES: usize = 7;

// 帮助文本用英文，与 clap 自带的 Usage、Options 等提示保持一致
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(
        long,
        env = "SCELL_LOG",
        default_value = "info",
        value_name = "FILTER",
        help = "Log filter, in RUST_LOG syntax"
    )]
    log: String,

    #[arg(
        short = 'e',
        long = "command",
        num_args = 1..,
        allow_hyphen_values = true,
        help = "Run a command in the first tab instead of the default shell"
    )]
    command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    scell_ui::run_installer_hooks();
    let gpu = gpu::avoid_discrete_gpu();
    let cli = Cli::parse();
    // 解压内置字体与下面的初始化并行
    let font = scell_ui::BundledFont::load();
    let logs = scell_config::ConfigPaths::discover().map(|p| p.logs_dir());
    let _log_guard = init_logging(&cli.log, logs.as_deref());
    if let Some(dir) = logs {
        install_panic_hook(dir);
    }
    if !gpu.is_empty() {
        tracing::info!(
            "混合显卡：本进程不加载 NVIDIA 驱动，独显保持休眠；设置 SCELL_DISCRETE_GPU=1 可取消"
        );
        scell_session::hide_env_from_shells(gpu.iter().map(|v| (*v).to_owned()).collect());
    }

    // 网络、计时等异步任务在独立的 tokio 运行时上，与 GPUI 主线程解耦
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("scell-io")
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    let (sessions, events) = SessionService::new(runtime.handle().clone());
    // 未指定命令时由 UI 按设置选择 shell
    let first_shell = cli.command.split_first().map(|(program, args)| LocalShell {
        program: Some(program.into()),
        args: args.to_vec(),
        ..Default::default()
    });

    gpui_kit::application()
        .with_assets(scell_ui::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            scell_ui::init(cx, sessions, events, font);
            if let Err(err) = scell_ui::open_main_window(first_shell, cx) {
                tracing::error!(%err, "无法打开主窗口");
                cx.quit();
            }
        });

    drop(runtime);
    Ok(())
}

/// 日志同时写到标准错误和日志目录；目录不可用时只写标准错误。返回的守卫负责在退出时刷盘
fn init_logging(
    filter: &str,
    dir: Option<&Path>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = || EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());
    let file = dir.and_then(|dir| {
        // 目录不存在时 appender 清理旧日志会往标准错误报错
        std::fs::create_dir_all(dir).ok()?;
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("scell")
            .filename_suffix("log")
            .max_log_files(LOG_FILES)
            .build(dir)
            .ok()
    });
    let (file, guard) = match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .init();
    guard
}

/// panic 时把信息与调用栈同步写进日志目录下的崩溃记录：GPUI 可能随后直接中止进程，异步日志来不及刷盘
fn install_panic_hook(dir: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current();
        let report = format!(
            // 用户提交问题时会附上这份记录，用英文
            "SCell {} crashed\nThread: {}\n{info}\n\n{backtrace}\n",
            env!("CARGO_PKG_VERSION"),
            thread.name().unwrap_or("<unnamed>"),
        );
        let name = chrono::Local::now()
            .format("crash-%Y%m%d-%H%M%S.txt")
            .to_string();
        let path = dir.join(name);
        if std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, report))
            .is_ok()
        {
            tracing::error!(path = %path.display(), "程序崩溃，已写入崩溃记录：{info}");
        }
        default(info);
    }));
}
