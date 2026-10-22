//! 文件相关的服务：共享的传输引擎，以及让 UI 在 tokio 上执行文件系统操作的入口。

use std::sync::Arc;

use scell_vfs::{LocalFs, TransferEngine, TransferEvent, Vfs};

use crate::SessionService;

/// 同时运行的传输任务数。
pub(crate) const TRANSFER_CONCURRENCY: usize = 3;

impl SessionService {
    /// 全应用共用的传输引擎。
    pub fn transfers(&self) -> &TransferEngine {
        &self.inner.transfers
    }

    /// 传输引擎的事件流，由 UI 消费。
    pub fn transfer_events(&self) -> flume::Receiver<TransferEvent> {
        self.inner.transfer_events.clone()
    }

    /// 本机文件系统。
    pub fn local_vfs(&self) -> Arc<dyn Vfs> {
        Arc::new(LocalFs)
    }

    /// 在 IO 运行时上执行 `fut`，返回与执行器无关的 Future。[`Vfs`] 的方法须经此调用：
    /// 本地实现依赖 tokio 的阻塞线程池。运行时关闭或任务 panic 时得到 `None`。
    pub fn run<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
    ) -> impl Future<Output = Option<T>> + Send + 'static {
        let task = self.inner.rt.spawn(fut);
        async move { task.await.ok() }
    }
}
