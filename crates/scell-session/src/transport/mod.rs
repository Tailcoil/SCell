mod local;
mod ssh;

pub(crate) use local::spawn_local;
pub use local::{LocalShell, hide_env_from_shells};
pub use ssh::SshTarget;
pub(crate) use ssh::{SshEnv, SshMode, spawn_ssh};

use std::sync::Arc;

use scell_term::TermSize;
use scell_vfs::Vfs;
use tokio::sync::oneshot;

/// 打开会话文件系统的结果，失败时为面向用户的原因。
pub(crate) type VfsReply = oneshot::Receiver<Result<Arc<dyn Vfs>, String>>;

/// 传输层的控制面。数据面统一为“输出字节进 pump、输入字节出 channel”。
pub(crate) trait TransportControl: Send + Sync {
    fn resize(&self, size: TermSize) -> std::io::Result<()>;
    fn kill(&self);
    /// 断开后立即重连；不支持重连的传输忽略。
    fn reconnect(&self) {}
    /// 开始采集会话所在机器的指标，直到 `stop` 触发或 `tx` 断开。
    fn watch_metrics(
        &self,
        tx: flume::Sender<crate::telemetry::MetricsUpdate>,
        stop: oneshot::Receiver<()>,
    );

    /// 当前连接上的端口转发。本地会话没有。
    fn forwards(&self) -> Vec<crate::ForwardStatus> {
        Vec::new()
    }

    /// 在当前连接上临时添加一条转发。
    fn add_forward(
        &self,
        _rule: scell_config::hosts::ForwardRule,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(rust_i18n::t!("session.local_forwards").into()));
        rx
    }

    fn remove_forward(&self, _id: crate::ForwardId) {}

    /// 远程会话当前是否连着；本地会话始终为真。
    fn is_connected(&self) -> bool {
        true
    }
    /// 会话所在机器的文件系统。默认为本机。
    fn open_vfs(&self) -> VfsReply {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Ok(Arc::new(scell_vfs::LocalFs) as Arc<dyn Vfs>));
        rx
    }
    /// 无需程序配合即可得知的本机工作目录（目前仅 Linux 本地 shell）。
    fn cwd(&self) -> Option<std::path::PathBuf> {
        None
    }
}
