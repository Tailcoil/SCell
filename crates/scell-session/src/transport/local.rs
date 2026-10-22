use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use scell_term::TermSize;
use tokio::runtime::Handle;

use super::TransportControl;
use crate::codec::{Decoder, Encoder};
use crate::pump::Pump;
use crate::session::Shared;
use crate::{Encoding, SessionError, SessionEvent};

/// 单次读取上限。超过后释放终端锁让渲染有机会取快照。
const READ_CHUNK: usize = 64 * 1024;

/// 子进程退出后等读线程读完剩余输出的上限。后台进程仍持有 PTY 时读端没有 EOF，
/// Windows 的 ConPTY 在关闭前也不会给 EOF，不能无限等。
const EXIT_DRAIN: Duration = Duration::from_millis(500);

/// 只给本进程用、不该传给 shell 的环境变量，见 [`hide_env_from_shells`]。
static HIDDEN_ENV: OnceLock<Vec<String>> = OnceLock::new();

/// 登记本进程为自身设置的环境变量（如显卡驱动选择），之后启动的本地 shell 不继承它们。只有第一次调用生效。
pub fn hide_env_from_shells(names: Vec<String>) {
    let _ = HIDDEN_ENV.set(names);
}

/// 本地 shell 配置档。`program` 为空时使用系统默认 shell：Unix 为登录 shell，Windows 为 PowerShell。
#[derive(Debug, Clone, Default)]
pub struct LocalShell {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub encoding: Encoding,
}

struct LocalControl {
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "只有 Linux 能按 pid 读工作目录")
    )]
    pid: Option<u32>,
    rt: Handle,
}

impl TransportControl for LocalControl {
    fn watch_metrics(
        &self,
        tx: flume::Sender<crate::telemetry::MetricsUpdate>,
        stop: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.rt.spawn(crate::telemetry::run_local(tx, stop));
    }

    fn resize(&self, size: TermSize) -> std::io::Result<()> {
        self.master
            .lock()
            .resize(pty_size(size))
            .map_err(std::io::Error::other)
    }

    fn kill(&self) {
        let _ = self.killer.lock().kill();
    }

    fn cwd(&self) -> Option<PathBuf> {
        // 读 shell 进程本身的目录：前台子进程（如 vim）切换目录不算
        #[cfg(target_os = "linux")]
        return self
            .pid
            .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).ok());
        #[cfg(not(target_os = "linux"))]
        None
    }
}

fn pty_size(size: TermSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(size.cell_width),
        pixel_height: size.rows.saturating_mul(size.cell_height),
    }
}

fn command(shell: &LocalShell) -> CommandBuilder {
    let program = |program: &Path, args: &[String]| {
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd
    };
    let mut cmd = match &shell.program {
        Some(path) => program(path, &shell.args),
        None => match crate::profiles::default_shell() {
            Some(profile) => program(&profile.program, &profile.args),
            None => CommandBuilder::new_default_prog(),
        },
    };
    let cwd = shell.cwd.clone().or_else(std::env::home_dir);
    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }
    for name in HIDDEN_ENV.get().into_iter().flatten() {
        cmd.env_remove(name);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "SCell");
    for (k, v) in &shell.env {
        cmd.env(k, v);
    }
    cmd
}

/// 启动本地 shell，并为读、写、等待退出各开一个线程。返回会话的控制面。
pub(crate) fn spawn_local(
    shell: &LocalShell,
    size: TermSize,
    shared: Arc<Shared>,
    input: flume::Receiver<Vec<u8>>,
    rt: Handle,
) -> Result<Arc<dyn TransportControl>, SessionError> {
    let spawn_err = |e: anyhow::Error| SessionError::Spawn(format!("{e:#}"));
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(spawn_err)?;
    let mut child = pair
        .slave
        .spawn_command(command(shell))
        .map_err(spawn_err)?;
    // 子进程持有 slave 副本；本端不关闭的话，子进程退出后读端收不到 EOF
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(spawn_err)?;
    let mut writer = pair.master.take_writer().map_err(spawn_err)?;
    let control = Arc::new(LocalControl {
        killer: Mutex::new(child.clone_killer()),
        master: Mutex::new(pair.master),
        pid: child.process_id(),
        rt: rt.clone(),
    });
    let id = shared.id;

    let mut pump = Pump::new(shared.clone(), Decoder::new(shell.encoding), rt);
    // 读线程结束时丢弃发送端，等待线程据此确认输出已读完再发 Exited
    let (drained_tx, drained_rx) = mpsc::channel::<()>();
    spawn_thread(format!("pty-read-{id}"), move || {
        let _drained = drained_tx;
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => pump.on_output(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // Linux 上子进程退出后 master 读取返回 EIO，属正常结束
                Err(_) => break,
            }
        }
    })?;

    let encoder = Encoder::new(shell.encoding);
    spawn_thread(format!("pty-write-{id}"), move || {
        while let Ok(bytes) = input.recv() {
            let bytes = encoder.encode(bytes);
            if writer
                .write_all(&bytes)
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    })?;

    let events = shared;
    spawn_thread(format!("pty-wait-{id}"), move || {
        let code = child.wait().ok().map(|s| s.exit_code());
        let _ = drained_rx.recv_timeout(EXIT_DRAIN);
        events.emit(SessionEvent::Exited { id, code });
    })?;

    Ok(control)
}

fn spawn_thread(name: String, f: impl FnOnce() + Send + 'static) -> Result<(), SessionError> {
    thread::Builder::new()
        .name(name)
        .spawn(f)
        .map(drop)
        .map_err(|e| SessionError::Spawn(e.to_string()))
}
