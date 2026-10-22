//! 单个传输任务的执行。

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, oneshot, watch};

use super::{
    Conflict, ConflictAction, ConflictPolicy, Control, Shared, TransferEvent, TransferId,
    TransferInfo, TransferRequest, TransferState,
};
use crate::{FileType, Metadata, VfsError, VfsPath, WriteMode};

/// 运行中的进度事件间隔。
const EMIT_INTERVAL: Duration = Duration::from_millis(200);
/// 网络类错误的重试次数。
const RETRIES: u32 = 3;

enum Stop {
    Cancelled,
    Failed(VfsError),
}

impl From<VfsError> for Stop {
    fn from(e: VfsError) -> Self {
        Self::Failed(e)
    }
}

/// 遍历出的一项，`rel` 为相对任务根的各级名称。
enum Item {
    Dir(Vec<Vec<u8>>),
    File(Vec<Vec<u8>>, Metadata),
}

/// 一次复制的结果。
enum Copied {
    Done,
    /// 收到暂停或取消，已在该偏移处收尾。
    Interrupted(u64),
}

pub(super) struct Job {
    id: TransferId,
    req: TransferRequest,
    info: Arc<Mutex<TransferInfo>>,
    control: watch::Receiver<Control>,
    shared: Arc<Shared>,
    /// 用户选了“全部应用”后会替换为具体策略。
    policy: ConflictPolicy,
    permit: Option<OwnedSemaphorePermit>,
    last_emit: Instant,
    last_bytes: u64,
}

fn join(base: &VfsPath, rel: &[Vec<u8>]) -> VfsPath {
    rel.iter().fold(base.clone(), |p, name| p.join(name))
}

