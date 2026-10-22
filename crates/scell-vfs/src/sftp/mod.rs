//! 自研 SFTP v3 客户端。运行在任意双向字节流上，通常是 SSH 的 `sftp` 子系统通道。
//! 不用 russh-sftp：它把路径一律按 UTF-8 有损解码，非 UTF-8 文件名的原始字节会丢失；这里路径全程保持字节。
//!
//! 请求按 id 多路复用：写任务串行发出报文，读任务按 id 把回复分发给等待方，所以多个操作与
//! 同一文件的多个读写请求可以同时在途。

mod file;
mod proto;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use self::file::{SftpReader, SftpWriter};
use self::proto::*;
use crate::{Entry, FileReader, FileWriter, Metadata, Result, Vfs, VfsError, VfsPath, WriteMode};

/// 一条回复：类型与 id 之后的内容。
pub(crate) struct Reply {
    kind: u8,
    body: Bytes,
}

/// 等待中的请求。`None` 表示连接已断开，之后不再接受新请求。
type Pending = Arc<parking_lot::Mutex<Option<HashMap<u32, oneshot::Sender<Reply>>>>>;

/// SFTP 会话，可廉价克隆，所有克隆共用一条通道；最后一个克隆释放时通道关闭。
#[derive(Clone)]
pub struct SftpClient {
    inner: Arc<Inner>,
}

struct Inner {
    outgoing: mpsc::UnboundedSender<Bytes>,
    pending: Pending,
    next_id: AtomicU32,
    extensions: Vec<(String, Vec<u8>)>,
    /// 需要与会话同生共死的对象，如承载通道的 SSH 连接。
    _owner: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl SftpClient {
    /// 完成版本协商并在当前 tokio 运行时上启动收发任务。
    pub async fn new<S>(stream: S) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self::start(stream, None).await
    }

    /// 同 [`Self::new`]，并让会话持有 `owner`（如 SSH 连接），保证通道所在的连接不早于会话关闭。
    pub async fn with_owner<S>(stream: S, owner: impl std::any::Any + Send + Sync) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self::start(stream, Some(Box::new(owner))).await
    }

    async fn start<S>(
        stream: S,
        owner: Option<Box<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (mut rd, mut wr) = tokio::io::split(stream);
        let mut init = Encoder::new(FXP_INIT);
        init.u32(VERSION);
        wr.write_all(&init.finish()).await?;
        wr.flush().await?;
        let (kind, body) = read_packet(&mut rd).await?;
        if kind != FXP_VERSION {
            return Err(VfsError::Protocol(format!(
                "expected VERSION, got packet type {kind}"
            )));
        }
        let mut d = Decoder::new(&body);
        let version = d.u32()?;
        if version < VERSION {
            return Err(VfsError::Protocol(format!(
                "server only supports SFTP v{version}"
            )));
        }
        let mut extensions = Vec::new();
        while !d.is_empty() {
            let name = String::from_utf8_lossy(d.string()?).into_owned();
            extensions.push((name, d.string()?.to_vec()));
        }
        tracing::debug!(version, ?extensions, "SFTP 会话已建立");

        let (outgoing, mut rx) = mpsc::unbounded_channel::<Bytes>();
        tokio::spawn(async move {
            while let Some(packet) = rx.recv().await {
                if wr.write_all(&packet).await.is_err() {
                    return;
                }
                // 攒一批再刷新，流水线读写时减少系统调用
                if rx.is_empty() && wr.flush().await.is_err() {
                    return;
                }
            }
            // 客户端全部释放：关闭写方向，服务器随之退出，读任务读到 EOF 后结束
            let _ = wr.shutdown().await;
        });
        let pending: Pending = Arc::new(parking_lot::Mutex::new(Some(HashMap::new())));
        let dispatch = pending.clone();
        tokio::spawn(async move {
            loop {
                let (kind, body) = match read_packet(&mut rd).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::debug!(%e, "SFTP 通道已关闭");
                        break;
                    }
                };
                if body.len() < 4 {
                    break;
                }
                let id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let waiter = dispatch.lock().as_mut().and_then(|m| m.remove(&id));
                // 没有等待方的回复（如后台关闭句柄）直接丢弃
                if let Some(tx) = waiter {
                    let _ = tx.send(Reply {
                        kind,
                        body: body.slice(4..),
                    });
                }
            }
            // 丢弃全部等待方，它们收到 ConnectionLost
            dispatch.lock().take();
        });
        Ok(Self {
            inner: Arc::new(Inner {
                outgoing,
                pending,
                next_id: AtomicU32::new(1),
                extensions,
                _owner: owner,
            }),
        })
    }

    /// 服务器宣告的扩展（如 `posix-rename@openssh.com`）。
    pub fn has_extension(&self, name: &str) -> bool {
        self.inner.extensions.iter().any(|(n, _)| n == name)
    }

    /// 发出请求，不等待回复。
    pub(crate) fn send(
        &self,
        kind: u8,
        build: impl FnOnce(&mut Encoder),
    ) -> Result<oneshot::Receiver<Reply>> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut e = Encoder::request(kind, id);
        build(&mut e);
        let (tx, rx) = oneshot::channel();
        // 在锁内检查连接状态并登记，避免读任务清空后再登记而永远收不到回复
        self.inner
            .pending
            .lock()
            .as_mut()
            .ok_or(VfsError::ConnectionLost)?
            .insert(id, tx);
        self.inner
            .outgoing
            .send(e.finish())
            .map_err(|_| VfsError::ConnectionLost)?;
        Ok(rx)
    }

    /// 发出请求，丢弃回复。用于析构时关闭句柄。
    pub(crate) fn send_detached(&self, kind: u8, build: impl FnOnce(&mut Encoder)) {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut e = Encoder::request(kind, id);
        build(&mut e);
        let _ = self.inner.outgoing.send(e.finish());
    }

    async fn request(&self, kind: u8, build: impl FnOnce(&mut Encoder)) -> Result<Reply> {
        let rx = self.send(kind, build)?;
        rx.await.map_err(|_| VfsError::ConnectionLost)
    }

    async fn path_request(&self, kind: u8, path: &VfsPath) -> Result<Reply> {
        self.request(kind, |e| {
            e.string(path.as_bytes());
        })
        .await
    }

    async fn open(&self, path: &VfsPath, flags: u32) -> Result<Bytes> {
        let reply = self
            .request(FXP_OPEN, |e| {
                e.string(path.as_bytes())
                    .u32(flags)
                    .attrs(&Metadata::default());
            })
            .await?;
        handle(reply)
    }

    pub(crate) fn close_detached(&self, handle: &[u8]) {
        self.send_detached(FXP_CLOSE, |e| {
            e.string(handle);
        });
    }

    async fn close(&self, handle: &[u8]) -> Result<()> {
        status(
            self.request(FXP_CLOSE, |e| {
                e.string(handle);
            })
            .await?,
        )
    }
}

