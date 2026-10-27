//! 端口转发：本地（-L）、远程（-R）与动态 SOCKS5（-D）。
//!
//! 每个转发由 [`Forward`] 句柄代表，释放句柄即停止监听（已建立的连接继续到各自结束）。
//! 转发依赖其所在的 [`crate::Connection`]，连接断开后转发自然失效。

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parking_lot::Mutex;
use russh::client::{Handle, Msg};
use russh::{Channel, ChannelOpenFailure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::SshError;
use crate::connect::ClientHandler;
use crate::liveness::Guarded;

/// 转发的流量统计。
#[derive(Debug, Default)]
pub struct ForwardStats {
    /// 当前打开的连接数。
    pub active: AtomicUsize,
    /// 累计的字节数（发往远端 / 从远端收到），传输中实时增加。
    pub sent: AtomicU64,
    pub received: AtomicU64,
}

/// 远程转发的路由：服务器上监听的端口 → 本地目标。由连接处理器在服务器发来连接时查询。
pub(crate) type RemoteRoutes = Arc<Mutex<HashMap<u32, RemoteRoute>>>;

pub(crate) struct RemoteRoute {
    host: String,
    port: u16,
    stats: Arc<ForwardStats>,
}

/// 一条正在运行的转发。释放即停止。
pub struct Forward {
    /// 实际监听的端口（请求 0 时由系统或服务器分配）。
    pub port: u16,
    pub stats: Arc<ForwardStats>,
    task: Option<tokio::task::AbortHandle>,
    remote: Option<RemoteCancel>,
}

struct RemoteCancel {
    handle: Arc<Handle<ClientHandler>>,
    routes: RemoteRoutes,
    address: String,
    port: u32,
}

impl Drop for Forward {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(remote) = self.remote.take() {
            remote.routes.lock().remove(&remote.port);
            // 通知服务器停止监听；没有运行时（进程退出中）时连接也即将关闭，无需通知
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = remote
                        .handle
                        .cancel_tcpip_forward(remote.address, remote.port)
                        .await;
                });
            }
        }
    }
}

/// 在两端之间转发字节，结束时计入统计。SSH 连接断开（`dead`）时两个方向立即结束。
async fn pipe(
    tcp: TcpStream,
    channel: Channel<Msg>,
    stats: Arc<ForwardStats>,
    dead: CancellationToken,
) {
    stats.active.fetch_add(1, Ordering::Relaxed);
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut ssh_read, mut ssh_write) =
        tokio::io::split(Guarded::channel(channel.into_stream(), &dead));
    // 一个方向读到结尾后关闭对端的写入，另一方向继续直到对方也结束
    tokio::join!(
        async {
            copy_counting(&mut tcp_read, &mut ssh_write, &stats.sent).await;
            let _ = ssh_write.shutdown().await;
        },
        async {
            copy_counting(&mut ssh_read, &mut tcp_write, &stats.received).await;
            let _ = tcp_write.shutdown().await;
        },
    );
    stats.active.fetch_sub(1, Ordering::Relaxed);
}

/// 复制到读完或出错，每块都计入 `counter`。
async fn copy_counting(
    from: &mut (impl tokio::io::AsyncRead + Unpin),
    to: &mut (impl tokio::io::AsyncWrite + Unpin),
    counter: &AtomicU64,
) {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
}

async fn listen(host: &str, port: u16) -> Result<TcpListener, SshError> {
    TcpListener::bind((host, port))
        .await
        .map_err(SshError::Connect)
}

pub(crate) async fn local(
    handle: Arc<Handle<ClientHandler>>,
    dead: CancellationToken,
    bind: (&str, u16),
    target: (String, u16),
) -> Result<Forward, SshError> {
    let listener = listen(bind.0, bind.1).await?;
    let port = listener.local_addr().map_err(SshError::Connect)?.port();
    let stats = Arc::new(ForwardStats::default());
    let task_stats = stats.clone();
    let task = tokio::spawn(async move {
        while let Ok((tcp, peer)) = listener.accept().await {
            let (handle, target, stats, dead) = (
                handle.clone(),
                target.clone(),
                task_stats.clone(),
                dead.clone(),
            );
            tokio::spawn(async move {
                let origin = (peer.ip().to_string(), u32::from(peer.port()));
                match handle
                    .channel_open_direct_tcpip(target.0, u32::from(target.1), origin.0, origin.1)
                    .await
                {
                    Ok(channel) => pipe(tcp, channel, stats, dead).await,
                    Err(e) => tracing::debug!(%e, "本地转发：远端拒绝连接"),
                }
            });
        }
    });
    Ok(Forward {
        port,
        stats,
        task: Some(task.abort_handle()),
        remote: None,
    })
}

