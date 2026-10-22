use rust_i18n::t;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SshError {
    #[error("{}", t!("error.connect", err = .0))]
    Connect(#[source] std::io::Error),
    #[error("{}", t!("error.protocol", err = .0))]
    Protocol(#[from] russh::Error),
    #[error("{}", t!("error.host_key_rejected"))]
    HostKeyRejected,
    #[error("{}", t!("error.known_hosts", err = .0))]
    KnownHosts(String),
    #[error("{}", t!("error.auth_failed"))]
    AuthFailed,
    #[error("{}", t!("error.key", err = .0))]
    Key(#[from] russh::keys::Error),
    #[error("{}", t!("error.agent", err = .0))]
    Agent(String),
    #[error("{}", t!("error.cancelled"))]
    Cancelled,
}

impl SshError {
    /// 是否为网络层面的失败（连不上、超时、连接被断开），值得稍后重试；
    /// 认证、主机密钥、算法协商等失败重试也不会成功。
    pub fn is_transient(&self) -> bool {
        use russh::Error as E;
        match self {
            Self::Connect(_) => true,
            Self::Protocol(e) => matches!(
                e,
                E::IO(_)
                    | E::Disconnect
                    | E::HUP
                    | E::ConnectionTimeout
                    | E::KeepaliveTimeout
                    | E::InactivityTimeout
                    | E::Elapsed(_)
                    | E::SendError
                    | E::RecvError
                    // 经跳板连接时目标主机不可达
                    | E::ChannelOpenFailure(_)
            ),
            _ => false,
        }
    }
}
