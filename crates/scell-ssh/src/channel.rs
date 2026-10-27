use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelStream};
use tokio_util::sync::CancellationToken;

use crate::liveness::Guarded;

/// 交互式 shell 通道上的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    Data(Vec<u8>),
    ExitStatus(u32),
    Closed,
}

/// 会话通道。所在连接断开后，读写都立即返回，不会停在等对端窗口上。
pub struct ShellChannel {
    channel: Channel<Msg>,
    /// 所在连接的断开信号。
    dead: CancellationToken,
}

fn disconnected() -> crate::SshError {
    russh::Error::Disconnect.into()
}

impl ShellChannel {
    pub(crate) fn new(channel: Channel<Msg>, dead: CancellationToken) -> Self {
        Self { channel, dead }
    }

    /// 等待下一个事件；通道关闭或连接断开后持续返回 [`ShellEvent::Closed`]。
    pub async fn next_event(&mut self) -> ShellEvent {
        loop {
            let msg = tokio::select! {
                // 先取完已收到的输出
                biased;
                msg = self.channel.wait() => msg,
                () = self.dead.cancelled() => None,
            };
            match msg {
                Some(ChannelMsg::Data { data }) => return ShellEvent::Data(data.to_vec()),
                // stderr 与 stdout 在 PTY 会话中同样显示在终端里
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    return ShellEvent::Data(data.to_vec());
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    return ShellEvent::ExitStatus(exit_status);
                }
                Some(ChannelMsg::Close) | None => return ShellEvent::Closed,
                Some(_) => continue,
            }
        }
    }

    pub async fn write(&self, data: &[u8]) -> Result<(), crate::SshError> {
        tokio::select! {
            r = self.channel.data(data) => r.map_err(Into::into),
            () = self.dead.cancelled() => Err(disconnected()),
        }
    }

    /// 告知远端输入已结束（如脚本经标准输入送完）。
    pub async fn eof(&self) {
        tokio::select! {
            _ = self.channel.eof() => {}
            () = self.dead.cancelled() => {}
        }
    }

    /// 关闭通道。
    pub async fn close(&self) {
        tokio::select! {
            _ = self.channel.close() => {}
            () = self.dead.cancelled() => {}
        }
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), crate::SshError> {
        tokio::select! {
            r = self.channel.window_change(cols as u32, rows as u32, 0, 0) => r.map_err(Into::into),
            () = self.dead.cancelled() => Err(disconnected()),
        }
    }

    /// 转成字节流，交给上层协议（如在提权后的 sftp-server 上跑 SFTP）。之后到达的 stderr 被丢弃。
    pub fn into_stream(self) -> Guarded<ChannelStream<Msg>> {
        Guarded::channel(self.channel.into_stream(), &self.dead)
    }
}
