//! 机密存取。密码与私钥口令只存 OS 凭据库：Linux 为 Secret Service（GNOME Keyring、KWallet 等），
//! Windows 为凭据管理器。配置文件里从不出现机密。
//!
//! 所有调用都会阻塞（D-Bus / Win32），且凭据库被锁时可能弹出系统的解锁对话框，须在后台线程调用。
//! 凭据库不可用（没有 Secret Service 守护进程等）时读取一律视为“未保存”，写入返回错误。

use std::path::Path;
use std::sync::{Arc, OnceLock};

use keyring_core::{CredentialStore, Entry, Error};

use crate::HostId;

const SERVICE: &str = "SCell";

/// 一条机密在凭据库中的位置。
#[derive(Debug, Clone, Copy)]
pub enum SecretKey<'a> {
    /// 某台主机的登录密码。
    Password(&'a HostId),
    /// 某个加密私钥的口令，按文件路径区分，可被多台主机共用。
    Passphrase(&'a Path),
    /// 主机上某个用户的密码：su 存目标用户的，sudo 存登录用户的。与 [`Self::Password`] 分开存放，
    /// sudo 拒绝时清除这一条不会连带删掉 SSH 登录密码。
    UserPassword(&'a HostId, &'a str),
}

impl SecretKey<'_> {
    fn account(self) -> String {
        match self {
            Self::Password(id) => format!("password:{}", id.0),
            Self::Passphrase(path) => format!("passphrase:{}", path.display()),
            Self::UserPassword(id, user) => format!("user-password:{}:{user}", id.0),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(String);

static AVAILABLE: OnceLock<bool> = OnceLock::new();

/// 首次使用时连接平台凭据库；测试可先调用 `use_store` 换成内存实现。
fn ready() -> bool {
    *AVAILABLE.get_or_init(|| match platform_store() {
        Ok(store) => {
            keyring_core::set_default_store(store);
            true
        }
        Err(e) => {
            tracing::warn!("系统凭据库不可用：{e}");
            false
        }
    })
}

#[cfg(windows)]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(target_os = "macos")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(not(any(windows, unix)))]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(Error::NoDefaultStore)
}

/// 指定凭据库实现。只在首次使用前调用有效，用于测试。
#[cfg(test)]
fn use_store(store: Arc<CredentialStore>) {
    if AVAILABLE.set(true).is_ok() {
        keyring_core::set_default_store(store);
    }
}

fn entry(key: SecretKey) -> Option<Entry> {
    if !ready() {
        return None;
    }
    Entry::new(SERVICE, &key.account())
        .inspect_err(|e| tracing::warn!("无法访问凭据库：{e}"))
        .ok()
}

pub fn get(key: SecretKey) -> Option<String> {
    match entry(key)?.get_password() {
        Ok(secret) => Some(secret),
        Err(Error::NoEntry) => None,
        Err(e) => {
            tracing::warn!("读取凭据失败：{e}");
            None
        }
    }
}

pub fn set(key: SecretKey, secret: &str) -> Result<(), SecretError> {
    let entry =
        entry(key).ok_or_else(|| SecretError(rust_i18n::t!("error.no_keyring").into_owned()))?;
    entry
        .set_password(secret)
        .map_err(|e| SecretError(e.to_string()))
}

/// 删除机密；本来就没有时也算成功。
pub fn delete(key: SecretKey) {
    let Some(entry) = entry(key) else {
        return;
    };
    match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => {}
        Err(e) => tracing::warn!("删除凭据失败：{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_mock_store() {
        use_store(keyring_core::mock::Store::new().unwrap());
        let id = HostId("h1".into());
        let key = SecretKey::Password(&id);
        assert_eq!(get(key), None);
        set(key, "pw").unwrap();
        assert_eq!(get(key).as_deref(), Some("pw"));
        let path = Path::new("/k");
        assert_eq!(get(SecretKey::Passphrase(path)), None);
        delete(key);
        delete(key);
        assert_eq!(get(key), None);
    }
}
