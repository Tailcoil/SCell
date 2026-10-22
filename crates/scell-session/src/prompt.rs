//! 会话向用户提问：主机密钥确认、密码、私钥口令、keyboard-interactive。问题随 [`crate::SessionEvent::Prompt`] 上抛，
//! UI 在所属窗格内显示卡片并调用 [`Prompt::answer`]；提问方异步等待回答，不阻塞其他会话。

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
pub use scell_ssh::InteractivePrompt;
use tokio::sync::oneshot;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromptKind {
    /// known_hosts 中没有记录的新主机，确认后写入。
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
    /// `retry` 为真表示上一次输入（或凭据库中保存的）不正确。
    Password {
        user: String,
        host: String,
        retry: bool,
    },
    Passphrase {
        path: PathBuf,
        retry: bool,
    },
    /// 服务器发来的一轮问题，以 [`PromptAnswer::Responses`] 按序作答。
    Interactive {
        user: String,
        host: String,
        name: String,
        instructions: String,
        prompts: Vec<InteractivePrompt>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    /// 确认类问题的“是”。
    Accept,
    /// `remember` 为真时，认证成功后存入系统凭据库。
    Secret {
        value: String,
        remember: bool,
    },
    Responses(Vec<String>),
    Cancel,
}

/// 一个待回答的问题。可克隆以便事件分发，只有第一次回答生效。
#[derive(Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    reply: Arc<Mutex<Option<oneshot::Sender<PromptAnswer>>>>,
}

impl Prompt {
    pub(crate) fn new(kind: PromptKind) -> (Self, oneshot::Receiver<PromptAnswer>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                kind,
                reply: Arc::new(Mutex::new(Some(tx))),
            },
            rx,
        )
    }

    pub fn answer(&self, answer: PromptAnswer) {
        if let Some(tx) = self.reply.lock().take() {
            let _ = tx.send(answer);
        }
    }

    pub fn is_answered(&self) -> bool {
        self.reply.lock().is_none()
    }
}

impl std::fmt::Debug for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prompt").field("kind", &self.kind).finish()
    }
}

impl PartialEq for Prompt {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

impl Eq for Prompt {}
