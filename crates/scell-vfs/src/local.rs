//! 本机文件系统。阻塞的系统调用经 tokio::fs 放到阻塞线程池。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::{Entry, FileReader, FileWriter, Metadata, Result, Vfs, VfsError, VfsPath, WriteMode};

/// 本地读取的块大小。
const CHUNK: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFs;

impl LocalFs {
    pub fn to_path(path: &VfsPath) -> PathBuf {
        platform::to_path(path.as_bytes())
    }

    pub fn from_path(path: &Path) -> VfsPath {
        VfsPath::new(platform::from_path(path))
    }
}

fn metadata(m: &std::fs::Metadata) -> Metadata {
    let secs = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    let mut out = Metadata {
        size: Some(m.len()),
        accessed: secs(m.accessed()),
        modified: secs(m.modified()),
        ..Default::default()
    };
    platform::fill(m, &mut out);
    out
}

#[async_trait]
impl Vfs for LocalFs {
    async fn home(&self) -> Result<VfsPath> {
        let home = std::env::home_dir().ok_or(VfsError::NotFound)?;
        Ok(Self::from_path(&home))
    }

    async fn canonicalize(&self, path: &VfsPath) -> Result<VfsPath> {
        let real = tokio::fs::canonicalize(Self::to_path(path)).await?;
        Ok(Self::from_path(&platform::simplify(real)))
    }

    async fn list(&self, dir: &VfsPath) -> Result<Vec<Entry>> {
        let mut rd = tokio::fs::read_dir(Self::to_path(dir)).await?;
        let mut entries = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            let name = platform::from_path(Path::new(&entry.file_name()));
            // 列目录与读属性之间文件可能已被删除，跳过即可
            let Ok(m) = entry.metadata().await else {
                continue;
            };
            entries.push(Entry {
                path: dir.join(&name),
                name,
                metadata: metadata(&m),
            });
        }
        Ok(entries)
    }

    async fn stat(&self, path: &VfsPath) -> Result<Metadata> {
        Ok(metadata(&tokio::fs::metadata(Self::to_path(path)).await?))
    }

    async fn lstat(&self, path: &VfsPath) -> Result<Metadata> {
        Ok(metadata(
            &tokio::fs::symlink_metadata(Self::to_path(path)).await?,
        ))
    }

    async fn open_read(&self, path: &VfsPath, offset: u64) -> Result<Box<dyn FileReader>> {
        let mut file = tokio::fs::File::open(Self::to_path(path)).await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        Ok(Box::new(LocalReader(file)))
    }

    async fn open_write(&self, path: &VfsPath, mode: WriteMode) -> Result<Box<dyn FileWriter>> {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true);
        match mode {
            WriteMode::CreateNew => options.create_new(true),
            WriteMode::Truncate => options.create(true).truncate(true),
            WriteMode::Resume(_) => options.create(true),
        };
        let mut file = options.open(Self::to_path(path)).await?;
        if let WriteMode::Resume(offset) = mode {
            // 丢掉偏移之后上次没写完整的内容
            file.set_len(offset).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        Ok(Box::new(LocalWriter(file)))
    }

    async fn mkdir(&self, path: &VfsPath) -> Result<()> {
        Ok(tokio::fs::create_dir(Self::to_path(path)).await?)
    }

    async fn remove_file(&self, path: &VfsPath) -> Result<()> {
        Ok(tokio::fs::remove_file(Self::to_path(path)).await?)
    }

    async fn remove_dir(&self, path: &VfsPath) -> Result<()> {
        Ok(tokio::fs::remove_dir(Self::to_path(path)).await?)
    }

    async fn rename(&self, from: &VfsPath, to: &VfsPath) -> Result<()> {
        Ok(tokio::fs::rename(Self::to_path(from), Self::to_path(to)).await?)
    }

    async fn set_metadata(&self, path: &VfsPath, metadata: &Metadata) -> Result<()> {
        let path = Self::to_path(path);
        let metadata = metadata.clone();
        tokio::task::spawn_blocking(move || platform::set_metadata(&path, &metadata))
            .await
            .map_err(|e| VfsError::Io(std::io::Error::other(e)))??;
        Ok(())
    }
}