impl Job {
    pub(super) fn new(
        id: TransferId,
        req: TransferRequest,
        info: Arc<Mutex<TransferInfo>>,
        control: watch::Receiver<Control>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            id,
            policy: req.conflict,
            req,
            info,
            control,
            shared,
            permit: None,
            last_emit: Instant::now(),
            last_bytes: 0,
        }
    }

    pub(super) async fn run(mut self) {
        let state = match self.execute().await {
            Ok(state) => state,
            Err(Stop::Cancelled) => TransferState::Cancelled,
            Err(Stop::Failed(e)) => TransferState::Failed(e.to_string()),
        };
        self.permit = None;
        if let TransferState::Failed(e) = &state {
            tracing::warn!(id = self.id.0, source = ?self.req.source_path, "传输失败：{e}");
        }
        self.info.lock().rate = 0.;
        self.set_state(state);
    }

    async fn execute(&mut self) -> Result<TransferState, Stop> {
        self.acquire().await?;
        self.set_state(TransferState::Running);
        let root = self.req.source.stat(&self.req.source_path).await?;
        let single = !root.is_dir();
        let items = if !single {
            self.walk().await?
        } else {
            vec![Item::File(Vec::new(), root)]
        };
        {
            let mut info = self.info.lock();
            for item in &items {
                if let Item::File(_, m) = item {
                    info.files_total += 1;
                    info.bytes_total += m.size.unwrap_or(0);
                }
            }
        }
        self.emit(true);

        let mut skipped = 0;
        let mut dirs = Vec::new();
        for item in items {
            self.checkpoint().await?;
            match item {
                Item::Dir(rel) => {
                    self.ensure_dir(&join(&self.req.dest_path, &rel)).await?;
                    dirs.push(rel);
                }
                Item::File(rel, meta) => {
                    let src = join(&self.req.source_path, &rel);
                    let dst = join(&self.req.dest_path, &rel);
                    if !self.file(&src, dst, &meta).await? {
                        skipped += 1;
                    }
                    let mut info = self.info.lock();
                    info.files_done += 1;
                }
            }
        }
        // 目录时间在其内容写完后才设置，否则写文件时又被改掉；从深到浅
        if self.req.preserve {
            for rel in dirs.iter().rev() {
                let src = join(&self.req.source_path, rel);
                if let Ok(meta) = self.req.source.stat(&src).await {
                    self.preserve(&join(&self.req.dest_path, rel), &meta).await;
                }
            }
        }
        Ok(if single && skipped == 1 {
            TransferState::Skipped
        } else {
            TransferState::Done
        })
    }

    /// 深度优先列出全部目录与文件，目录在其内容之前。符号链接与特殊文件跳过。
    async fn walk(&mut self) -> Result<Vec<Item>, Stop> {
        let mut items = vec![Item::Dir(Vec::new())];
        let mut stack = vec![Vec::new()];
        while let Some(rel) = stack.pop() {
            self.checkpoint().await?;
            let dir = join(&self.req.source_path, &rel);
            let mut entries = self.req.source.list(&dir).await?;
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            for entry in entries {
                let mut child = rel.clone();
                child.push(entry.name);
                match entry.metadata.file_type() {
                    FileType::Dir => {
                        items.push(Item::Dir(child.clone()));
                        stack.push(child);
                    }
                    FileType::File => items.push(Item::File(child, entry.metadata)),
                    _ => tracing::debug!(path = ?entry.path, "跳过符号链接或特殊文件"),
                }
            }
        }
        // 栈序会打乱目录先后，按路径排序后父目录一定在子项之前
        items.sort_by(|a, b| {
            let key = |i: &Item| match i {
                Item::Dir(r) | Item::File(r, _) => r.clone(),
            };
            key(a).cmp(&key(b))
        });
        Ok(items)
    }

    async fn ensure_dir(&self, path: &VfsPath) -> Result<(), Stop> {
        match self.req.dest.stat(path).await {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(VfsError::AlreadyExists.into()),
            Err(VfsError::NotFound) => Ok(self.req.dest.mkdir(path).await?),
            Err(e) => Err(e.into()),
        }
    }

    /// 传一个文件。返回 `false` 表示按冲突策略跳过。
    async fn file(&mut self, src: &VfsPath, dst: VfsPath, meta: &Metadata) -> Result<bool, Stop> {
        let size = meta.size.unwrap_or(0);
        let Some((dst, mode)) = self.resolve(dst, meta).await? else {
            self.add_bytes(size);
            return Ok(false);
        };
        let fresh = !matches!(mode, WriteMode::Resume(_));
        match self.copy(src, &dst, mode).await {
            Ok(()) => {}
            // 取消时删除从头写的半截文件；续传的保留，以便下次接着传
            Err(Stop::Cancelled) if fresh => {
                let _ = self.req.dest.remove_file(&dst).await;
                return Err(Stop::Cancelled);
            }
            Err(e) => return Err(e),
        }
        if self.req.preserve {
            self.preserve(&dst, meta).await;
        }
        Ok(true)
    }

    /// 处理目标已存在的情况，返回最终的目标路径与写入方式；`None` 表示跳过。
    async fn resolve(
        &mut self,
        dst: VfsPath,
        incoming: &Metadata,
    ) -> Result<Option<(VfsPath, WriteMode)>, Stop> {
        let existing = match self.req.dest.stat(&dst).await {
            Ok(m) => m,
            Err(VfsError::NotFound) => return Ok(Some((dst, WriteMode::Truncate))),
            Err(e) => return Err(e.into()),
        };
        if existing.is_dir() {
            return Err(VfsError::AlreadyExists.into());
        }
        let action = match self.policy {
            ConflictPolicy::Overwrite => ConflictAction::Overwrite,
            ConflictPolicy::Skip => ConflictAction::Skip,
            ConflictPolicy::Rename => ConflictAction::Rename,
            ConflictPolicy::Resume => ConflictAction::Resume,
            ConflictPolicy::Ask => self.ask(&dst, &existing, incoming).await?,
        };
        Ok(match action {
            ConflictAction::Overwrite => Some((dst, WriteMode::Truncate)),
            ConflictAction::Skip => None,
            ConflictAction::Cancel => return Err(Stop::Cancelled),
            ConflictAction::Rename => Some((self.free_name(&dst).await?, WriteMode::Truncate)),
            ConflictAction::Resume => {
                let have = existing.size.unwrap_or(0);
                let want = incoming.size.unwrap_or(0);
                match have.cmp(&want) {
                    std::cmp::Ordering::Less => Some((dst, WriteMode::Resume(have))),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some((dst, WriteMode::Truncate)),
                }
            }
        })
    }

    async fn ask(
        &mut self,
        dst: &VfsPath,
        existing: &Metadata,
        incoming: &Metadata,
    ) -> Result<ConflictAction, Stop> {
        let (tx, rx) = oneshot::channel();
        let conflict = Conflict {
            transfer: self.id,
            path: dst.clone(),
            existing: existing.clone(),
            incoming: incoming.clone(),
            reply: Arc::new(Mutex::new(Some(tx))),
        };
        let _ = self.shared.events.send(TransferEvent::Conflict(conflict));
        // 等待回答时不占并发名额，也能被取消
        self.permit = None;
        let answer = tokio::select! {
            answer = rx => answer.unwrap_or((ConflictAction::Cancel, false)),
            () = cancelled(self.control.clone()) => (ConflictAction::Cancel, false),
        };
        self.acquire().await?;
        let (action, all) = answer;
        if all {
            self.policy = match action {
                ConflictAction::Overwrite => ConflictPolicy::Overwrite,
                ConflictAction::Skip => ConflictPolicy::Skip,
                ConflictAction::Rename => ConflictPolicy::Rename,
                ConflictAction::Resume => ConflictPolicy::Resume,
                ConflictAction::Cancel => self.policy,
            };
        }
        Ok(action)
    }

    /// `a.txt` 已存在时依次尝试 `a (1).txt`、`a (2).txt`…
    async fn free_name(&self, dst: &VfsPath) -> Result<VfsPath, Stop> {
        let parent = dst.parent().unwrap_or_default();
        let name = dst.file_name().unwrap_or_default();
        // 隐藏文件开头的点不算扩展名
        let split = name
            .iter()
            .rposition(|&c| c == b'.')
            .filter(|&i| i > 0)
            .unwrap_or(name.len());
        let (stem, ext) = name.split_at(split);
        for n in 1..1000 {
            let candidate = parent.join(&[stem, format!(" ({n})").as_bytes(), ext].concat());
            match self.req.dest.lstat(&candidate).await {
                Err(VfsError::NotFound) => return Ok(candidate),
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
        }
        Err(VfsError::AlreadyExists.into())
    }

    /// 复制一个文件，处理暂停与网络类错误的续传重试。
    async fn copy(
        &mut self,
        src: &VfsPath,
        dst: &VfsPath,
        mut mode: WriteMode,
    ) -> Result<(), Stop> {
        // 本文件已计入进度的字节数
        let mut counted = match mode {
            WriteMode::Resume(offset) => offset,
            _ => 0,
        };
        self.add_bytes(counted);
        let mut failures = 0;
        loop {
            match self.copy_once(src, dst, mode, &mut counted).await {
                Ok(Copied::Done) => return Ok(()),
                Ok(Copied::Interrupted(at)) => {
                    self.checkpoint().await?;
                    mode = WriteMode::Resume(at);
                }
                Err(Stop::Failed(e)) if transient(&e) && failures < RETRIES => {
                    failures += 1;
                    tracing::info!(%e, attempt = failures, "传输出错，稍后续传重试");
                    tokio::time::sleep(Duration::from_secs(1 << failures)).await;
                    // 以目标实际写入的长度为准续传，进度回退到那里
                    let have = self
                        .req
                        .dest
                        .stat(dst)
                        .await?
                        .size
                        .unwrap_or(0)
                        .min(counted);
                    self.sub_bytes(counted - have);
                    counted = have;
                    mode = WriteMode::Resume(have);
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn copy_once(
        &mut self,
        src: &VfsPath,
        dst: &VfsPath,
        mode: WriteMode,
        counted: &mut u64,
    ) -> Result<Copied, Stop> {
        let offset = match mode {
            WriteMode::Resume(offset) => offset,
            _ => 0,
        };
        let mut reader = self.req.source.open_read(src, offset).await?;
        let mut writer = self.req.dest.open_write(dst, mode).await?;
        let mut pos = offset;
        loop {
            if *self.control.borrow() != Control::Run {
                writer.finish().await?;
                return Ok(Copied::Interrupted(pos));
            }
            let Some(chunk) = reader.read().await? else {
                break;
            };
            let len = chunk.len() as u64;
            writer.write(chunk).await?;
            pos += len;
            *counted += len;
            self.add_bytes(len);
        }
        writer.finish().await?;
        Ok(Copied::Done)
    }

    /// 保留权限与时间；失败只记日志（如目标文件系统不支持）。
    async fn preserve(&self, dst: &VfsPath, meta: &Metadata) {
        let attrs = Metadata {
            permissions: meta.mode(),
            accessed: meta.accessed,
            modified: meta.modified,
            ..Default::default()
        };
        if let Err(e) = self.req.dest.set_metadata(dst, &attrs).await {
            tracing::debug!(path = ?dst, %e, "无法保留属性");
        }
    }

    /// 取得并发名额；暂停期间不取，取消时返回。
    async fn acquire(&mut self) -> Result<(), Stop> {
        if self.permit.is_some() {
            return Ok(());
        }
        loop {
            let control = *self.control.borrow_and_update();
            match control {
                Control::Cancel => return Err(Stop::Cancelled),
                Control::Pause => self.set_state(TransferState::Paused),
                Control::Run => {
                    self.set_state(TransferState::Queued);
                    let permits = self.shared.permits.clone();
                    tokio::select! {
                        permit = permits.acquire_owned() => {
                            self.permit = permit.ok();
                            self.set_state(TransferState::Running);
                            return Ok(());
                        }
                        changed = self.control.changed() => {
                            if changed.is_err() {
                                return Err(Stop::Cancelled);
                            }
                            continue;
                        }
                    }
                }
            }
            if self.control.changed().await.is_err() {
                return Err(Stop::Cancelled);
            }
        }
    }

    /// 两步之间检查控制：取消则停止，暂停则交还名额并等待继续。
    async fn checkpoint(&mut self) -> Result<(), Stop> {
        let control = *self.control.borrow();
        match control {
            Control::Run => Ok(()),
            Control::Cancel => Err(Stop::Cancelled),
            Control::Pause => {
                self.permit = None;
                self.info.lock().rate = 0.;
                self.acquire().await
            }
        }
    }

    fn add_bytes(&mut self, n: u64) {
        self.info.lock().bytes_done += n;
        self.emit(false);
    }

    fn sub_bytes(&mut self, n: u64) {
        let mut info = self.info.lock();
        info.bytes_done = info.bytes_done.saturating_sub(n);
    }

    fn set_state(&mut self, state: TransferState) {
        let changed = {
            let mut info = self.info.lock();
            let changed = info.state != state;
            info.state = state;
            changed
        };
        if changed {
            self.emit(true);
        }
    }

    /// 上报快照；非强制时节流，并顺带更新速率。
    fn emit(&mut self, force: bool) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_emit);
        if !force && elapsed < EMIT_INTERVAL {
            return;
        }
        let snapshot = {
            let mut info = self.info.lock();
            if info.state == TransferState::Running && elapsed >= EMIT_INTERVAL {
                let sample =
                    info.bytes_done.saturating_sub(self.last_bytes) as f64 / elapsed.as_secs_f64();
                // 指数平滑，避免数字跳动
                info.rate = if info.rate == 0. {
                    sample
                } else {
                    info.rate * 0.7 + sample * 0.3
                };
            }
            self.last_bytes = info.bytes_done;
            info.clone()
        };
        self.last_emit = now;
        let _ = self.shared.events.send(TransferEvent::Updated(snapshot));
    }
}

/// 网络或 IO 层面的偶发错误，续传重试可能成功。连接断开不在此列：同一个 Vfs 已不可用。
fn transient(e: &VfsError) -> bool {
    matches!(e, VfsError::Io(_))
}

/// 等到任务被取消。
async fn cancelled(mut control: watch::Receiver<Control>) {
    loop {
        if *control.borrow_and_update() == Control::Cancel {
            return;
        }
        if control.changed().await.is_err() {
            return;
        }
    }
}
