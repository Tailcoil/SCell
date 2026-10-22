//! 认证与切换用户时的机密索取：先取连接缓存与系统凭据库，错误时清除并询问用户。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rust_i18n::t;
use scell_config::HostId;
use scell_config::hosts::SwitchMethod;
use scell_config::secrets::{self, SecretKey};
use scell_ssh::{BoxFuture, InteractiveRequest, Prompter};

use super::Interaction;
use crate::prompt::{PromptAnswer, PromptKind};
use crate::pump::notice;
use crate::session::Shared;

/// 连接上缓存的机密，见 [`crate::pool::Link::secrets`]。
pub(crate) type SecretCache = Arc<Mutex<Vec<(Secret, String)>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Secret {
    /// SSH 登录密码。
    Password(HostId),
    Passphrase(PathBuf),
    /// 主机上某个用户的密码，供 su（目标用户）和 sudo（登录用户）使用。
    UserPassword(HostId, String),
}

impl Secret {
    fn key(&self) -> SecretKey<'_> {
        match self {
            Self::Password(id) => SecretKey::Password(id),
            Self::Passphrase(path) => SecretKey::Passphrase(path),
            Self::UserPassword(id, user) => SecretKey::UserPassword(id, user),
        }
    }
}

/// 一跳认证或切换用户的提问方：优先使用连接缓存和凭据库中保存的密码与口令，
/// 错误时删除并改为询问；认证成功后写入连接缓存，勾选“记住”的同时写入系统凭据库。
#[derive(Clone)]
pub(super) struct HopPrompter(pub(super) Arc<Hop>);

pub(super) struct Hop {
    ui: Arc<Interaction>,
    host: HostId,
    cache: SecretCache,
    state: Mutex<HopState>,
}

#[derive(Default)]
struct HopState {
    /// 已索取过的机密（区分首次与重试）。
    asked: Vec<Secret>,
    /// 上一次取自连接缓存或凭据库的机密（再次索取说明有误，需清除）。
    from_store: Vec<Secret>,
    /// 本次用户输入的值，认证通过后写入连接缓存。
    verified: Vec<(Secret, String)>,
    /// 用户勾选了“记住”的机密，认证通过后写入系统凭据库。
    pending: Vec<(Secret, String)>,
}

/// 凭据库调用会阻塞，放到阻塞线程池。
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

impl Hop {
    pub(super) fn new(ui: Arc<Interaction>, host: HostId, cache: SecretCache) -> Arc<Self> {
        Arc::new(Self {
            ui,
            host,
            cache,
            state: Mutex::new(HopState::default()),
        })
    }

    pub(super) fn has_asked(&self) -> bool {
        !self.state.lock().asked.is_empty()
    }

    fn cached(&self, secret: &Secret) -> Option<String> {
        self.cache
            .lock()
            .iter()
            .find(|(s, _)| s == secret)
            .map(|(_, v)| v.clone())
    }

    /// 先取连接缓存与凭据库（`secret` 没有时再试只读的 `fallback`）；取过且被用过一次（说明不正确）
    /// 时只清除 `secret` 并询问用户，`fallback` 不受影响。
    async fn obtain(
        self: Arc<Self>,
        secret: Secret,
        fallback: Option<Secret>,
        make_prompt: impl FnOnce(bool) -> PromptKind,
    ) -> Option<String> {
        let (first, stale) = {
            let mut state = self.state.lock();
            let first = !state.asked.contains(&secret);
            if first {
                state.asked.push(secret.clone());
            }
            let before = state.from_store.len();
            state.from_store.retain(|s| *s != secret);
            (first, state.from_store.len() != before)
        };
        if stale {
            self.cache.lock().retain(|(s, _)| *s != secret);
            let s = secret.clone();
            blocking(move || secrets::delete(s.key())).await;
        }
        if first {
            let candidates: Vec<Secret> = std::iter::once(secret.clone()).chain(fallback).collect();
            let mut found = candidates.iter().find_map(|s| self.cached(s));
            for s in &candidates {
                if found.is_some() {
                    break;
                }
                let s = s.clone();
                found = blocking(move || secrets::get(s.key())).await.flatten();
            }
            if let Some(value) = found {
                self.state.lock().from_store.push(secret);
                return Some(value);
            }
        }
        match self.ui.ask(make_prompt(!first)).await {
            PromptAnswer::Secret { value, remember } => {
                let mut state = self.state.lock();
                state.verified.retain(|(s, _)| *s != secret);
                state.verified.push((secret.clone(), value.clone()));
                state.pending.retain(|(s, _)| *s != secret);
                if remember {
                    state.pending.push((secret, value.clone()));
                }
                Some(value)
            }
            _ => None,
        }
    }

