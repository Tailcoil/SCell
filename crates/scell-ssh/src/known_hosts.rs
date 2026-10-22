use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::{PublicKey, check_known_hosts_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    Known,
    /// known_hosts 中没有该主机的记录。
    New,
    /// 记录中的同算法密钥与服务器出示的不一致，可能遭遇中间人攻击。
    Changed,
}

/// 主机密钥的最终裁决方。UI 层实现它，以便在会话内提示用户确认。
/// 返回装箱的 Future 以保持对象安全，连接内部以 `Arc<dyn HostKeyVerifier>` 持有。
pub trait HostKeyVerifier: Send + Sync + 'static {
    fn verify(
        &self,
        host: String,
        port: u16,
        key: PublicKey,
        status: HostKeyStatus,
    ) -> Pin<Box<dyn Future<Output = bool> + Send>>;
}

/// 与 OpenSSH 相同的 `SHA256:...` 指纹。
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
}

/// OpenSSH 格式的 known_hosts 文件。
#[derive(Debug, Clone)]
pub struct KnownHostsFile {
    pub path: PathBuf,
}

impl KnownHostsFile {
    /// 文件不存在时视为没有记录。文件读不了或该主机的记录解析失败时返回错误：
    /// 此时无法判断密钥是否变更，按新主机让用户确认可能把中间人伪装成首次连接。
    pub fn status(&self, host: &str, port: u16, key: &PublicKey) -> Result<HostKeyStatus, String> {
        match check_known_hosts_path(host, port, key, &self.path) {
            Ok(true) => Ok(HostKeyStatus::Known),
            Ok(false) => Ok(HostKeyStatus::New),
            Err(russh::keys::Error::KeyChanged { .. }) => Ok(HostKeyStatus::Changed),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    pub fn learn(&self, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        learn_known_hosts_path(host, port, key, &self.path).map_err(std::io::Error::other)
    }
}
