//! SSH 传输：在 tokio 上按跳板链逐跳建立连接、打开 shell，把字节流接到 pump。
//! 连接进度与失败原因直接写进终端，用户在所属窗格内就能看到。
//!
//! 文件会话（[`SshMode::Files`]）走同一套连接、认证与重连流程，只是不开 shell：
//! 连上后保持连接，供 SFTP 使用，终端不显示。

use rust_i18n::t;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use scell_config::HostId;
use scell_config::hosts::{AuthPref, ForwardRule, HostProfile};
use scell_config::secrets::{self, SecretKey};
use scell_ssh::{
    AuthMethod, BoxFuture, ConnectParams, Connection, Endpoint, HostKeyStatus, HostKeyVerifier,
    InteractiveRequest, KnownHostsFile, Prompter, PublicKey, ShellEvent,
};
use scell_term::TermSize;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::{TransportControl, VfsReply};
use crate::codec::{Decoder, Encoder};
use crate::pool::{ConnectionPool, Link, hop_key};
use crate::prompt::{Prompt, PromptAnswer, PromptKind};
use crate::pump::{Pump, notice};
use crate::session::Shared;
use crate::{Encoding, SessionEvent};

/// 要连接的主机，以及解析好的跳板链（最外层在前）。
#[derive(Debug, Clone)]
pub struct SshTarget {
    pub host: HostProfile,
    pub jumps: Vec<HostProfile>,
    pub known_hosts: PathBuf,
}

const TERM: &str = "xterm-256color";

/// 连接意外断开时，把远端程序可能留下的模式（备用屏、鼠标上报、括号粘贴、应用光标键、
/// kitty 键盘协议、隐藏光标）复位，回滚内容才能正常查看。
const RESET_MODES: &str = "\x1b[?1049l\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[=0u\x1b[?25h\x1b[0m";

/// 连上后保持这么久才算稳定。连上即被断开（服务器拒绝开会话、shell 立即退出等）不清零失败计数，
/// 否则会一直以最短间隔重连。
const STABLE_AFTER: Duration = Duration::from_secs(5);

/// 自动重连前的等待：1、2、4… 秒，最长 30 秒。
fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(5)).min(30))
}

/// 会话连上后做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SshMode {
    /// 打开交互 shell。
    Shell,
    /// 只保持连接，供 SFTP 等使用。
    Files,
}

/// 文件会话检查连接是否断开的间隔。
const LINK_POLL: Duration = Duration::from_secs(2);

/// SSH 会话共用的服务级资源。
#[derive(Clone)]
pub(crate) struct SshEnv {
    pub(crate) pool: ConnectionPool,
    pub(crate) rt: Handle,
    /// 连接意外断开后是否自动重连，每次断开时读取。
    pub(crate) auto_reconnect: Arc<AtomicBool>,
}

struct SshControl {
    resize: mpsc::UnboundedSender<TermSize>,
    reconnect: mpsc::UnboundedSender<()>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    /// 当前 shell 所在的连接；未连上时为空。
    link: CurrentLink,
    rt: Handle,
}

type CurrentLink = Arc<Mutex<Option<Arc<Link>>>>;

impl TransportControl for SshControl {
    fn resize(&self, size: TermSize) -> std::io::Result<()> {
        // 连接建立前的尺寸变化在打开 shell 时一并生效
        let _ = self.resize.send(size);
        Ok(())
    }

    fn kill(&self) {
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
        // 句柄可能还被别处持有，不能靠它析构来释放连接
        self.link.lock().take();
    }

    fn reconnect(&self) {
        let _ = self.reconnect.send(());
    }

    fn watch_metrics(
        &self,
        tx: flume::Sender<crate::telemetry::MetricsUpdate>,
        stop: tokio::sync::oneshot::Receiver<()>,
    ) {
        let Some(link) = self.link.lock().clone() else {
            let _ = tx.send(crate::telemetry::MetricsUpdate::Failed(
                t!("session.not_connected").into(),
            ));
            return;
        };
        self.rt
            .spawn(async move { crate::telemetry::run_remote(&link.conn, tx, stop).await });
    }

