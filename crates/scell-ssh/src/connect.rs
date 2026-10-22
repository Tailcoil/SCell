use std::sync::Arc;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use russh::{ChannelStream, client::Msg};
use tokio::io::{AsyncRead, AsyncWrite};

use russh::client::{AuthResult, KeyboardInteractiveAuthResponse};
use russh::{MethodKind, MethodSet};

use crate::channel::ShellChannel;
use crate::forward::{self, Forward, RemoteRoutes};
use crate::{
    AuthMethod, ConnectParams, HostKeyVerifier, InteractivePrompt, InteractiveRequest,
    KnownHostsFile, Prompter, SshError,
};

const PASSWORD_PROMPTS: usize = 3;

/// 询问口令解密私钥，口令错误时重问。
async fn load_encrypted_key(
    path: &std::path::Path,
    prompter: &dyn Prompter,
) -> Result<russh::keys::PrivateKey, SshError> {
    let mut last = None;
    for _ in 0..PASSWORD_PROMPTS {
        let passphrase = prompter.passphrase(path).await.ok_or(SshError::Cancelled)?;
        match load_secret_key(path, Some(&passphrase)) {
            Ok(key) => return Ok(key),
            // 文件读不到不是口令的问题，不必重问
            Err(e @ russh::keys::Error::IO(_)) => return Err(e.into()),
            // 口令错误时不同格式报的错误不同，统一按口令错误重问
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or(SshError::AuthFailed, Into::into))
}

/// 一次认证尝试的结果。失败时带上服务器表示仍可继续的方法（`None` 表示未知）。
enum Outcome {
    Success,
    Failure(Option<MethodSet>),
}

impl From<AuthResult> for Outcome {
    fn from(result: AuthResult) -> Self {
        match result {
            AuthResult::Success => Self::Success,
            AuthResult::Failure {
                remaining_methods, ..
            } => Self::Failure(Some(remaining_methods)),
        }
    }
}

/// 已认证的 SSH 连接。同一连接可开多个通道（终端、SFTP、探针、转发、下一跳）。
pub struct Connection {
    /// 转发任务持有克隆；认证在克隆之前完成，见 [`Self::handle_mut`]。
    handle: Arc<Handle<ClientHandler>>,
    remote_routes: RemoteRoutes,
}

pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: KnownHostsFile,
    verifier: Arc<dyn HostKeyVerifier>,
    remote_routes: RemoteRoutes,
}

impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 证书校验尚未实现，按其携带的公钥对待
        let key = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            russh::keys::PublicKeyOrCertificate::Certificate(cert) => {
                russh::keys::PublicKey::from(cert.public_key().clone())
            }
        };
        let status = self
            .known_hosts
            .status(&self.host, self.port, &key)
            .map_err(SshError::KnownHosts)?;
        Ok(self
            .verifier
            .verify(self.host.clone(), self.port, key, status)
            .await)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        forward::accept_remote(&self.remote_routes, channel, connected_port, reply);
        Ok(())
    }
}

