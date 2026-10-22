use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use parking_lot::Mutex;
use scell_term::{Palette, TermSize, Terminal};
use tokio::runtime::Handle;

use crate::pool::ConnectionPool;
use crate::session::Shared;
use crate::transport::{SshEnv, SshMode, TransportControl, spawn_local, spawn_ssh};
use crate::{LocalShell, SessionEvent, SessionHandle, SessionId, SshTarget};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    #[error("{}", rust_i18n::t!("session.spawn_failed", err = .0))]
    Spawn(String),
}

/// 应用级会话服务。所有会话归它所有，UI 只持有句柄。
#[derive(Clone)]
pub struct SessionService {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) rt: Handle,
    events: flume::Sender<SessionEvent>,
    next_id: AtomicU64,
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
    ssh: SshEnv,
    /// 新会话的回滚行数，已有会话不受影响。
    scrollback: AtomicUsize,
    pub(crate) transfers: scell_vfs::TransferEngine,
    pub(crate) transfer_events: flume::Receiver<scell_vfs::TransferEvent>,
}

impl SessionService {
    /// `rt` 为承载网络与计时任务的 tokio 运行时；返回的接收端由 UI 消费。
    pub fn new(rt: Handle) -> (Self, flume::Receiver<SessionEvent>) {
        let (tx, rx) = flume::unbounded();
        let (transfers, transfer_events) =
            scell_vfs::TransferEngine::new(rt.clone(), crate::files::TRANSFER_CONCURRENCY);
        let inner = Inner {
            rt: rt.clone(),
            events: tx,
            next_id: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            ssh: SshEnv {
                pool: ConnectionPool::default(),
                rt: rt.clone(),
                auto_reconnect: Arc::new(AtomicBool::new(true)),
            },
            scrollback: AtomicUsize::new(10_000),
            transfers,
            transfer_events,
        };
        (
            Self {
                inner: Arc::new(inner),
            },
            rx,
        )
    }

    pub fn set_scrollback(&self, lines: usize) {
        self.inner.scrollback.store(lines, Ordering::Relaxed);
    }

    /// 远程连接意外断开后是否按退避间隔自动重连，对已有会话同样生效。
    pub fn set_auto_reconnect(&self, on: bool) {
        self.inner.ssh.auto_reconnect.store(on, Ordering::Relaxed);
    }

    pub fn spawn_local(
        &self,
        shell: LocalShell,
        size: TermSize,
    ) -> Result<SessionHandle, SessionError> {
        let (shared, input) = self.new_shared(size);
        let control = spawn_local(&shell, size, shared.clone(), input, self.inner.rt.clone())?;
        Ok(self.register(shared, control, "本地"))
    }

    /// 启动 SSH 会话。立即返回，连接在后台进行，同一主机已有连接时直接在其上开新通道；需要用户确认或输入时发出 [`SessionEvent::Prompt`]。
    /// 失败或断开时把原因写进终端并发出 [`SessionEvent::Disconnected`]，远端 shell 退出时发出 [`SessionEvent::Exited`]；
    /// 两种情况下会话都保留，可经 [`SessionHandle::reconnect`] 在原终端上重连。
    pub fn spawn_ssh(&self, target: SshTarget, size: TermSize) -> SessionHandle {
        self.ssh(target, SshMode::Shell, size, false)
    }

    /// 同 [`Self::spawn_ssh`]，但先不连接，直到调用 [`SessionHandle::reconnect`]。
    pub fn spawn_ssh_idle(&self, target: SshTarget, size: TermSize) -> SessionHandle {
        self.ssh(target, SshMode::Shell, size, true)
    }

    /// 启动只用于文件的 SSH 会话：连接、认证、提示与重连同 [`Self::spawn_ssh`]，但不开 shell。
    /// 发出 [`SessionEvent::Connected`] 后可经 [`SessionHandle::open_vfs`] 打开 SFTP。
    /// `idle` 时先不连接，直到调用 [`SessionHandle::reconnect`]。
    pub fn spawn_files(&self, target: SshTarget, idle: bool) -> SessionHandle {
        self.ssh(target, SshMode::Files, TermSize::default(), idle)
    }

    fn ssh(&self, target: SshTarget, mode: SshMode, size: TermSize, idle: bool) -> SessionHandle {
        let (shared, input) = self.new_shared(size);
        let control = spawn_ssh(
            target,
            mode,
            idle,
            size,
            shared.clone(),
            input,
            self.inner.ssh.clone(),
        );
        let kind = match mode {
            SshMode::Shell => "SSH",
            SshMode::Files => "文件",
        };
        self.register(shared, control, kind)
    }

    fn new_shared(&self, size: TermSize) -> (Arc<Shared>, flume::Receiver<Vec<u8>>) {
        let id = SessionId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let terminal = Arc::new(Terminal::new(
            size,
            Palette::default(),
            self.inner.scrollback.load(Ordering::Relaxed),
        ));
        let (input_tx, input_rx) = flume::unbounded();
        let shared = Arc::new(Shared::new(
            id,
            terminal,
            self.inner.events.clone(),
            input_tx,
        ));
        (shared, input_rx)
    }

    fn register(
        &self,
        shared: Arc<Shared>,
        control: Arc<dyn TransportControl>,
        kind: &str,
    ) -> SessionHandle {
        let id = shared.id;
        let handle = SessionHandle { shared, control };
        self.inner.sessions.lock().insert(id, handle.clone());
        tracing::info!(session = %id, kind, "会话已启动");
        handle
    }

    pub fn get(&self, id: SessionId) -> Option<SessionHandle> {
        self.inner.sessions.lock().get(&id).cloned()
    }

    /// 结束进程并释放会话。
    pub fn close(&self, id: SessionId) {
        if let Some(handle) = self.inner.sessions.lock().remove(&id) {
            handle.kill();
        }
    }
}