    fn forwards(&self) -> Vec<crate::ForwardStatus> {
        self.link
            .lock()
            .as_ref()
            .map(|l| crate::forwards::list(l))
            .unwrap_or_default()
    }

    fn add_forward(&self, rule: ForwardRule) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let Some(link) = self.link.lock().clone() else {
            let _ = tx.send(Err(t!("session.not_connected").into()));
            return rx;
        };
        self.rt.spawn(async move {
            let result = crate::forwards::start(&link, rule, false).await.map(|_| ());
            let _ = tx.send(result);
        });
        rx
    }

    fn remove_forward(&self, id: crate::ForwardId) {
        if let Some(link) = self.link.lock().as_ref() {
            crate::forwards::remove(link, id);
        }
    }

    fn is_connected(&self) -> bool {
        self.link
            .lock()
            .as_ref()
            .is_some_and(|l| !l.conn.is_closed())
    }

    /// 在当前连接上开 sftp 子系统，无需再次认证。SFTP 会话持有该连接，终端断开或关闭不影响它。
    fn open_vfs(&self) -> VfsReply {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let Some(link) = self.link.lock().clone() else {
            let _ = tx.send(Err(t!("session.not_connected").into()));
            return rx;
        };
        self.rt.spawn(async move {
            let result = async {
                let stream = link
                    .conn
                    .open_subsystem("sftp")
                    .await
                    .map_err(|e| t!("sftp.open_failed", err = e).into_owned())?;
                let client = scell_vfs::SftpClient::with_owner(stream, link)
                    .await
                    .map_err(|e| t!("sftp.handshake_failed", err = e).into_owned())?;
                Ok(Arc::new(client) as Arc<dyn scell_vfs::Vfs>)
            }
            .await;
            let _ = tx.send(result);
        });
        rx
    }
}

pub(crate) fn spawn_ssh(
    target: SshTarget,
    mode: SshMode,
    idle: bool,
    size: TermSize,
    shared: Arc<Shared>,
    input: flume::Receiver<Vec<u8>>,
    env: SshEnv,
) -> Arc<dyn TransportControl> {
    let (resize_tx, resize) = mpsc::unbounded_channel();
    let (reconnect_tx, reconnect) = mpsc::unbounded_channel();
    let link = CurrentLink::default();
    let control = Arc::new(SshControl {
        resize: resize_tx,
        reconnect: reconnect_tx,
        task: Mutex::new(None),
        link: link.clone(),
        rt: env.rt.clone(),
    });
    let encoding = Encoding::of_label(target.host.encoding.as_deref());
    let ui = Arc::new(Interaction {
        shared: shared.clone(),
        known_hosts: KnownHostsFile {
            path: target.known_hosts.clone(),
        },
    });
    let rt = env.rt.clone();
    let session = SshSession {
        target,
        mode,
        idle,
        size,
        shared,
        input,
        resize,
        reconnect,
        env,
        ui,
        encoding,
        link,
    };
    let task = rt.spawn(session.run());
    *control.task.lock() = Some(task.abort_handle());
    control
}

/// 一个 SSH 会话的后台任务。连接断开后任务不结束，等待重连，终端与回滚内容一直沿用。
struct SshSession {
    target: SshTarget,
    mode: SshMode,
    /// 先不连接，等第一次重连请求。
    idle: bool,
    size: TermSize,
    shared: Arc<Shared>,
    input: flume::Receiver<Vec<u8>>,
    resize: mpsc::UnboundedReceiver<TermSize>,
    reconnect: mpsc::UnboundedReceiver<()>,
    env: SshEnv,
    ui: Arc<Interaction>,
    encoding: Encoding,
    link: CurrentLink,
}

/// 一次连接的结局。
enum Ended {
    /// 远端 shell 给出了退出码。
    Exit(u32),
    /// 连接断开或没能建立。`transient` 表示网络层面的问题，值得自动重试；`connected` 表示这次连上过，
    /// `stable` 表示连上后保持了至少 [`STABLE_AFTER`]。
    Lost {
        transient: bool,
        connected: bool,
        stable: bool,
        reason: String,
    },
}

