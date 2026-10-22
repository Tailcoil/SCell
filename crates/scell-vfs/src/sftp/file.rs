//! 流水线读写：同一文件保持多个 READ / WRITE 请求在途，吞吐不受往返延迟限制。

use std::collections::VecDeque;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::oneshot;

use super::proto::{FXP_READ, FXP_WRITE};
use super::{Reply, SftpClient, data, write_status};
use crate::{FileReader, FileWriter, Result, VfsError};

/// 单个请求的数据量。OpenSSH 接受最大 256 KiB 的报文，32 KiB 是各实现普遍支持的大小。
const CHUNK: u32 = 32 * 1024;
/// 同一文件同时在途的请求数。
const DEPTH: usize = 64;

async fn recv(rx: oneshot::Receiver<Reply>) -> Result<Reply> {
    rx.await.map_err(|_| VfsError::ConnectionLost)
}

pub(crate) struct SftpReader {
    client: SftpClient,
    /// 读完或出错后置空，析构时不再关闭。
    handle: Option<Bytes>,
    /// 下一个要请求的偏移。
    next: u64,
    queue: VecDeque<(u64, oneshot::Receiver<Reply>)>,
    eof: bool,
}

impl SftpReader {
    pub(crate) fn new(client: SftpClient, handle: Bytes, offset: u64) -> Self {
        Self {
            client,
            handle: Some(handle),
            next: offset,
            queue: VecDeque::new(),
            eof: false,
        }
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.client.close_detached(&handle);
        }
    }
}

#[async_trait]
impl FileReader for SftpReader {
    async fn read(&mut self) -> Result<Option<Bytes>> {
        loop {
            let Some(handle) = self.handle.clone() else {
                return Ok(None);
            };
            if self.eof {
                self.close();
                return Ok(None);
            }
            while self.queue.len() < DEPTH {
                let offset = self.next;
                let rx = self.client.send(FXP_READ, |e| {
                    e.string(&handle).u64(offset).u32(CHUNK);
                })?;
                self.queue.push_back((offset, rx));
                self.next += u64::from(CHUNK);
            }
            let Some((offset, rx)) = self.queue.pop_front() else {
                continue;
            };
            match data(recv(rx).await?) {
                Ok(None) => {
                    // 之后在途请求的回复由读任务丢弃
                    self.queue.clear();
                    self.eof = true;
                }
                Ok(Some(chunk)) => {
                    // 读得比请求少：后面在途请求的偏移都不对了，丢弃后从实际位置重新请求
                    if chunk.len() < CHUNK as usize {
                        self.queue.clear();
                        self.next = offset + chunk.len() as u64;
                    }
                    if !chunk.is_empty() {
                        return Ok(Some(chunk));
                    }
                }
                Err(e) => {
                    self.close();
                    return Err(e);
                }
            }
        }
    }
}

impl Drop for SftpReader {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct SftpWriter {
    client: SftpClient,
    handle: Option<Bytes>,
    offset: u64,
    queue: VecDeque<oneshot::Receiver<Reply>>,
}

impl SftpWriter {
    pub(crate) fn new(client: SftpClient, handle: Bytes, offset: u64) -> Self {
        Self {
            client,
            handle: Some(handle),
            offset,
            queue: VecDeque::new(),
        }
    }

    async fn wait_one(&mut self) -> Result<()> {
        match self.queue.pop_front() {
            Some(rx) => write_status(recv(rx).await?),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl FileWriter for SftpWriter {
    async fn write(&mut self, mut data: Bytes) -> Result<()> {
        let handle = self.handle.clone().ok_or(VfsError::ConnectionLost)?;
        while !data.is_empty() {
            while self.queue.len() >= DEPTH {
                self.wait_one().await?;
            }
            let chunk = data.split_to(data.len().min(CHUNK as usize));
            let offset = self.offset;
            let rx = self.client.send(FXP_WRITE, |e| {
                e.string(&handle).u64(offset).string(&chunk);
            })?;
            self.queue.push_back(rx);
            self.offset += chunk.len() as u64;
        }
        Ok(())
    }

    async fn finish(mut self: Box<Self>) -> Result<()> {
        let mut result = Ok(());
        while !self.queue.is_empty() {
            if let Err(e) = self.wait_one().await {
                result = result.and(Err(e));
            }
        }
        let handle = self.handle.take().ok_or(VfsError::ConnectionLost)?;
        let closed = super::status(
            recv(self.client.send(super::proto::FXP_CLOSE, |e| {
                e.string(&handle);
            })?)
            .await?,
        );
        result.and(closed)
    }
}

impl Drop for SftpWriter {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.client.close_detached(&handle);
        }
    }
}