/// 读一个报文，返回类型与其后的内容。
async fn read_packet<R: AsyncRead + Unpin>(rd: &mut R) -> Result<(u8, Bytes)> {
    let len = rd.read_u32().await.map_err(|_| VfsError::ConnectionLost)? as usize;
    if len == 0 || len > MAX_PACKET {
        return Err(VfsError::Protocol(format!("bad packet length {len}")));
    }
    let mut buf = vec![0; len];
    rd.read_exact(&mut buf)
        .await
        .map_err(|_| VfsError::ConnectionLost)?;
    let body = Bytes::from(buf);
    Ok((body[0], body.slice(1..)))
}

fn unexpected(kind: u8) -> VfsError {
    VfsError::Protocol(format!("unexpected reply type {kind}"))
}

/// 解析 STATUS；`FX_EOF` 返回 `Ok(false)`。
fn status_or_eof(reply: &Reply) -> Result<bool> {
    let mut d = Decoder::new(&reply.body);
    match d.u32()? {
        FX_OK => Ok(true),
        FX_EOF => Ok(false),
        code => Err(status_error(code, d.string().unwrap_or_default())),
    }
}

fn status(reply: Reply) -> Result<()> {
    if reply.kind != FXP_STATUS {
        return Err(unexpected(reply.kind));
    }
    match status_or_eof(&reply)? {
        true => Ok(()),
        false => Err(VfsError::Failure(
            rust_i18n::t!("error.unexpected_eof").into_owned(),
        )),
    }
}

fn handle(reply: Reply) -> Result<Bytes> {
    match reply.kind {
        FXP_HANDLE => {
            let mut d = Decoder::new(&reply.body);
            Ok(Bytes::copy_from_slice(d.string()?))
        }
        FXP_STATUS => Err(status(reply)
            .err()
            .unwrap_or_else(|| unexpected(FXP_STATUS))),
        kind => Err(unexpected(kind)),
    }
}

fn attrs(reply: Reply) -> Result<Metadata> {
    match reply.kind {
        FXP_ATTRS => Decoder::new(&reply.body).attrs(),
        FXP_STATUS => Err(status(reply)
            .err()
            .unwrap_or_else(|| unexpected(FXP_STATUS))),
        kind => Err(unexpected(kind)),
    }
}

/// NAME 回复中的一项：文件名与属性。
type Name = (Vec<u8>, Metadata);

/// NAME 回复中的各项；目录读完时为 `None`。
fn names(reply: Reply) -> Result<Option<Vec<Name>>> {
    match reply.kind {
        FXP_NAME => {
            let mut d = Decoder::new(&reply.body);
            let count = d.u32()?;
            let mut out = Vec::with_capacity(count.min(1024) as usize);
            for _ in 0..count {
                let name = d.string()?.to_vec();
                d.string()?; // longname，`ls -l` 格式，不使用
                out.push((name, d.attrs()?));
            }
            Ok(Some(out))
        }
        FXP_STATUS if !status_or_eof(&reply)? => Ok(None),
        kind => Err(unexpected(kind)),
    }
}