/// 建立连接失败的原因，已带上出错的那一跳的名称。
struct HopError {
    message: String,
    transient: bool,
}

impl HopError {
    fn new(hop: &HostProfile, e: &scell_ssh::SshError) -> Self {
        Self {
            message: t!("session.hop_failed", host = hop.name, err = e).into(),
            transient: e.is_transient(),
        }
    }
}

impl SshSession {
    async fn run(mut self) {
        let id = self.shared.id;
        // 只有连上过才自动重连：首次就连不上多半是地址有误或主机不在线
        let mut ever_connected = false;
        let mut failures = 0;
        // 进度行的写法：首次直接写；会话结束后另起一行；上次没连上时覆盖那行错误，连续失败只留最近一条
        let mut lead = "";
        if self.idle && self.reconnect.recv().await.is_none() {
            return;
        }
        loop {
            // 断开期间视图不接受输入，这里只丢弃断开那一刻还在途的字节
            while self.input.try_recv().is_ok() {}
            self.shared.emit(SessionEvent::Connecting(id));
            let ended = self.attempt(lead).await;
            lead = match ended {
                Ended::Lost {
                    connected: false, ..
                } => "\r\x1b[2K",
                _ => "\r\n",
            };
            let retry_in = match ended {
                Ended::Exit(code) => {
                    self.shared.emit(SessionEvent::Exited {
                        id,
                        code: Some(code),
                    });
                    None
                }
                Ended::Lost {
                    transient,
                    connected,
                    stable,
                    reason,
                } => {
                    ever_connected |= connected;
                    if stable {
                        failures = 0;
                    }
                    let auto = ever_connected
                        && transient
                        && self.env.auto_reconnect.load(Ordering::Relaxed);
                    let retry_in = auto.then(|| backoff(failures));
                    failures += 1;
                    self.shared.emit(SessionEvent::Disconnected {
                        id,
                        retry_in,
                        reason,
                    });
                    retry_in
                }
            };
            let timer = async {
                match retry_in {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                request = self.reconnect.recv() => if request.is_none() {
                    return;
                },
                () = timer => {}
            }
            while self.reconnect.try_recv().is_ok() {}
        }
    }

    /// 建立连接（池中已有则复用）、打开 shell 并转发字节，直到 shell 结束或连接断开。
    async fn attempt(&mut self, lead: &str) -> Ended {
        let host = &self.target.host;
        notice(
            &self.shared,
            &format!("{lead}\x1b[2m{} ({})…\x1b[0m", host.name, host.address()),
        );
        let (mut link, reused) = match connect_chain(&self.target, &self.ui, &self.env.pool).await {
            Ok(c) => c,
            Err(e) => return self.fail(e),
        };
        // 转发属于连接：复用已有连接时它们已在运行
        if !reused {
            self.start_forwards(&link).await;
        }
        if self.mode == SshMode::Files {
            return self.hold(link).await;
        }
        while let Ok(s) = self.resize.try_recv() {
            self.size = s;
        }
        let (cols, rows) = (self.size.cols, self.size.rows);
        let mut shell = match link.conn.open_shell(TERM, cols, rows).await {
            Ok(shell) => shell,
            // 共用的连接开不出新通道（如 MaxSessions 已满）时，为这个会话另建一条
            Err(e) if reused => {
                tracing::info!(%e, host = %host.name, "复用的连接无法打开通道，改为新建连接");
                link = match connect_hop(host, link.parent.clone(), &self.ui).await {
                    Ok(link) => link,
                    Err(e) => return self.fail(e),
                };
                match link.conn.open_shell(TERM, cols, rows).await {
                    Ok(shell) => shell,
                    Err(e) => return self.fail(HopError::new(host, &e)),
                }
            }
            Err(e) => return self.fail(HopError::new(host, &e)),
        };
        *self.link.lock() = Some(link.clone());
        // 清掉“连接中”这一行，远端输出从行首开始
        notice(&self.shared, "\r\x1b[2K");
        let id = self.shared.id;
        self.shared.emit(SessionEvent::Connected(id));
        tracing::info!(session = %id, host = %host.name, "SSH 会话已建立");
        let established = std::time::Instant::now();

        let mut pump = Pump::new(
            self.shared.clone(),
            Decoder::new(self.encoding),
            self.env.rt.clone(),
        );
        let encoder = Encoder::new(self.encoding);
        let mut code = None;
        loop {
            tokio::select! {
                event = shell.next_event() => match event {
                    ShellEvent::Data(data) => pump.on_output(&data),
                    ShellEvent::ExitStatus(c) => code = Some(c),
                    ShellEvent::Closed => break,
                },
                Ok(bytes) = self.input.recv_async() => {
                    if shell.write(&encoder.encode(bytes)).await.is_err() {
                        break;
                    }
                }
                Some(s) = self.resize.recv() => {
                    self.size = s;
                    let _ = shell.resize(s.cols, s.rows).await;
                }
            }
        }
        // 连接（及其跳板）要活到 shell 结束；其他会话或 SFTP 仍在用时不会真正断开
        self.link.lock().take();
        drop(link);
        match code {
            Some(code) => Ended::Exit(code),
            None => {
                notice(
                    &self.shared,
                    &format!(
                        "{RESET_MODES}\r\n\x1b[2m{}\x1b[0m",
                        t!("session.connection_lost")
                    ),
                );
                Ended::Lost {
                    transient: true,
                    connected: true,
                    stable: established.elapsed() >= STABLE_AFTER,
                    reason: t!("session.connection_lost").into(),
                }
            }
        }
    }

    /// 文件会话：保持连接直到它断开（SFTP 通道各自持有连接，断开前不受影响）。
    async fn hold(&mut self, link: Arc<Link>) -> Ended {
        *self.link.lock() = Some(link.clone());
        let id = self.shared.id;
        self.shared.emit(SessionEvent::Connected(id));
        tracing::info!(session = %id, host = %self.target.host.name, "文件会话已建立");
        let established = std::time::Instant::now();
        while !link.conn.is_closed() {
            tokio::time::sleep(LINK_POLL).await;
        }
        self.link.lock().take();
        Ended::Lost {
            transient: true,
            connected: true,
            stable: established.elapsed() >= STABLE_AFTER,
            reason: t!("session.connection_lost").into(),
        }
    }

    /// 启动主机配置的端口转发，句柄存在连接上，连接断开即停止。失败的写进终端，不影响会话。
    async fn start_forwards(&self, link: &Link) {
        for rule in &self.target.host.forwards {
            let listen = rule.listen.clone();
            if let Err(e) = crate::forwards::start(link, rule.clone(), true).await {
                notice(
                    &self.shared,
                    &format!(
                        "\r\n\x1b[33m{}\x1b[0m\r\n",
                        t!("forward.start_failed", listen = listen, err = e)
                    ),
                );
            }
        }
    }

    fn fail(&self, e: HopError) -> Ended {
        // 错误替换掉“连接中”这一行
        notice(
            &self.shared,
            &format!("\r\x1b[2K\x1b[31m{}\x1b[0m", e.message),
        );
        Ended::Lost {
            transient: e.transient,
            connected: false,
            stable: false,
            reason: e.message,
        }
    }
}

/// 逐跳建立连接，已在池中且存活的直接复用。返回目标主机这一跳，以及它是否为复用的。
async fn connect_chain(
    target: &SshTarget,
    ui: &Arc<Interaction>,
    pool: &ConnectionPool,
) -> Result<(Arc<Link>, bool), HopError> {
    let mut key = Vec::new();
    let mut parent: Option<Arc<Link>> = None;
    let mut reused = false;
    for hop in target.jumps.iter().chain(std::iter::once(&target.host)) {
        key.push(hop_key(hop));
        // 持锁直到这一跳登记进池（含等待用户确认主机密钥、输入密码），同一主机不会被并发重复连接与提问
        let gate = pool.gate(&key);
        let _guard = gate.lock().await;
        if let Some(link) = pool.get(&key) {
            parent = Some(link);
            reused = true;
            continue;
        }
        let link = connect_hop(hop, parent.take(), ui).await?;
        pool.insert(key.clone(), &link);
        parent = Some(link);
        reused = false;
    }
    let link = parent.expect("链至少含目标主机这一跳");
    Ok((link, reused))
}

/// 新建一跳连接：直连，或经由上一跳的 direct-tcpip 通道。
async fn connect_hop(
    hop: &HostProfile,
    parent: Option<Arc<Link>>,
    ui: &Arc<Interaction>,
) -> Result<Arc<Link>, HopError> {
    let prompter = HopPrompter::new(ui.clone(), hop.id.clone());
    let params = connect_params(hop, prompter.clone());
    if params.user.is_empty() {
        return Err(HopError {
            message: t!("session.no_user", host = hop.name).into(),
            transient: false,
        });
    }
    let known_hosts = ui.known_hosts.clone();
    let verifier: Arc<dyn HostKeyVerifier> = ui.clone();
    let result = match &parent {
        None => Connection::connect(&params, known_hosts, verifier).await,
        Some(prev) => prev.conn.connect_via(&params, known_hosts, verifier).await,
    };
    let conn = result.map_err(|e| HopError::new(hop, &e))?;
    prompter.commit(&ui.shared).await;
    Ok(Arc::new(Link {
        conn,
        parent,
        forwards: Mutex::default(),
    }))
}

fn connect_params(host: &HostProfile, prompter: HopPrompter) -> ConnectParams {
    // 取不到本机用户名时留空，由 connect_hop 在连接前报错，不拿猜测的用户名去试（会计入 fail2ban）
    let user = host
        .user
        .clone()
        .or_else(scell_config::local_user)
        .unwrap_or_default();
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

/// 把 SSH 层的提问转成会话事件，等待 UI 回答。
#[derive(Clone)]
struct Interaction {
    shared: Arc<Shared>,
    known_hosts: KnownHostsFile,
}

impl Interaction {
    async fn ask(&self, kind: PromptKind) -> PromptAnswer {
        let (prompt, reply) = Prompt::new(kind);
        self.shared
            .emit(SessionEvent::Prompt(self.shared.id, prompt));
        reply.await.unwrap_or(PromptAnswer::Cancel)
    }
}

impl HostKeyVerifier for Interaction {
    fn verify(
        &self,
        host: String,
        port: u16,
        key: PublicKey,
        status: HostKeyStatus,
    ) -> BoxFuture<bool> {
        let this = self.clone();
        Box::pin(async move {
            let (shared, known_hosts) = (&this.shared, &this.known_hosts);
            match status {
                HostKeyStatus::Known => true,
                // 密钥变更一律阻断，需用户自行核实后修改 known_hosts
                HostKeyStatus::Changed => {
                    notice(
                        shared,
                        &format!(
                            "\r\n\x1b[31m{}\r\n{}\x1b[0m\r\n",
                            t!(
                                "session.host_key_changed",
                                host = host,
                                file = known_hosts.path.display()
                            ),
                            t!(
                                "session.current_key",
                                fingerprint = scell_ssh::fingerprint(&key)
                            )
                        ),
                    );
                    false
                }
                HostKeyStatus::New => {
                    let kind = PromptKind::HostKey {
                        host: host.clone(),
                        port,
                        algorithm: key.algorithm().as_str().to_owned(),
                        fingerprint: scell_ssh::fingerprint(&key),
                    };
                    if this.ask(kind).await != PromptAnswer::Accept {
                        return false;
                    }
                    if let Err(e) = known_hosts.learn(&host, port, &key) {
                        tracing::warn!(%e, "写入 known_hosts 失败");
                    }
                    true
                }
            }
        })
    }
}

/// 一跳认证的提问方：优先使用凭据库中保存的密码与口令，错误时删除并改为询问；
/// 用户勾选“记住”的输入在这一跳认证成功后才写入凭据库。
#[derive(Clone)]
struct HopPrompter(Arc<Hop>);

struct Hop {
    ui: Arc<Interaction>,
    host: HostId,
    state: Mutex<HopState>,
}

#[derive(Default)]
struct HopState {
    /// 已询问或已从凭据库取用的密码次数。
    password_asks: usize,
    password_from_store: bool,
    /// 已询问过口令的私钥，以及其中取自凭据库的。
    passphrase_asks: Vec<PathBuf>,
    passphrase_from_store: Vec<PathBuf>,
    /// 待认证成功后保存的机密。
    pending: Vec<(Secret, String)>,
}

#[derive(Clone, PartialEq, Eq)]
enum Secret {
    Password(HostId),
    Passphrase(PathBuf),
}

impl Secret {
    fn key(&self) -> SecretKey<'_> {
        match self {
            Self::Password(id) => SecretKey::Password(id),
            Self::Passphrase(path) => SecretKey::Passphrase(path),
        }
    }
}

/// 凭据库调用会阻塞，放到阻塞线程池。
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

impl HopPrompter {
    fn new(ui: Arc<Interaction>, host: HostId) -> Self {
        Self(Arc::new(Hop {
            ui,
            host,
            state: Mutex::new(HopState::default()),
        }))
    }

