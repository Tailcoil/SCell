use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use scell_term::{TermSize, Terminal};

use crate::SessionEvent;
use crate::transport::TransportControl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub(crate) u64);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "s{}", self.0)
    }
}

/// 会话的共享状态，由 pump、计时任务与句柄共同持有。
pub(crate) struct Shared {
    pub(crate) id: SessionId,
    pub(crate) terminal: Arc<Terminal>,
    pub(crate) events: flume::Sender<SessionEvent>,
    /// 发往传输层的 UTF-8 字节；转码在传输的写线程中完成。
    pub(crate) input: flume::Sender<Vec<u8>>,
    wakeup_pending: AtomicBool,
}

impl Shared {
    pub(crate) fn new(
        id: SessionId,
        terminal: Arc<Terminal>,
        events: flume::Sender<SessionEvent>,
        input: flume::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            id,
            terminal,
            events,
            input,
            wakeup_pending: AtomicBool::new(false),
        }
    }

    /// 合并唤醒：UI 确认之前的重复通知全部丢弃，刷屏时不会淹没事件通道。
    pub(crate) fn wakeup(&self) {
        if !self.wakeup_pending.swap(true, Ordering::AcqRel) {
            let _ = self.events.send(SessionEvent::Wakeup(self.id));
        }
    }

    pub(crate) fn emit(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }
}

/// 会话句柄，可廉价克隆。
#[derive(Clone)]
pub struct SessionHandle {
    pub(crate) shared: Arc<Shared>,
    pub(crate) control: Arc<dyn TransportControl>,
}

impl SessionHandle {
    pub fn id(&self) -> SessionId {
        self.shared.id
    }

    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.shared.terminal
    }

    /// 写入已编码的终端输入（按键、粘贴）。
    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.shared.input.send(bytes);
        }
    }

    pub fn resize(&self, size: TermSize) {
        if self.shared.terminal.size() == size {
            return;
        }
        self.shared.terminal.resize(size);
        if let Err(err) = self.control.resize(size) {
            tracing::warn!(session = %self.id(), %err, "调整 PTY 尺寸失败");
        }
    }

    /// 从进程信息读到的工作目录；拿不到时为 `None`，可改用 OSC 7 上报的目录。
    pub fn process_cwd(&self) -> Option<std::path::PathBuf> {
        self.control.cwd()
    }

    /// 已断开的远程会话立即重连，沿用原终端与回滚内容；正在等待自动重连时跳过等待。
    pub fn reconnect(&self) {
        self.control.reconnect();
    }

    /// 开始采集会话所在机器的指标（本地会话为本机）。释放返回值即停止，远端探针随通道关闭退出。
    /// 远程会话须已连上，否则立即收到 [`crate::MetricsUpdate::Failed`]。
    pub fn watch_metrics(&self) -> crate::MetricsWatch {
        let (tx, rx) = flume::unbounded();
        let (stop_tx, stop) = tokio::sync::oneshot::channel();
        self.control.watch_metrics(tx, stop);
        crate::MetricsWatch { rx, _stop: stop_tx }
    }

    /// 会话所在连接上的端口转发（同一主机的会话共用）。
    pub fn forwards(&self) -> Vec<crate::ForwardStatus> {
        self.control.forwards()
    }

    /// 在当前连接上临时添加转发；连接断开后不再恢复。
    pub fn add_forward(
        &self,
        rule: scell_config::hosts::ForwardRule,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let rx = self.control.add_forward(rule);
        async move {
            rx.await
                .unwrap_or_else(|_| Err(rust_i18n::t!("session.closed").into()))
        }
    }

    pub fn remove_forward(&self, id: crate::ForwardId) {
        self.control.remove_forward(id);
    }

    pub fn is_connected(&self) -> bool {
        self.control.is_connected()
    }

    /// 测一次当前连接的往返时间（同一主机的会话共用连接）。本地会话、未连上或超时时为 `None`。
    /// 返回的 Future 与执行器无关，可在 UI 线程上等待。
    pub fn ping(&self) -> impl Future<Output = Option<std::time::Duration>> + Send + 'static {
        let rx = self.control.ping();
        async move { rx.await.ok().flatten() }
    }

    /// 会话所在机器的文件系统：本地会话为本机，SSH 会话在当前连接上打开 SFTP。
    /// 返回的 Future 与执行器无关，可在 UI 线程上等待。
    pub fn open_vfs(
        &self,
    ) -> impl Future<Output = Result<Arc<dyn scell_vfs::Vfs>, String>> + Send + 'static {
        let rx = self.control.open_vfs();
        async move {
            rx.await
                .unwrap_or_else(|_| Err(rust_i18n::t!("session.closed").into()))
        }
    }

    /// UI 读取完最新内容后调用，允许下一次唤醒。须在取快照之前调用，避免漏掉更新。
    pub fn ack_wakeup(&self) {
        self.shared.wakeup_pending.store(false, Ordering::Release);
    }

    /// 在会话中按顺序执行一组自动操作步骤。
    pub fn run_steps(&self, steps: Vec<scell_config::hosts::Step>) {
        if !steps.is_empty() {
            self.control.run_steps(steps);
        }
    }

    pub(crate) fn kill(&self) {
        self.control.kill();
    }
}