impl Connection {
    /// 直连 TCP 并完成认证。
    pub async fn connect(
        params: &ConnectParams,
        known_hosts: KnownHostsFile,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Self, SshError> {
        let addr = (params.endpoint.host.as_str(), params.endpoint.port);
        let tcp =
            tokio::time::timeout(params.connect_timeout, tokio::net::TcpStream::connect(addr))
                .await
                .map_err(|_| SshError::Connect(std::io::ErrorKind::TimedOut.into()))?
                .map_err(SshError::Connect)?;
        let _ = tcp.set_nodelay(true);
        Self::establish(params, tcp, known_hosts, verifier).await
    }

    /// 经由本连接（跳板机）的 direct-tcpip 通道连接下一跳。
    pub async fn connect_via(
        &self,
        params: &ConnectParams,
        known_hosts: KnownHostsFile,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Self, SshError> {
        let channel = self
            .handle
            .channel_open_direct_tcpip(
                params.endpoint.host.clone(),
                params.endpoint.port as u32,
                "127.0.0.1",
                0,
            )
            .await?;
        Self::establish(params, channel.into_stream(), known_hosts, verifier).await
    }

    async fn establish<S>(
        params: &ConnectParams,
        stream: S,
        known_hosts: KnownHostsFile,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Self, SshError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let config = Arc::new(client::Config {
            keepalive_interval: params.keepalive,
            nodelay: true,
            ..Default::default()
        });
        let remote_routes = RemoteRoutes::default();
        let handler = ClientHandler {
            host: params.endpoint.host.clone(),
            port: params.endpoint.port,
            known_hosts,
            verifier,
            remote_routes: remote_routes.clone(),
        };
        let handle = client::connect_stream(config, stream, handler).await?;
        let mut conn = Self {
            handle: Arc::new(handle),
            remote_routes,
        };
        conn.authenticate(params).await?;
        Ok(conn)
    }

    /// 按序尝试认证方法。服务器每次失败都会告知还接受哪些方法，不在其中的直接跳过，
    /// 避免无谓的失败尝试计入 MaxAuthTries 或 fail2ban。
    async fn authenticate(&mut self, params: &ConnectParams) -> Result<(), SshError> {
        let user = params.user.as_str();
        let host = params.endpoint.host.as_str();
        // 先以 none 探询服务器接受的方法（OpenSSH 客户端也这样做，服务器不记为失败）
        let mut allowed: Option<MethodSet> = match self.handle_mut().authenticate_none(user).await?
        {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure {
                remaining_methods, ..
            } => Some(remaining_methods).filter(|m| !m.is_empty()),
        };
        // agent 不可用、私钥读不出时与 OpenSSH 一样跳过，继续后续方法；
        // 若一次都没真正尝试过，报告这个更具体的原因
        let mut skipped = None;
        let mut attempted = false;
        for method in &params.auth {
            let kind = match method {
                AuthMethod::Agent | AuthMethod::KeyFile { .. } => MethodKind::PublicKey,
                AuthMethod::Password(_) | AuthMethod::AskPassword => MethodKind::Password,
                AuthMethod::KeyboardInteractive => MethodKind::KeyboardInteractive,
            };
            if allowed.as_ref().is_some_and(|a| !a.contains(&kind)) {
                continue;
            }
            let outcome = match method {
                AuthMethod::Password(password) => self
                    .handle_mut()
                    .authenticate_password(user, password.as_str())
                    .await?
                    .into(),
                AuthMethod::AskPassword => {
                    let Some(prompter) = &params.prompter else {
                        continue;
                    };
                    self.authenticate_asked_password(user, host, prompter.as_ref())
                        .await?
                }
                AuthMethod::KeyFile { path, passphrase } => {
                    let loaded = match load_secret_key(path, passphrase.as_deref()) {
                        Err(russh::keys::Error::KeyIsEncrypted) if passphrase.is_none() => {
                            let Some(prompter) = &params.prompter else {
                                continue;
                            };
                            load_encrypted_key(path, prompter.as_ref()).await
                        }
                        other => other.map_err(SshError::from),
                    };
                    let key = match loaded {
                        Ok(key) => key,
                        Err(SshError::Cancelled) => return Err(SshError::Cancelled),
                        Err(e) => {
                            tracing::warn!("跳过私钥 {}：{e}", path.display());
                            skipped = Some(e);
                            continue;
                        }
                    };
                    let hash = self.handle.best_supported_rsa_hash().await?.flatten();
                    self.handle_mut()
                        .authenticate_publickey(
                            user,
                            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                        )
                        .await?
                        .into()
                }
                AuthMethod::Agent => match self
                    .authenticate_agent(user, params.agent_socket.as_deref())
                    .await
                {
                    Err(SshError::Agent(e)) => {
                        tracing::debug!("ssh-agent 不可用：{e}");
                        skipped = Some(SshError::Agent(e));
                        continue;
                    }
                    other => other?,
                },
                AuthMethod::KeyboardInteractive => {
                    let Some(prompter) = &params.prompter else {
                        continue;
                    };
                    self.authenticate_interactive(user, host, prompter.as_ref())
                        .await?
                }
            };
            attempted = true;
            match outcome {
                Outcome::Success => return Ok(()),
                Outcome::Failure(Some(remaining)) => allowed = Some(remaining),
                Outcome::Failure(None) => {}
            }
        }
        Err(match skipped {
            Some(e) if !attempted => e,
            _ => SshError::AuthFailed,
        })
    }

    /// 询问密码并尝试，错误时重问，与 OpenSSH 的 NumberOfPasswordPrompts 默认值相同。
    async fn authenticate_asked_password(
        &mut self,
        user: &str,
        host: &str,
        prompter: &dyn Prompter,
    ) -> Result<Outcome, SshError> {
        let mut outcome = Outcome::Failure(None);
        for _ in 0..PASSWORD_PROMPTS {
            let password = prompter
                .password(user, host)
                .await
                .ok_or(SshError::Cancelled)?;
            outcome = self
                .handle_mut()
                .authenticate_password(user, password)
                .await?
                .into();
            match &outcome {
                Outcome::Failure(Some(remaining)) if remaining.contains(&MethodKind::Password) => {}
                _ => break,
            }
        }
        Ok(outcome)
    }

    async fn authenticate_agent(
        &mut self,
        user: &str,
        socket: Option<&std::path::Path>,
    ) -> Result<Outcome, SshError> {
        let mut agent = crate::agent::connect(socket).await?;
        let identities = agent
            .request_identities()
            .await
            .map_err(|e| SshError::Agent(e.to_string()))?;
        let hash = self.handle.best_supported_rsa_hash().await?.flatten();
        let mut outcome = Outcome::Failure(None);
        for identity in identities {
            outcome = self
                .handle_mut()
                .authenticate_publickey_with(
                    user,
                    identity.public_key().into_owned(),
                    hash,
                    &mut agent,
                )
                .await
                .map_err(|e| SshError::Agent(e.to_string()))?
                .into();
            match &outcome {
                Outcome::Success => break,
                Outcome::Failure(Some(remaining))
                    if !remaining.contains(&MethodKind::PublicKey) =>
                {
                    break;
                }
                Outcome::Failure(_) => {}
            }
        }
        Ok(outcome)
    }

    async fn authenticate_interactive(
        &mut self,
        user: &str,
        host: &str,
        prompter: &dyn Prompter,
    ) -> Result<Outcome, SshError> {
        let mut reply = self
            .handle_mut()
            .authenticate_keyboard_interactive_start(user, None)
            .await?;
        loop {
            match reply {
                KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Success),
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods, ..
                } => return Ok(Outcome::Failure(Some(remaining_methods))),
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    // 不含问题的轮次（常见于 PAM 的提示信息）直接空答
                    let responses = if prompts.is_empty() {
                        Vec::new()
                    } else {
                        let request = InteractiveRequest {
                            name,
                            instructions,
                            prompts: prompts
                                .into_iter()
                                .map(|p| InteractivePrompt {
                                    text: p.prompt,
                                    echo: p.echo,
                                })
                                .collect(),
                        };
                        prompter
                            .interactive(user, host, request)
                            .await
                            .ok_or(SshError::Cancelled)?
                    };
                    reply = self
                        .handle_mut()
                        .authenticate_keyboard_interactive_respond(responses)
                        .await?;
                }
            }
        }
    }

    /// 打开带 PTY 的交互式 shell 通道。
    pub async fn open_shell(
        &self,
        term: &str,
        cols: u16,
        rows: u16,
    ) -> Result<ShellChannel, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel
            .request_pty(true, term, cols as u32, rows as u32, 0, 0, &[])
            .await?;
        channel.request_shell(true).await?;
        Ok(ShellChannel::new(channel))
    }

    /// 不分配终端地执行命令。输出与退出码经 [`ShellChannel`] 读取（stderr 并入输出）；
    /// 关闭通道后服务器端进程的标准输出断开，下次写出时收到 SIGPIPE 退出。
    pub async fn open_exec(&self, command: &str) -> Result<ShellChannel, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        Ok(ShellChannel::new(channel))
    }

    /// 在 PTY 中执行命令，用于只从终端读密码的程序（如 su）。输出经 [`ShellChannel`] 读取。
    pub async fn open_exec_pty(&self, command: &str) -> Result<ShellChannel, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.request_pty(true, "dumb", 80, 24, 0, 0, &[]).await?;
        channel.exec(true, command).await?;
        Ok(ShellChannel::new(channel))
    }

    /// 打开子系统通道（如 `sftp`），返回字节流供上层协议使用。
    pub async fn open_subsystem(&self, name: &str) -> Result<ChannelStream<Msg>, SshError> {
        let channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, name).await?;
        Ok(channel.into_stream())
    }

    /// 认证需要独占的句柄，只在连接建立期间（尚未被转发共享）调用。
    fn handle_mut(&mut self) -> &mut Handle<ClientHandler> {
        Arc::get_mut(&mut self.handle).expect("认证在连接被共享之前完成")
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// 本地转发（`-L`）：在本机 `bind` 监听，每个连接经本连接转到远端可达的 `target`。
    pub async fn forward_local(
        &self,
        bind: (&str, u16),
        target: (String, u16),
    ) -> Result<Forward, SshError> {
        forward::local(self.handle.clone(), bind, target).await
    }

    /// 远程转发（`-R`）：请求服务器在 `bind` 监听，每个连接转回本机可达的 `target`。
    /// 服务器的 `GatewayPorts` 决定能否监听非回环地址。
    pub async fn forward_remote(
        &self,
        bind: (&str, u16),
        target: (String, u16),
    ) -> Result<Forward, SshError> {
        forward::remote(
            self.handle.clone(),
            self.remote_routes.clone(),
            bind,
            target,
        )
        .await
    }

    /// 动态转发（`-D`）：在本机 `bind` 提供 SOCKS5 代理，目标由客户端指定、经本连接访问。
    pub async fn forward_dynamic(&self, bind: (&str, u16)) -> Result<Forward, SshError> {
        forward::dynamic(self.handle.clone(), bind).await
    }
}
