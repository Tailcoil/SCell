use crate::VfsPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    Symlink,
    Other,
}

/// 文件属性，字段与 SFTP v3 的 ATTRS 对应；服务器未提供的项为 `None`。时间为 Unix 秒。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// 含文件类型位（`S_IFMT`）的完整 mode。
    pub permissions: Option<u32>,
    pub accessed: Option<u64>,
    pub modified: Option<u64>,
}

const S_IFMT: u32 = 0o170_000;
pub(crate) const S_IFDIR: u32 = 0o040_000;
pub(crate) const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;

impl Metadata {
    pub fn file_type(&self) -> FileType {
        match self.permissions.map(|p| p & S_IFMT) {
            Some(S_IFDIR) => FileType::Dir,
            Some(S_IFREG) => FileType::File,
            Some(S_IFLNK) => FileType::Symlink,
            _ => FileType::Other,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == FileType::Dir
    }

    /// 权限位（`0o7777` 以内），不含类型。
    pub fn mode(&self) -> Option<u32> {
        self.permissions.map(|p| p & 0o7777)
    }
}

/// 目录中的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 文件名的原始字节。
    pub name: Vec<u8>,
    pub path: VfsPath,
    pub metadata: Metadata,
}

/// 打开文件写入的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// 新建；已存在时报 [`crate::VfsError::AlreadyExists`]（SFTP 服务器可能报一般失败）。
    CreateNew,
    /// 新建或清空已有文件。
    Truncate,
    /// 续传：从 `offset` 处接着写，文件不存在时新建。
    Resume(u64),
}
