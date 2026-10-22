//! 传输引擎：本地 ↔ 本地，以及本地 → SFTP（本机 sftp-server）。

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use scell_vfs::transfer::{ConflictAction, ConflictPolicy, TransferState};
use scell_vfs::{
    LocalFs, TransferEngine, TransferEvent, TransferId, TransferInfo, TransferRequest, Vfs, VfsPath,
};

fn local(path: &std::path::Path) -> VfsPath {
    LocalFs::from_path(path)
}

fn engine(concurrency: usize) -> (TransferEngine, flume::Receiver<TransferEvent>) {
    TransferEngine::new(tokio::runtime::Handle::current(), concurrency)
}

fn request(
    source: Arc<dyn Vfs>,
    from: VfsPath,
    dest: Arc<dyn Vfs>,
    to: VfsPath,
    conflict: ConflictPolicy,
) -> TransferRequest {
    TransferRequest {
        source,
        source_path: from,
        dest,
        dest_path: to,
        conflict,
        preserve: true,
    }
}

/// 处理事件直到 `id` 满足条件，冲突交给 `on_conflict` 回答。
async fn wait(
    events: &flume::Receiver<TransferEvent>,
    id: TransferId,
    mut done: impl FnMut(&TransferInfo) -> bool,
    mut on_conflict: impl FnMut(&scell_vfs::transfer::Conflict),
) -> TransferInfo {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv_async())
            .await
            .expect("等待传输事件超时")
            .unwrap();
        match event {
            TransferEvent::Updated(info) if info.id == id && done(&info) => return info,
            TransferEvent::Conflict(c) => on_conflict(&c),
            _ => {}
        }
    }
}

async fn finished(events: &flume::Receiver<TransferEvent>, id: TransferId) -> TransferInfo {
    wait(
        events,
        id,
        |i| i.state.is_finished(),
        |c| panic!("意外的冲突 {c:?}"),
    )
    .await
}

fn tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("top.txt"), "top").unwrap();
    std::fs::write(root.join("a/b/deep.bin"), vec![7u8; 300_000]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let gbk = std::ffi::OsStr::from_bytes(b"\xd6\xd0\xce\xc4.txt");
        std::fs::write(root.join("a").join(gbk), "gbk").unwrap();
        std::os::unix::fs::symlink("top.txt", root.join("link")).unwrap();
    }
    // Windows 上改时间需要写属性权限，只读打开会被拒绝
    std::fs::File::options()
        .write(true)
        .open(root.join("top.txt"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_500_000_000))
        .unwrap();
}

#[tokio::test]
async fn uploads_directory_tree_to_sftp_preserving_metadata() {
    let Some((sftp, _child)) = common::sftp().await else {
        return;
    };
    let sftp = Arc::new(sftp);
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    tree(src.path());
    let (engine, events) = engine(2);
    let id = engine.enqueue(request(
        Arc::new(LocalFs),
        local(src.path()),
        sftp.clone(),
        local(&dst.path().join("copy")),
        ConflictPolicy::Ask,
    ));
    let info = finished(&events, id).await;
    assert_eq!(info.state, TransferState::Done);
    let files = if cfg!(unix) { 3 } else { 2 };
    assert_eq!((info.files_done, info.files_total), (files, files));
    assert_eq!(info.bytes_done, info.bytes_total);

    let copy = dst.path().join("copy");
    assert_eq!(
        std::fs::read(copy.join("a/b/deep.bin")).unwrap(),
        vec![7u8; 300_000]
    );
    assert!(!copy.join("link").exists(), "符号链接不复制");
    let m = sftp.stat(&local(&copy.join("top.txt"))).await.unwrap();
    assert_eq!(m.modified, Some(1_500_000_000));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let gbk = std::ffi::OsStr::from_bytes(b"\xd6\xd0\xce\xc4.txt");
        assert_eq!(std::fs::read(copy.join("a").join(gbk)).unwrap(), b"gbk");
    }
}

#[tokio::test]
async fn conflict_policies() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    std::fs::write(&src, "0123456789").unwrap();
    let fs: Arc<dyn Vfs> = Arc::new(LocalFs);
    let (engine, events) = engine(4);
    let run = |dst: &str, policy| {
        engine.enqueue(request(
            fs.clone(),
            local(&src),
            fs.clone(),
            local(&dir.path().join(dst)),
            policy,
        ))
    };

    std::fs::write(dir.path().join("skip.txt"), "old").unwrap();
    let id = run("skip.txt", ConflictPolicy::Skip);
    assert_eq!(finished(&events, id).await.state, TransferState::Skipped);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("skip.txt")).unwrap(),
        "old"
    );

    std::fs::write(dir.path().join("dup.txt"), "old").unwrap();
    std::fs::write(dir.path().join("dup (1).txt"), "old").unwrap();
    let id = run("dup.txt", ConflictPolicy::Rename);
    assert_eq!(finished(&events, id).await.state, TransferState::Done);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dup (2).txt")).unwrap(),
        "0123456789"
    );

    // 目标是源的前缀：续传只补后半段
    std::fs::write(dir.path().join("part.txt"), "0123").unwrap();
    let id = run("part.txt", ConflictPolicy::Resume);
    finished(&events, id).await;
    assert_eq!(
        std::fs::read_to_string(dir.path().join("part.txt")).unwrap(),
        "0123456789"
    );

    std::fs::write(dir.path().join("over.txt"), "old content longer").unwrap();
    let id = run("over.txt", ConflictPolicy::Overwrite);
    finished(&events, id).await;
    assert_eq!(
        std::fs::read_to_string(dir.path().join("over.txt")).unwrap(),
        "0123456789"
    );
}

