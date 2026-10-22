//! 传输引擎：在任意两个 [`Vfs`] 之间复制文件或目录。
//!
//! 每个 [`TransferRequest`] 是一个任务；目录先遍历一遍得出总量，再逐个文件复制。任务受并发上限约束，
//! 暂停的任务交还名额，继续时从断点续写。状态以 [`TransferEvent::Updated`] 快照上报（运行中节流），
//! 需要用户决定的冲突以 [`TransferEvent::Conflict`] 上抛，带一次性回复。

mod job;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, oneshot, watch};

use crate::{Metadata, Vfs, VfsPath};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

/// 目标已存在时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// 逐个询问。
    Ask,
    Overwrite,
    Skip,
    /// 另起名字，如 `a (1).txt`。
    Rename,
    /// 目标比源小时从目标末尾续传，一样大时跳过，更大时覆盖。
    Resume,
}

/// 对一次冲突的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    Overwrite,
    Skip,
    Rename,
    Resume,
    /// 取消整个任务。
    Cancel,
}

#[derive(Clone)]
pub struct TransferRequest {
    pub source: Arc<dyn Vfs>,
    pub source_path: VfsPath,
    pub dest: Arc<dyn Vfs>,
    /// 目标的完整路径（含名称），不是目标所在目录。
    pub dest_path: VfsPath,
    pub conflict: ConflictPolicy,
    /// 保留权限与修改时间。
    pub preserve: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferState {
    /// 等待并发名额。
    Queued,
    Running,
    Paused,
    Done,
    /// 单个文件的任务因冲突被跳过。
    Skipped,
    Cancelled,
    Failed(String),
}

impl TransferState {
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Skipped | Self::Cancelled | Self::Failed(_)
        )
    }
}

/// 任务的状态快照。
#[derive(Debug, Clone, PartialEq)]
pub struct TransferInfo {
    pub id: TransferId,
    pub source: VfsPath,
    pub dest: VfsPath,
    pub state: TransferState,
    /// 目录任务在遍历完成前为 0。
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub files_total: u64,
    pub files_done: u64,
    /// 最近一段时间的平均速率，字节每秒。
    pub rate: f64,
}

impl TransferInfo {
    /// 按当前速率估计的剩余时间。
    pub fn eta(&self) -> Option<Duration> {
        let left = self.bytes_total.checked_sub(self.bytes_done)?;
        (self.rate > 1.0).then(|| Duration::from_secs_f64(left as f64 / self.rate))
    }
}

/// 目标已存在、需要用户决定的冲突。可克隆以便事件分发，只有第一次回答生效；
/// 不回答就丢弃等同于取消任务。
#[derive(Clone)]
pub struct Conflict {
    pub transfer: TransferId,
    pub path: VfsPath,
    pub existing: Metadata,
    pub incoming: Metadata,
    reply: ConflictReply,
}

/// 冲突的回答与是否“全部应用”。
pub(crate) type ConflictReply = Arc<Mutex<Option<oneshot::Sender<(ConflictAction, bool)>>>>;

impl Conflict {
    /// `apply_to_all` 为真时，同一任务中之后的冲突按同样方式处理，不再询问。
    pub fn answer(&self, action: ConflictAction, apply_to_all: bool) {
        if let Some(tx) = self.reply.lock().take() {
            let _ = tx.send((action, apply_to_all));
        }
    }
}

impl std::fmt::Debug for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conflict")
            .field("transfer", &self.transfer)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransferEvent {
    Updated(TransferInfo),
    Conflict(Conflict),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Pause,
    Cancel,
}

struct Entry {
    request: TransferRequest,
    control: watch::Sender<Control>,
    info: Arc<Mutex<TransferInfo>>,
}

struct Shared {
    rt: Handle,
    permits: Arc<Semaphore>,
    events: flume::Sender<TransferEvent>,
    jobs: Mutex<HashMap<TransferId, Entry>>,
    next_id: AtomicU64,
}

/// 传输引擎句柄，可廉价克隆。
#[derive(Clone)]
pub struct TransferEngine {
    shared: Arc<Shared>,
}

impl TransferEngine {
    /// `concurrency` 为同时运行的任务数上限。
    pub fn new(rt: Handle, concurrency: usize) -> (Self, flume::Receiver<TransferEvent>) {
        let (events, rx) = flume::unbounded();
        let shared = Shared {
            rt,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            events,
            jobs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        };
        (
            Self {
                shared: Arc::new(shared),
            },
            rx,
        )
    }

    pub fn enqueue(&self, request: TransferRequest) -> TransferId {
        let id = TransferId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let info = Arc::new(Mutex::new(TransferInfo {
            id,
            source: request.source_path.clone(),
            dest: request.dest_path.clone(),
            state: TransferState::Queued,
            bytes_total: 0,
            bytes_done: 0,
            files_total: 0,
            files_done: 0,
            rate: 0.,
        }));
        let (control, control_rx) = watch::channel(Control::Run);
        let job = job::Job::new(
            id,
            request.clone(),
            info.clone(),
            control_rx,
            self.shared.clone(),
        );
        let snapshot = info.lock().clone();
        self.shared.jobs.lock().insert(
            id,
            Entry {
                request,
                control,
                info,
            },
        );
        let _ = self.shared.events.send(TransferEvent::Updated(snapshot));
        self.shared.rt.spawn(job.run());
        id
    }

    fn control(&self, id: TransferId, value: Control) {
        if let Some(entry) = self.shared.jobs.lock().get(&id) {
            entry.control.send_if_modified(|c| {
                // 已取消的不能再暂停或继续
                let changed = *c != value && *c != Control::Cancel;
                if changed {
                    *c = value;
                }
                changed
            });
        }
    }

    pub fn pause(&self, id: TransferId) {
        self.control(id, Control::Pause);
    }

    pub fn resume(&self, id: TransferId) {
        self.control(id, Control::Run);
    }

    pub fn cancel(&self, id: TransferId) {
        self.control(id, Control::Cancel);
    }

    /// 重新开始失败或取消的任务，已传部分续传。返回新任务的 id，原任务从列表移除。
    pub fn retry(&self, id: TransferId) -> Option<TransferId> {
        let request = {
            let mut jobs = self.shared.jobs.lock();
            let finished = jobs.get(&id).is_some_and(|e| {
                matches!(
                    e.info.lock().state,
                    TransferState::Failed(_) | TransferState::Cancelled
                )
            });
            if !finished {
                return None;
            }
            jobs.remove(&id)?.request
        };
        Some(self.enqueue(TransferRequest {
            conflict: ConflictPolicy::Resume,
            ..request
        }))
    }

    /// 从列表中移除已结束的任务。
    pub fn remove(&self, id: TransferId) -> bool {
        let mut jobs = self.shared.jobs.lock();
        let finished = jobs
            .get(&id)
            .is_some_and(|e| e.info.lock().state.is_finished());
        finished && jobs.remove(&id).is_some()
    }

    /// 全部任务的快照，按加入顺序。
    pub fn list(&self) -> Vec<TransferInfo> {
        let mut out: Vec<TransferInfo> = self
            .shared
            .jobs
            .lock()
            .values()
            .map(|e| e.info.lock().clone())
            .collect();
        out.sort_by_key(|i| i.id);
        out
    }
}
