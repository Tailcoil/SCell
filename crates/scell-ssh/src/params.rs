use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

/// 认证方法，按列表顺序尝试，第一个成功即停止。
#[derive(Clone)]
#[non_exhaustive]
pub enum AuthMethod {
    Agent,
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
    Password(String),
    /// 轮到时通过 [`Prompter`] 向用户询问密码。
    AskPassword,
    /// keyboard-interactive（PAM、OTP 等），服务器的每轮提问都经 [`Prompter`] 转给用户。
    KeyboardInteractive,
}

/// keyboard-interactive 的一轮提问。服务器可以连续发多轮，也可以发不含问题的空轮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveRequest {
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<InteractivePrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractivePrompt {
    pub text: String,
    /// 为假时输入需遮蔽（密码、验证码）。
    pub echo: bool,
}

/// 认证过程中需要用户输入时的提问方，由 UI 实现。返回 `None` 表示用户取消，认证随即中止。
/// 密码与口令错误时会以相同参数再次调用（最多三次），实现方可据此提示“请重试”。
pub trait Prompter: Send + Sync + 'static {
    fn password(&self, user: &str, host: &str) -> BoxFuture<Option<String>>;
    /// 私钥已加密且未提供口令时询问。
    fn passphrase(&self, path: &Path) -> BoxFuture<Option<String>>;
    /// 返回的回答须与 `request.prompts` 一一对应。
    fn interactive(
        &self,
        user: &str,
        host: &str,
        request: InteractiveRequest,
    ) -> BoxFuture<Option<Vec<String>>>;
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone)]
pub struct ConnectParams {
    pub endpoint: Endpoint,
    pub user: String,
    pub auth: Vec<AuthMethod>,
    pub keepalive: Option<Duration>,
    pub connect_timeout: Duration,
    /// 未设置时需要询问的方法（密码、keyboard-interactive、加密私钥）都会被跳过。
    pub prompter: Option<Arc<dyn Prompter>>,
    /// ssh-agent 的套接字（对应 OpenSSH 的 `IdentityAgent`）；未设置时用系统默认的 agent。
    pub agent_socket: Option<PathBuf>,
}

impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不输出认证方法：其中可能含密码
        f.debug_struct("ConnectParams")
            .field("endpoint", &self.endpoint)
            .field("user", &self.user)
            .field("keepalive", &self.keepalive)
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

impl ConnectParams {
    pub fn new(endpoint: Endpoint, user: impl Into<String>, auth: Vec<AuthMethod>) -> Self {
        Self {
            endpoint,
            user: user.into(),
            auth,
            keepalive: Some(Duration::from_secs(30)),
            connect_timeout: Duration::from_secs(15),
            prompter: None,
            agent_socket: None,
        }
    }
}