    async fn commit(&self, shared: &Arc<Shared>) {
        self.0.commit(shared).await;
    }
}

impl Hop {
    /// 先取凭据库；取过且被用过一次（说明不正确）时删除它并询问用户。
    async fn obtain(
        self: Arc<Self>,
        secret: Secret,
        first: bool,
        stale: bool,
        ask: PromptKind,
    ) -> Option<String> {
        if stale {
            let s = secret.clone();
            blocking(move || secrets::delete(s.key())).await;
        }
        if first {
            let s = secret.clone();
            if let Some(Some(stored)) = blocking(move || secrets::get(s.key())).await {
                let mut state = self.state.lock();
                match &secret {
                    Secret::Password(_) => state.password_from_store = true,
                    Secret::Passphrase(path) => state.passphrase_from_store.push(path.clone()),
                }
                return Some(stored);
            }
        }
        match self.ui.ask(ask).await {
            PromptAnswer::Secret { value, remember } => {
                let mut state = self.state.lock();
                state.pending.retain(|(s, _)| *s != secret);
                if remember {
                    state.pending.push((secret, value.clone()));
                }
                Some(value)
            }
            _ => None,
        }
    }

    /// 这一跳认证成功：写入用户要求记住的机密。
    async fn commit(&self, shared: &Arc<Shared>) {
        let pending = std::mem::take(&mut self.state.lock().pending);
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
        let (first, stale) = {
            let mut state = self.0.state.lock();
            state.password_asks += 1;
            let stale = state.password_from_store;
            state.password_from_store = false;
            (state.password_asks == 1, stale)
        };
        let kind = PromptKind::Password {
            user: user.to_owned(),
            host: host.to_owned(),
            retry: !first,
        };
        let secret = Secret::Password(self.0.host.clone());
        Box::pin(self.0.clone().obtain(secret, first, stale, kind))
    }

    fn passphrase(&self, path: &Path) -> BoxFuture<Option<String>> {
        let (first, stale) = {
            let mut state = self.0.state.lock();
            let first = !state.passphrase_asks.iter().any(|p| p == path);
            state.passphrase_asks.push(path.to_owned());
            let before = state.passphrase_from_store.len();
            state.passphrase_from_store.retain(|p| p != path);
            (first, state.passphrase_from_store.len() != before)
        };
        let kind = PromptKind::Passphrase {
            path: path.to_owned(),
            retry: !first,
        };
        let secret = Secret::Passphrase(path.to_owned());
        Box::pin(self.0.clone().obtain(secret, first, stale, kind))
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
