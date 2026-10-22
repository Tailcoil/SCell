use crate::{Prompt, SessionId};

/// 会话层上报给 UI 的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionEvent {
    /// 终端内容有更新。同一会话在 UI 确认前只发一次，见 [`crate::SessionHandle::ack_wakeup`]。
    Wakeup(SessionId),
    Title(SessionId, Option<String>),
    Bell(SessionId),
    WorkingDirectory(SessionId, String),
    ClipboardStore(SessionId, String),
    Exited {
        id: SessionId,
        code: Option<u32>,
    },
    /// 需要用户回答的问题，见 [`Prompt`]。
    Prompt(SessionId, Prompt),
    /// 远程会话开始（重新）连接。
    Connecting(SessionId),
    /// 远程会话已建立（认证完成、shell 已打开）。
    Connected(SessionId),
    /// 远程连接断开或建立失败，回滚内容保留，可经 [`crate::SessionHandle::reconnect`] 重连。
    /// `retry_in` 为自动重连的等待时间；不会自动重连时为 `None`。`reason` 为一行原因（终端里也会写出）。
    Disconnected {
        id: SessionId,
        retry_in: Option<std::time::Duration>,
        reason: String,
    },
}
