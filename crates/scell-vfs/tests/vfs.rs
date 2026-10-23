//! 同一组用例分别针对本地文件系统与 SFTP（本机 sftp-server）。

#![allow(clippy::unwrap_used)]

mod common;

use bytes::Bytes;
use scell_vfs::{FileType, LocalFs, Metadata, Vfs, VfsError, VfsPath, WriteMode, remove_all};

fn root(dir: &tempfile::TempDir) -> VfsPath {
    LocalFs::from_path(dir.path())
}

/// 内容不重复的测试数据，错位读写能被发现。
fn pattern(len: usize) -> Vec<u8> {
    (0..len as u32)
        .flat_map(|i| i.to_le_bytes())
        .take(len)
        .collect()
}

async fn read_all(vfs: &dyn Vfs, path: &VfsPath, offset: u64) -> Vec<u8> {
    let mut reader = vfs.open_read(path, offset).await.unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = reader.read().await.unwrap() {
        out.extend_from_slice(&chunk);
    }
    out
}

async fn write_all(vfs: &dyn Vfs, path: &VfsPath, mode: WriteMode, data: &[u8]) {
    let mut writer = vfs.open_write(path, mode).await.unwrap();
    // 不规则的块，跨越实现内部的分块边界
    for chunk in data.chunks(100_003) {
        writer.write(Bytes::copy_from_slice(chunk)).await.unwrap();
    }
    writer.finish().await.unwrap();
}

async fn suite(vfs: &dyn Vfs, root: VfsPath) {
    // 大文件：多轮流水线读写，结尾不足一块
    let big = root.join(b"big.bin");
    let data = pattern(3 * 1024 * 1024 + 12345);
    write_all(vfs, &big, WriteMode::Truncate, &data).await;
    assert_eq!(vfs.stat(&big).await.unwrap().size, Some(data.len() as u64));
    assert_eq!(read_all(vfs, &big, 0).await, data);
    assert_eq!(read_all(vfs, &big, 1_000_000).await, &data[1_000_000..]);

    // 续传：先写一半，再从断点接着写
    let part = root.join(b"part.bin");
    write_all(vfs, &part, WriteMode::Truncate, &data[..777_777]).await;
    write_all(vfs, &part, WriteMode::Resume(777_777), &data[777_777..]).await;
    assert_eq!(read_all(vfs, &part, 0).await, data);

    // 空文件与新建冲突
    let empty = root.join(b"empty");
    write_all(vfs, &empty, WriteMode::CreateNew, b"").await;
    assert!(read_all(vfs, &empty, 0).await.is_empty());
    assert!(vfs.open_write(&empty, WriteMode::CreateNew).await.is_err());

    // 非 UTF-8 文件名：GBK 编码的“中文”，按原始字节创建、列出、改名、删除。
    // Windows 的文件名是 UTF-16、macOS 的 APFS 只接受 UTF-8，任意字节存不进去，改用 UTF-8 的中文名
    let (name, renamed_name): (&[u8], &[u8]) = if cfg!(all(unix, not(target_os = "macos"))) {
        (b"\xd6\xd0\xce\xc4.txt", b"\xce\xc4.txt")
    } else {
        ("中文.txt".as_bytes(), "文.txt".as_bytes())
    };
    let dir = root.join(b"sub");
    vfs.mkdir(&dir).await.unwrap();
    let gbk = dir.join(name);
    write_all(vfs, &gbk, WriteMode::Truncate, b"gbk").await;
    let entries = vfs.list(&dir).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, name);
    assert_eq!(entries[0].path, gbk);
    assert_eq!(entries[0].metadata.file_type(), FileType::File);
    let renamed = dir.join(renamed_name);
    vfs.rename(&gbk, &renamed).await.unwrap();
    assert_eq!(read_all(vfs, &renamed, 0).await, b"gbk");
    assert!(matches!(vfs.stat(&gbk).await, Err(VfsError::NotFound)));

    // 改名覆盖已存在的目标
    vfs.rename(&renamed, &empty).await.unwrap();
    assert_eq!(read_all(vfs, &empty, 0).await, b"gbk");

    // 权限与修改时间
    vfs.set_metadata(
        &empty,
        &Metadata {
            permissions: Some(0o600),
            modified: Some(1_600_000_000),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let m = vfs.stat(&empty).await.unwrap();
    assert_eq!(m.modified, Some(1_600_000_000));
    #[cfg(unix)]
    assert_eq!(m.mode(), Some(0o600));

    // 目录属性、规范化、递归删除
    assert!(vfs.stat(&dir).await.unwrap().is_dir());
    let nested = dir.join(b"a");
    vfs.mkdir(&nested).await.unwrap();
    write_all(vfs, &nested.join(b"f"), WriteMode::Truncate, b"x").await;
    let dotted = VfsPath::new([nested.as_bytes(), b"/../a"].concat());
    assert_eq!(
        vfs.canonicalize(&dotted).await.unwrap(),
        vfs.canonicalize(&nested).await.unwrap()
    );
    assert!(vfs.remove_dir(&dir).await.is_err(), "非空目录不能直接删除");
    remove_all(vfs, &dir).await.unwrap();
    assert!(matches!(vfs.lstat(&dir).await, Err(VfsError::NotFound)));
    assert!(matches!(
        vfs.open_read(&root.join(b"missing"), 0).await,
        Err(VfsError::NotFound)
    ));

    let mut names: Vec<_> = vfs
        .list(&root)
        .await
        .unwrap()
        .into_iter()
        .map(|e| String::from_utf8(e.name).unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["big.bin", "empty", "part.bin"]);
}

#[tokio::test]
async fn local_fs() {
    let dir = tempfile::tempdir().unwrap();
    suite(&LocalFs, root(&dir)).await;
}

#[tokio::test]
async fn sftp_against_openssh_sftp_server() {
    let Some((client, _child)) = common::sftp().await else {
        return;
    };
    assert!(client.has_extension("posix-rename@openssh.com"));
    let dir = tempfile::tempdir().unwrap();
    assert!(!client.home().await.unwrap().as_bytes().is_empty());
    suite(&client, root(&dir)).await;
}

#[tokio::test]
async fn pending_requests_fail_when_server_exits() {
    let Some((client, mut child)) = common::sftp().await else {
        return;
    };
    child.kill().await.unwrap();
    let err = client.stat(&VfsPath::from("/")).await.unwrap_err();
    assert!(matches!(err, VfsError::ConnectionLost), "{err:?}");
}
