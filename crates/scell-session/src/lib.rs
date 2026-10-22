//! 会话编排：创建会话、驱动字节流进入终端状态机、向 UI 上报事件。
//!
//! 对 UI 暴露两样东西：可克隆的 [`SessionService`] 句柄，以及 [`SessionEvent`] 事件流。
//! 会话归服务所有，与窗口和视图解耦，视图销毁不影响连接。

rust_i18n::i18n!("locales", fallback = "en");

mod codec;
mod cwd;
mod event;
mod files;
mod forwards;
mod pool;
mod profiles;
mod prompt;
mod pump;
mod service;
mod session;
mod telemetry;
mod transport;

pub use codec::Encoding;
pub use cwd::{local_cwd, osc7_path};
pub use event::SessionEvent;
pub use forwards::{ForwardId, ForwardStatus};
pub use profiles::{ShellProfile, detect_shells};
pub use prompt::{InteractivePrompt, Prompt, PromptAnswer, PromptKind};
pub use service::{SessionError, SessionService};
pub use session::{SessionHandle, SessionId};
pub use telemetry::{DiskUsage, Metrics, MetricsUpdate, MetricsWatch, ProcessUsage, Usage};
pub use transport::{LocalShell, SshTarget, hide_env_from_shells};
