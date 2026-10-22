use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelStream};

/// 交互式 shell 通道上的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    Data(Vec<u8>),
    ExitStatus(u32),
    Closed,
}

pub struct ShellChannel {
    channel: Channel<Msg>,
}

impl ShellChannel {
    pub(crate) fn new(channel: Channel<Msg>) -> Self {
        Self { channel }
    }

    /// 等待下一个事件；通道关闭后持续返回 [`ShellEvent::Closed`]。
    pub async fn next_event(&mut self) -> ShellEvent {
        loop {
            match self.channel.wait().await {
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
        self.channel.data(data).await.map_err(Into::into)
    }

    /// 告知远端输入已结束（如脚本经标准输入送完）。
    pub async fn eof(&self) {
        let _ = self.channel.eof().await;
    }

    /// 关闭通道。
    pub async fn close(&self) {
        let _ = self.channel.close().await;
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), crate::SshError> {
        self.channel
            .window_change(cols as u32, rows as u32, 0, 0)
            .await
            .map_err(Into::into)
    }

    /// 转成字节流，交给上层协议（如在提权后的 sftp-server 上跑 SFTP）。之后到达的 stderr 被丢弃。
    pub fn into_stream(self) -> ChannelStream<Msg> {
        self.channel.into_stream()
    }
}
