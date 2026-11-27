//! 主机配置到 SSH 连接参数的翻译：登录用户、认证方法顺序与提问回调。

use std::sync::Arc;

use scell_config::hosts::{AuthPref, HostProfile};
use scell_ssh::{AuthMethod, ConnectParams, Endpoint};

use super::auth::HopPrompter;

pub(super) fn resolved_user(host: &HostProfile) -> String {
    host.user
        .clone()
        .or_else(scell_config::local_user)
        .unwrap_or_default()
}

pub(super) fn connect_params(host: &HostProfile, prompter: HopPrompter) -> ConnectParams {
    // 取不到本机用户名时留空，由 connect_hop 在连接前报错，不拿猜测的用户名去试（会计入 fail2ban）
    let user = resolved_user(host);
    let auth = if host.auth.is_empty() {
        default_auth()
    } else {
        host.auth
            .iter()
            .map(|a| match a {
                AuthPref::Agent => AuthMethod::Agent,
                AuthPref::Key { path } => AuthMethod::KeyFile {
                    path: scell_config::expand_tilde(path),
                    passphrase: None,
                },
                AuthPref::Password => AuthMethod::AskPassword,
                AuthPref::KeyboardInteractive => AuthMethod::KeyboardInteractive,
            })
            .collect()
    };
    let mut params = ConnectParams::new(Endpoint::new(host.host.clone(), host.port), user, auth);
    params.prompter = Some(Arc::new(prompter));
    params
}

/// 与 OpenSSH 的默认顺序相同：agent、默认私钥、keyboard-interactive、密码。
/// 服务器不接受的方法会被跳过，所以多数服务器上只会问一次。
fn default_auth() -> Vec<AuthMethod> {
    let mut auth = vec![AuthMethod::Agent];
    if let Some(ssh) = std::env::home_dir().map(|h| h.join(".ssh")) {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            let path = ssh.join(name);
            if path.exists() {
                auth.push(AuthMethod::KeyFile {
                    path,
                    passphrase: None,
                });
            }
        }
    }
    auth.extend([AuthMethod::KeyboardInteractive, AuthMethod::AskPassword]);
    auth
}