pub(crate) async fn dynamic(
    handle: Arc<Handle<ClientHandler>>,
    dead: CancellationToken,
    bind: (&str, u16),
) -> Result<Forward, SshError> {
    let listener = listen(bind.0, bind.1).await?;
    let port = listener.local_addr().map_err(SshError::Connect)?.port();
    let stats = Arc::new(ForwardStats::default());
    let task_stats = stats.clone();
    let task = tokio::spawn(async move {
        while let Ok((tcp, peer)) = listener.accept().await {
            let (handle, stats, dead) = (handle.clone(), task_stats.clone(), dead.clone());
            tokio::spawn(async move {
                if let Err(e) = socks5(tcp, peer, handle, stats, dead).await {
                    tracing::debug!(%e, "SOCKS5 请求失败");
                }
            });
        }
    });
    Ok(Forward {
        port,
        stats,
        task: Some(task.abort_handle()),
        remote: None,
    })
}

/// SOCKS5（RFC 1928）的无认证 CONNECT。
async fn socks5(
    mut tcp: TcpStream,
    peer: std::net::SocketAddr,
    handle: Arc<Handle<ClientHandler>>,
    stats: Arc<ForwardStats>,
    dead: CancellationToken,
) -> std::io::Result<()> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_owned());
    // 问候：版本、方法数、方法列表；只接受“无需认证”
    let mut head = [0u8; 2];
    tcp.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(bad("not SOCKS5"));
    }
    let mut methods = vec![0u8; usize::from(head[1])];
    tcp.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        tcp.write_all(&[5, 0xff]).await?;
        return Err(bad("client requires authentication"));
    }
    tcp.write_all(&[5, 0]).await?;

    // 请求：版本、命令、保留、地址类型、地址、端口
    let mut req = [0u8; 4];
    tcp.read_exact(&mut req).await?;
    let reply = |code: u8| [5, code, 0, 1, 0, 0, 0, 0, 0, 0];
    if req[1] != 1 {
        // 只支持 CONNECT
        tcp.write_all(&reply(7)).await?;
        return Err(bad("unsupported SOCKS command"));
    }
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            tcp.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let len = tcp.read_u8().await?;
            let mut name = vec![0u8; usize::from(len)];
            tcp.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| bad("domain name is not UTF-8"))?
        }
        4 => {
            let mut a = [0u8; 16];
            tcp.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => {
            tcp.write_all(&reply(8)).await?;
            return Err(bad("unsupported address type"));
        }
    };
    let port = tcp.read_u16().await?;
    match handle
        .channel_open_direct_tcpip(
            host,
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
    {
        Ok(channel) => {
            tcp.write_all(&reply(0)).await?;
            pipe(tcp, channel, stats, dead).await;
            Ok(())
        }
        Err(e) => {
            // 远端连不上目标：一般失败
            tcp.write_all(&reply(5)).await?;
            Err(std::io::Error::other(e.to_string()))
        }
    }
}

pub(crate) async fn remote(
    handle: Arc<Handle<ClientHandler>>,
    routes: RemoteRoutes,
    bind: (&str, u16),
    target: (String, u16),
) -> Result<Forward, SshError> {
    let port = handle.tcpip_forward(bind.0, u32::from(bind.1)).await?;
    // 请求 0 时服务器返回分配的端口，否则返回 0
    let port = if bind.1 == 0 { port } else { u32::from(bind.1) };
    let stats = Arc::new(ForwardStats::default());
    routes.lock().insert(
        port,
        RemoteRoute {
            host: target.0,
            port: target.1,
            stats: stats.clone(),
        },
    );
    Ok(Forward {
        port: port as u16,
        stats,
        task: None,
        remote: Some(RemoteCancel {
            handle,
            routes,
            address: bind.0.to_owned(),
            port,
        }),
    })
}

/// 服务器为远程转发发来的连接：按端口找到本地目标并接通，没有登记的拒绝。
pub(crate) fn accept_remote(
    routes: &RemoteRoutes,
    channel: Channel<Msg>,
    port: u32,
    reply: russh::client::ChannelOpenHandle,
    dead: CancellationToken,
) {
    let route = routes
        .lock()
        .get(&port)
        .map(|r| (r.host.clone(), r.port, r.stats.clone()));
    let Some((host, target_port, stats)) = route else {
        tokio::spawn(reply.reject(ChannelOpenFailure::AdministrativelyProhibited));
        return;
    };
    tokio::spawn(async move {
        match TcpStream::connect((host.as_str(), target_port)).await {
            Ok(tcp) => {
                reply.accept().await;
                pipe(tcp, channel, stats, dead).await;
            }
            Err(e) => {
                tracing::debug!(%e, host, target_port, "远程转发：本地目标连不上");
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
        }
    });
}
