//! 认证流程：对进程内的 russh 服务端验证 keyboard-interactive 与按服务器方法跳过。

#![allow(clippy::unwrap_used)]

use std::borrow::Cow;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::server::{self, Auth, Response};
use russh::{MethodKind, MethodSet};
use scell_ssh::{
    AuthMethod, BoxFuture, ConnectParams, Connection, Endpoint, HostKeyStatus, HostKeyVerifier,
    InteractiveRequest, KnownHostsFile, Prompter, PublicKey, SshError,
};

/// 只接受 keyboard-interactive：先问密码，再问验证码。
struct Server {
    round: usize,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, password: &str) -> Result<Auth, Self::Error> {
        // 与 OpenSSH 的 sshd 相同，密码错误后仍接受密码（russh 默认会去掉）
        Ok(if password == "right" {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                partial_success: false,
            }
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _: &str,
        _: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let answers: Vec<String> = response
            .into_iter()
            .flatten()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect();
        self.round += 1;
        Ok(match (self.round, answers.as_slice()) {
            (1, []) => Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Password: "), false)]),
            },
            (2, [pw]) if pw == "secret" => Auth::Partial {
                name: Cow::Borrowed("2FA"),
                instructions: Cow::Borrowed("Enter the code"),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Code: "), true)]),
            },
            (3, [code]) if code == "123456" => Auth::Accept,
            _ => Auth::reject(),
        })
    }
}

async fn serve(methods: &[MethodKind]) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]))],
        methods: MethodSet::from(methods),
        auth_rejection_time: std::time::Duration::ZERO,
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let session = server::run_stream(config, socket, Server { round: 0 })
            .await
            .unwrap();
        let _ = session.await;
    });
    port
}

struct TrustAll;

impl HostKeyVerifier for TrustAll {
    fn verify(
        &self,
        _: String,
        _: u16,
        _: PublicKey,
        _: HostKeyStatus,
    ) -> Pin<Box<dyn Future<Output = bool> + Send>> {
        Box::pin(async { true })
    }
}

/// 记录收到的提问，按问题文字作答；`cancel` 时拒绝回答。
#[derive(Default)]
struct Answers {
    /// 依次给出的密码；用完后取消。
    password_answers: Mutex<Vec<&'static str>>,
    passwords: Mutex<usize>,
    requests: Mutex<Vec<InteractiveRequest>>,
    cancel: bool,
}

impl Prompter for Answers {
    fn password(&self, _: &str, _: &str) -> BoxFuture<Option<String>> {
        *self.passwords.lock().unwrap() += 1;
        let mut answers = self.password_answers.lock().unwrap();
        let answer = (!answers.is_empty()).then(|| answers.remove(0).to_owned());
        Box::pin(async move { answer })
    }

    fn passphrase(&self, _: &Path) -> BoxFuture<Option<String>> {
        Box::pin(async { None })
    }

    fn interactive(
        &self,
        _: &str,
        _: &str,
        request: InteractiveRequest,
    ) -> BoxFuture<Option<Vec<String>>> {
        let answers = request
            .prompts
            .iter()
            .map(|p| match p.text.as_str() {
                "Password: " => "secret".to_owned(),
                _ => "123456".to_owned(),
            })
            .collect();
        self.requests.lock().unwrap().push(request);
        let cancel = self.cancel;
        Box::pin(async move { (!cancel).then_some(answers) })
    }
}

async fn connect(answers: Arc<Answers>, methods: &[MethodKind]) -> Result<Connection, SshError> {
    let port = serve(methods).await;
    let mut params = ConnectParams::new(
        Endpoint::new("127.0.0.1", port),
        "tester",
        vec![
            AuthMethod::Agent,
            AuthMethod::AskPassword,
            AuthMethod::KeyboardInteractive,
        ],
    );
    params.prompter = Some(answers);
    let known_hosts = KnownHostsFile {
        path: std::env::temp_dir().join("scell-auth-test-known-hosts"),
    };
    Connection::connect(&params, known_hosts, Arc::new(TrustAll)).await
}

#[tokio::test]
async fn keyboard_interactive_multi_round_skips_unoffered_methods() {
    let answers = Arc::new(Answers::default());
    connect(answers.clone(), &[MethodKind::KeyboardInteractive])
        .await
        .unwrap();

    // 服务器只提供 keyboard-interactive，agent 与密码都不该被尝试
    assert_eq!(*answers.passwords.lock().unwrap(), 0);
    let requests = answers.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].prompts[0].text, "Password: ");
    assert!(!requests[0].prompts[0].echo);
    assert_eq!(requests[1].name, "2FA");
    assert!(requests[1].prompts[0].echo);
}

#[tokio::test]
async fn cancelling_interactive_aborts() {
    let answers = Arc::new(Answers {
        cancel: true,
        ..Default::default()
    });
    assert!(matches!(
        connect(answers, &[MethodKind::KeyboardInteractive]).await,
        Err(SshError::Cancelled)
    ));
}

#[tokio::test]
async fn wrong_password_is_asked_again() {
    let answers = Arc::new(Answers {
        password_answers: Mutex::new(vec!["wrong", "right"]),
        ..Default::default()
    });
    connect(answers.clone(), &[MethodKind::Password])
        .await
        .unwrap();
    assert_eq!(*answers.passwords.lock().unwrap(), 2);
    assert!(answers.requests.lock().unwrap().is_empty());
}
