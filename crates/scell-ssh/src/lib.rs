//! SSH 客户端：连接、认证、主机密钥校验、跳板、通道与端口转发。
//!
//! 不依赖配置与 UI：调用方传入 [`ConnectParams`]，需要用户裁决的主机密钥问题
//! 通过 [`HostKeyVerifier`] 交给上层。

rust_i18n::i18n!("locales", fallback = "en");

mod agent;
mod channel;
mod connect;
mod error;
mod forward;
mod known_hosts;
mod liveness;
mod params;

pub use channel::{ShellChannel, ShellEvent};
pub use connect::Connection;
pub use error::SshError;
pub use forward::{Forward, ForwardStats};
pub use known_hosts::{HostKeyStatus, HostKeyVerifier, KnownHostsFile, fingerprint};
pub use liveness::Guarded;
pub use params::{
    AuthMethod, BoxFuture, ConnectParams, Endpoint, InteractivePrompt, InteractiveRequest, Prompter,
};
pub use russh::keys::PublicKey;