struct LocalReader(tokio::fs::File);

#[async_trait]
impl FileReader for LocalReader {
    async fn read(&mut self) -> Result<Option<Bytes>> {
        let mut buf = BytesMut::with_capacity(CHUNK);
        while buf.len() < CHUNK {
            if self.0.read_buf(&mut buf).await? == 0 {
                break;
            }
        }
        Ok((!buf.is_empty()).then(|| buf.freeze()))
    }
}

struct LocalWriter(tokio::fs::File);

#[async_trait]
impl FileWriter for LocalWriter {
    async fn write(&mut self, data: Bytes) -> Result<()> {
        Ok(self.0.write_all(&data).await?)
    }

    async fn finish(mut self: Box<Self>) -> Result<()> {
        Ok(self.0.flush().await?)
    }
}

#[cfg(unix)]
mod platform {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use crate::Metadata;

    pub(super) fn to_path(bytes: &[u8]) -> PathBuf {
        PathBuf::from(OsStr::from_bytes(bytes))
    }

    pub(super) fn from_path(path: &Path) -> Vec<u8> {
        path.as_os_str().as_bytes().to_vec()
    }

    pub(super) fn simplify(path: PathBuf) -> PathBuf {
        path
    }

    pub(super) fn fill(m: &std::fs::Metadata, out: &mut Metadata) {
        out.permissions = Some(m.mode());
        out.uid = Some(m.uid());
        out.gid = Some(m.gid());
    }

    pub(super) fn set_metadata(path: &Path, m: &Metadata) -> std::io::Result<()> {
        // 先改时间：只读权限下也能改，但顺序反过来可能先失去访问权
        if m.accessed.is_some() || m.modified.is_some() {
            let file = std::fs::File::open(path)?;
            file.set_times(super::times(m))?;
        }
        if let Some(mode) = m.mode() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::path::{Path, PathBuf};

    use crate::Metadata;
    use crate::types::{S_IFDIR, S_IFREG};

    /// Windows 本地路径一律按 UTF-8 处理，`/` 与 `\` 均可。
    pub(super) fn to_path(bytes: &[u8]) -> PathBuf {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }

    pub(super) fn from_path(path: &Path) -> Vec<u8> {
        path.to_string_lossy().replace('\\', "/").into_bytes()
    }

    /// 去掉 canonicalize 返回的 `\\?\` 前缀。
    pub(super) fn simplify(path: PathBuf) -> PathBuf {
        match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
            Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
            _ => path,
        }
    }

    pub(super) fn fill(m: &std::fs::Metadata, out: &mut Metadata) {
        let mode = if m.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        let exec = if m.is_dir() { 0o111 } else { 0 };
        // 没有 mode，据目录与否合成类型位，与 SFTP 的属性保持一致
        let kind = if m.is_dir() { S_IFDIR } else { S_IFREG };
        out.permissions = Some(kind | mode | exec);
    }

    pub(super) fn set_metadata(path: &Path, m: &Metadata) -> std::io::Result<()> {
        if m.accessed.is_some() || m.modified.is_some() {
            // 设置时间需要写属性权限
            let file = std::fs::OpenOptions::new().write(true).open(path)?;
            file.set_times(super::times(m))?;
        }
        if let Some(mode) = m.mode() {
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_readonly(mode & 0o200 == 0);
            std::fs::set_permissions(path, perms)?;
        }
        Ok(())
    }
}

fn times(m: &Metadata) -> std::fs::FileTimes {
    let at = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let mut times = std::fs::FileTimes::new();
    if let Some(a) = m.accessed {
        times = times.set_accessed(at(a));
    }
    if let Some(t) = m.modified {
        times = times.set_modified(at(t));
    }
    times
}
