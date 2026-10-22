//! 连接上的端口转发：主机配置的规则随连接自动启动，运行时也可临时增删。
//! 转发属于连接，同一主机的会话共用；连接断开后全部失效，重新连上时只恢复主机配置的规则。

use std::sync::atomic::{AtomicU64, Ordering};

use scell_config::hosts::{ForwardKind, ForwardRule};

use crate::pool::Link;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForwardId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 连接上的一条转发：运行中，或启动失败的原因。
pub(crate) struct ActiveForward {
    id: ForwardId,
    rule: ForwardRule,
    from_host: bool,
    state: Result<scell_ssh::Forward, String>,
}

/// 转发的状态快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardStatus {
    pub id: ForwardId,
    pub rule: ForwardRule,
    /// 来自主机配置；否则为运行时临时添加。
    pub from_host: bool,
    /// 实际监听的端口。
    pub port: Option<u16>,
    pub error: Option<String>,
    /// 当前打开的连接数与累计流量（发往远端 / 从远端收到）。
    pub active: usize,
    pub sent: u64,
    pub received: u64,
}

/// 启动一条转发并登记到连接上，失败也登记以便界面显示原因。
pub(crate) async fn start(
    link: &Link,
    rule: ForwardRule,
    from_host: bool,
) -> Result<ForwardId, String> {
    let state = open(link, &rule).await;
    let id = ForwardId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let result = state.as_ref().map(|_| id).map_err(Clone::clone);
    link.forwards.lock().push(ActiveForward {
        id,
        rule,
        from_host,
        state,
    });
    result
}

async fn open(link: &Link, rule: &ForwardRule) -> Result<scell_ssh::Forward, String> {
    let listen = rule
        .listen_addr()
        .ok_or_else(|| rust_i18n::t!("forward.bad_listen").into_owned())?;
    let bind = (listen.0.as_str(), listen.1);
    let target = rule.target_addr();
    let result = match (rule.kind, target) {
        (ForwardKind::Local, Some(target)) => link.conn.forward_local(bind, target).await,
        (ForwardKind::Remote, Some(target)) => link.conn.forward_remote(bind, target).await,
        (ForwardKind::Dynamic, _) => link.conn.forward_dynamic(bind).await,
        (_, None) => return Err(rust_i18n::t!("forward.bad_target").into()),
    };
    result.map_err(|e| e.to_string())
}

/// 停止并移除；释放句柄即停止监听。
pub(crate) fn remove(link: &Link, id: ForwardId) {
    link.forwards.lock().retain(|f| f.id != id);
}

pub(crate) fn list(link: &Link) -> Vec<ForwardStatus> {
    link.forwards
        .lock()
        .iter()
        .map(|f| {
            let (port, error, active, sent, received) = match &f.state {
                Ok(fw) => (
                    Some(fw.port),
                    None,
                    fw.stats.active.load(Ordering::Relaxed),
                    fw.stats.sent.load(Ordering::Relaxed),
                    fw.stats.received.load(Ordering::Relaxed),
                ),
                Err(e) => (None, Some(e.clone()), 0, 0, 0),
            };
            ForwardStatus {
                id: f.id,
                rule: f.rule.clone(),
                from_host: f.from_host,
                port,
                error,
                active,
                sent,
                received,
            }
        })
        .collect()
}