#[tokio::test]
async fn ask_with_apply_to_all_only_asks_once() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    for name in ["1", "2", "3"] {
        std::fs::write(src.path().join(name), "new").unwrap();
        std::fs::write(dst.path().join(name), "old").unwrap();
    }
    let fs: Arc<dyn Vfs> = Arc::new(LocalFs);
    let (engine, events) = engine(1);
    let id = engine.enqueue(request(
        fs.clone(),
        local(src.path()),
        fs,
        local(dst.path()),
        ConflictPolicy::Ask,
    ));
    let mut asked = 0;
    let info = wait(
        &events,
        id,
        |i| i.state.is_finished(),
        |c| {
            asked += 1;
            assert_eq!(c.existing.size, Some(3));
            c.answer(ConflictAction::Overwrite, true);
        },
    )
    .await;
    assert_eq!(info.state, TransferState::Done);
    assert_eq!(asked, 1);
    for name in ["1", "2", "3"] {
        assert_eq!(
            std::fs::read_to_string(dst.path().join(name)).unwrap(),
            "new"
        );
    }
}

#[tokio::test]
async fn pause_resume_cancel_and_retry_over_sftp() {
    let Some((sftp, _child)) = common::sftp().await else {
        return;
    };
    let sftp = Arc::new(sftp);
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..40_000_000u32).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &data).unwrap();
    let (engine, events) = engine(1);
    let send = |to: &str| {
        engine.enqueue(request(
            Arc::new(LocalFs),
            local(&src),
            sftp.clone(),
            local(&dir.path().join(to)),
            ConflictPolicy::Ask,
        ))
    };

    // 开始传输后暂停（本机传得很快，不等进度事件），进度停住；继续后完整传完
    let id = send("paused.bin");
    wait(&events, id, |i| i.state == TransferState::Running, |_| {}).await;
    engine.pause(id);
    let paused = wait(&events, id, |i| i.state == TransferState::Paused, |_| {}).await;
    assert!(paused.bytes_done < paused.bytes_total);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = engine.list().into_iter().find(|i| i.id == id).unwrap();
    assert_eq!(still.bytes_done, paused.bytes_done, "暂停后不再传输");
    engine.resume(id);
    assert_eq!(finished(&events, id).await.state, TransferState::Done);
    assert_eq!(std::fs::read(dir.path().join("paused.bin")).unwrap(), data);

    // 取消从头写的传输会删除半截文件
    let id = send("cancel.bin");
    wait(&events, id, |i| i.state == TransferState::Running, |_| {}).await;
    engine.cancel(id);
    assert_eq!(finished(&events, id).await.state, TransferState::Cancelled);
    assert!(!dir.path().join("cancel.bin").exists());

    // 已有半截文件时重试，从断点续传
    std::fs::write(dir.path().join("retry.bin"), &data[..1_234_567]).unwrap();
    let id = send("retry.bin");
    engine.cancel(id);
    let state = wait(
        &events,
        id,
        |i| i.state.is_finished(),
        |c| c.answer(ConflictAction::Cancel, false),
    )
    .await
    .state;
    assert_eq!(state, TransferState::Cancelled);
    let id = engine.retry(id).unwrap();
    let info = finished(&events, id).await;
    assert_eq!(info.state, TransferState::Done);
    assert_eq!(std::fs::read(dir.path().join("retry.bin")).unwrap(), data);
    assert_eq!(engine.list().len(), 3);
    assert!(engine.remove(id));
}

#[tokio::test]
async fn respects_concurrency_limit() {
    let dir = tempfile::tempdir().unwrap();
    let fs: Arc<dyn Vfs> = Arc::new(LocalFs);
    let (engine, events) = engine(1);
    let mut ids = Vec::new();
    for i in 0..3 {
        let src = dir.path().join(format!("s{i}"));
        std::fs::write(&src, vec![1u8; 5_000_000]).unwrap();
        ids.push(engine.enqueue(request(
            fs.clone(),
            local(&src),
            fs.clone(),
            local(&dir.path().join(format!("d{i}"))),
            ConflictPolicy::Overwrite,
        )));
    }
    let mut running = std::collections::HashSet::new();
    let mut done = 0;
    while done < ids.len() {
        let TransferEvent::Updated(info) = events.recv_async().await.unwrap() else {
            continue;
        };
        match info.state {
            TransferState::Running => {
                running.insert(info.id);
            }
            _ => {
                running.remove(&info.id);
            }
        }
        assert!(running.len() <= 1, "同时运行的任务超过上限");
        if info.state == TransferState::Done {
            done += 1;
        }
    }
}