/// DATA 回复；文件结束时为 `None`。
pub(crate) fn data(reply: Reply) -> Result<Option<Bytes>> {
    match reply.kind {
        FXP_DATA => {
            let mut d = Decoder::new(&reply.body);
            let len = d.string()?.len();
            // 跳过 u32 长度前缀，与报文共享内存
            Ok(Some(reply.body.slice(4..4 + len)))
        }
        FXP_STATUS if !status_or_eof(&reply)? => Ok(None),
        FXP_STATUS => Err(unexpected(FXP_STATUS)),
        kind => Err(unexpected(kind)),
    }
}

pub(crate) fn write_status(reply: Reply) -> Result<()> {
    status(reply)
}

#[async_trait]
impl Vfs for SftpClient {
    async fn home(&self) -> Result<VfsPath> {
        self.canonicalize(&VfsPath::from(".")).await
    }

    async fn canonicalize(&self, path: &VfsPath) -> Result<VfsPath> {
        let reply = self.path_request(FXP_REALPATH, path).await?;
        names(reply)?
            .and_then(|mut n| n.drain(..).next())
            .map(|(name, _)| VfsPath::new(name))
            .ok_or_else(|| VfsError::Protocol("REALPATH returned no path".into()))
    }

    async fn list(&self, dir: &VfsPath) -> Result<Vec<Entry>> {
        let handle = handle(self.path_request(FXP_OPENDIR, dir).await?)?;
        let mut entries = Vec::new();
        let result = loop {
            let reply = self
                .request(FXP_READDIR, |e| {
                    e.string(&handle);
                })
                .await;
            match reply.and_then(names) {
                Ok(Some(batch)) => {
                    entries.extend(
                        batch
                            .into_iter()
                            .filter(|(name, _)| {
                                let ok = VfsPath::is_component(name);
                                if !ok && name != b"." && name != b".." {
                                    tracing::warn!(?dir, name = ?VfsPath::new(name.clone()), "忽略无效的文件名");
                                }
                                ok
                            })
                            .map(|(name, metadata)| Entry {
                                path: dir.join(&name),
                                name,
                                metadata,
                            }),
                    );
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        let closed = self.close(&handle).await;
        result.and(closed)?;
        Ok(entries)
    }

    async fn stat(&self, path: &VfsPath) -> Result<Metadata> {
        attrs(self.path_request(FXP_STAT, path).await?)
    }

    async fn lstat(&self, path: &VfsPath) -> Result<Metadata> {
        attrs(self.path_request(FXP_LSTAT, path).await?)
    }

    async fn open_read(&self, path: &VfsPath, offset: u64) -> Result<Box<dyn FileReader>> {
        let handle = self.open(path, FXF_READ).await?;
        Ok(Box::new(SftpReader::new(self.clone(), handle, offset)))
    }

    async fn open_write(&self, path: &VfsPath, mode: WriteMode) -> Result<Box<dyn FileWriter>> {
        let (flags, offset) = match mode {
            WriteMode::CreateNew => (FXF_WRITE | FXF_CREAT | FXF_EXCL, 0),
            WriteMode::Truncate => (FXF_WRITE | FXF_CREAT | FXF_TRUNC, 0),
            WriteMode::Resume(offset) => (FXF_WRITE | FXF_CREAT, offset),
        };
        let handle = self.open(path, flags).await?;
        Ok(Box::new(SftpWriter::new(self.clone(), handle, offset)))
    }

    async fn mkdir(&self, path: &VfsPath) -> Result<()> {
        status(
            self.request(FXP_MKDIR, |e| {
                e.string(path.as_bytes()).attrs(&Metadata::default());
            })
            .await?,
        )
    }

    async fn remove_file(&self, path: &VfsPath) -> Result<()> {
        status(self.path_request(FXP_REMOVE, path).await?)
    }

    async fn remove_dir(&self, path: &VfsPath) -> Result<()> {
        status(self.path_request(FXP_RMDIR, path).await?)
    }

    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()> {
        // v3 的 RENAME 在目标存在时失败；OpenSSH 的扩展提供 rename(2) 语义
        let reply = if self.has_extension("posix-rename@openssh.com") {
            self.request(FXP_EXTENDED, |e| {
                e.string(b"posix-rename@openssh.com")
                    .string(from.as_bytes())
                    .string(to.as_bytes());
            })
            .await?
        } else {
            self.request(FXP_RENAME, |e| {
                e.string(from.as_bytes()).string(to.as_bytes());
            })
            .await?
        };
        status(reply)
    }

    async fn set_metadata(&self, path: &VfsPath, metadata: &Metadata) -> Result<()> {
        let attrs = Metadata {
            permissions: metadata.mode(),
            accessed: metadata.accessed,
            modified: metadata.modified,
            ..Default::default()
        };
        status(
            self.request(FXP_SETSTAT, |e| {
                e.string(path.as_bytes()).attrs(&attrs);
            })
            .await?,
        )
    }
}