    /// `su` 用目标用户的密码；`sudo` 用登录用户的密码，没存过时先试 SSH 登录密码。
    pub(super) async fn switch_password(
        self: Arc<Self>,
        method: SwitchMethod,
        target_user: &str,
        login_user: &str,
        host_label: &str,
    ) -> Option<String> {
        let (prompt_user, fallback) = match method {
            SwitchMethod::Su => (target_user, None),
            SwitchMethod::Sudo => (login_user, Some(Secret::Password(self.host.clone()))),
        };
        let secret = Secret::UserPassword(self.host.clone(), prompt_user.to_owned());
        let user = prompt_user.to_owned();
        let host = host_label.to_owned();
        self.obtain(secret, fallback, move |retry| PromptKind::Password {
            user,
            host,
            retry,
        })
        .await
    }

    /// 认证成功：把本次验证通过的密码存入连接缓存，勾选了“记住”的写入系统凭据库。
    /// 私钥口令只在建立连接时用到，不进连接缓存。
    pub(super) async fn commit(&self, shared: &Arc<Shared>) {
        let (verified, pending) = {
            let mut state = self.state.lock();
            (
                std::mem::take(&mut state.verified),
                std::mem::take(&mut state.pending),
            )
        };
        {
            let mut cache = self.cache.lock();
            for (secret, value) in verified {
                if matches!(secret, Secret::Passphrase(_)) {
                    continue;
                }
                cache.retain(|(s, _)| *s != secret);
                cache.push((secret, value));
            }
        }
        for (secret, value) in pending {
            let result = blocking(move || secrets::set(secret.key(), &value)).await;
            if let Some(Err(e)) = result {
                notice(
                    shared,
                    &format!(
                        "\r\n\x1b[33m{}\x1b[0m\r\n",
                        t!("session.keyring_failed", err = e)
                    ),
                );
            }
        }
    }
}

impl Prompter for HopPrompter {
    fn password(&self, user: &str, host: &str) -> BoxFuture<Option<String>> {
        let user = user.to_owned();
        let host = host.to_owned();
        let secret = Secret::Password(self.0.host.clone());
        Box::pin(
            self.0
                .clone()
                .obtain(secret, None, move |retry| PromptKind::Password {
                    user,
                    host,
                    retry,
                }),
        )
    }

    fn passphrase(&self, path: &Path) -> BoxFuture<Option<String>> {
        let path = path.to_owned();
        let secret = Secret::Passphrase(path.clone());
        Box::pin(
            self.0
                .clone()
                .obtain(secret, None, move |retry| PromptKind::Passphrase {
                    path,
                    retry,
                }),
        )
    }

    fn interactive(
        &self,
        user: &str,
        host: &str,
        request: InteractiveRequest,
    ) -> BoxFuture<Option<Vec<String>>> {
        let ui = self.0.ui.clone();
        let count = request.prompts.len();
        let kind = PromptKind::Interactive {
            user: user.to_owned(),
            host: host.to_owned(),
            name: request.name,
            instructions: request.instructions,
            prompts: request.prompts,
        };
        Box::pin(async move {
            match ui.ask(kind).await {
                PromptAnswer::Responses(r) if r.len() == count => Some(r),
                _ => None,
            }
        })
    }
}
