//! 文件系统抽象与传输引擎。本地与 SFTP 共用 [`Vfs`] 接口，传输引擎只依赖该接口。
//!
//! 路径以原始字节保存（[`VfsPath`]），由上层按主机编码解码后显示，回写时用原始字节，
//! 非 UTF-8 文件名因此可以无损地打开、重命名与删除。

rust_i18n::i18n!("locales", fallback = "en");

mod error;
mod local;
mod path;
mod sftp;
pub mod transfer;
mod types;

use async_trait::async_trait;
use bytes::Bytes;

pub use error::{Result, VfsError};
pub use local::LocalFs;
pub use path::VfsPath;
pub use sftp::SftpClient;
pub use transfer::{TransferEngine, TransferEvent, TransferId, TransferInfo, TransferRequest};
pub use types::{Entry, FileType, Metadata, WriteMode};

/// 文件系统。所有方法都可并发调用。
#[async_trait]
pub trait Vfs: Send + Sync {
    /// 登录后的初始目录（本地为家目录，SFTP 为服务器给出的当前目录）。
    async fn home(&self) -> Result<VfsPath>;
    async fn canonicalize(&self, path: &VfsPath) -> Result<VfsPath>;
    /// 列出目录内容，不含 `.` 与 `..`；元数据不跟随符号链接。
    async fn list(&self, dir: &VfsPath) -> Result<Vec<Entry>>;
    /// 跟随符号链接。
    async fn stat(&self, path: &VfsPath) -> Result<Metadata>;
    /// 不跟随符号链接。
    async fn lstat(&self, path: &VfsPath) -> Result<Metadata>;
    /// 从 `offset` 开始顺序读取。
    async fn open_read(&self, path: &VfsPath, offset: u64) -> Result<Box<dyn FileReader>>;
    async fn open_write(&self, path: &VfsPath, mode: WriteMode) -> Result<Box<dyn FileWriter>>;
    async fn mkdir(&self, path: &VfsPath) -> Result<()>;
    async fn remove_file(&self, path: &VfsPath) -> Result<()>;
    /// 只删除空目录，递归删除见 [`remove_all`]。
    async fn remove_dir(&self, path: &VfsPath) -> Result<()>;
    /// 目标已存在时覆盖。不支持 `posix-rename@openssh.com` 的 SFTP 服务器上目标存在会失败。
    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()>;
    /// 修改权限与时间；`metadata` 中为 `None` 的项不变，大小、属主忽略。
    async fn set_metadata(&self, path: &VfsPath, metadata: &Metadata) -> Result<()>;
}

/// 顺序读取文件内容。
#[async_trait]
pub trait FileReader: Send {
    /// 下一块数据；读完时返回 `None`。块大小由实现决定。
    async fn read(&mut self) -> Result<Option<Bytes>>;
}

/// 顺序写入文件内容。写入可能在后台流水线进行，错误可能延后到之后的 `write` 或 `finish` 才报告。
#[async_trait]
pub trait FileWriter: Send {
    async fn write(&mut self, data: Bytes) -> Result<()>;
    /// 等待全部写入完成并关闭文件。不调用而直接丢弃时，已提交的数据可能不完整。
    async fn finish(self: Box<Self>) -> Result<()>;
}

/// 递归删除文件或目录。符号链接本身被删除，不跟随。
pub async fn remove_all(vfs: &dyn Vfs, path: &VfsPath) -> Result<()> {
    if vfs.lstat(path).await?.file_type() != FileType::Dir {
        return vfs.remove_file(path).await;
    }
    // 深度优先，用显式栈避免递归 async
    let mut stack = vec![(path.clone(), false)];
    while let Some((dir, listed)) = stack.pop() {
        if listed {
            vfs.remove_dir(&dir).await?;
            continue;
        }
        stack.push((dir.clone(), true));
        for entry in vfs.list(&dir).await? {
            if entry.metadata.file_type() == FileType::Dir {
                stack.push((entry.path, false));
            } else {
                vfs.remove_file(&entry.path).await?;
            }
        }
    }
    Ok(())
}
