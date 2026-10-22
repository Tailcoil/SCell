//! SSH 连接复用。连接按“到这一跳为止的跳板链”登记，所以同一主机的多个终端共用一条连接，
//! 经同一跳板去不同目标时跳板连接也共用。池里只存弱引用：会话持有链上的强引用，
//! 最后一个使用者结束时整条链随之断开，不需要空闲回收。

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use scell_config::HostProfile;
use scell_ssh::Connection;

/// 链上的一跳：本跳连接、它所经由的上一跳（保证跳板活得比下游久），以及在这条连接上运行的端口转发。
pub(crate) struct Link {
    pub(crate) conn: Connection,
    pub(crate) parent: Option<Arc<Link>>,
    pub(crate) forwards: Mutex<Vec<crate::forwards::ActiveForward>>,
    /// 本次连接期间已验证通过的密码（含未勾选“记住”的），供同一连接上的切换用户与提权 SFTP 复用。
    pub(crate) secrets: crate::transport::SecretCache,
    /// 串行化切换用户（终端登录切换与提权 SFTP），避免并发弹出同一个密码提示。
    pub(crate) switch_gate: tokio::sync::Mutex<()>,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectionPool {
    links: Arc<Mutex<HashMap<Vec<String>, Weak<Link>>>>,
    /// 每条链一把建连锁。同时打开同一主机的多个会话（如恢复布局时终端与 SFTP）排队建连，
    /// 后到的复用先建好的连接；否则会各建一条，主机的端口转发也会重复启动而端口冲突。
    gates: Arc<Mutex<HashMap<Vec<String>, Gate>>>,
}

type Gate = Arc<tokio::sync::Mutex<()>>;

impl ConnectionPool {
    pub(crate) fn gate(&self, key: &[String]) -> Gate {
        self.gates.lock().entry(key.to_vec()).or_default().clone()
    }

    /// 仍然存活的连接。
    pub(crate) fn get(&self, key: &[String]) -> Option<Arc<Link>> {
        self.links
            .lock()
            .get(key)
            .and_then(Weak::upgrade)
            .filter(|link| !link.conn.is_closed())
    }

    pub(crate) fn insert(&self, key: Vec<String>, link: &Arc<Link>) {
        let mut links = self.links.lock();
        links.retain(|_, l| l.strong_count() > 0);
        links.insert(key, Arc::downgrade(link));
    }
}

/// 一跳的复用键。地址或用户改动后不再复用旧连接；认证方式的改动不影响已建立的连接。
pub(crate) fn hop_key(host: &HostProfile) -> String {
    format!(
        "{}|{}@{}:{}",
        host.id.0,
        host.user.as_deref().unwrap_or_default(),
        host.host,
        host.port
    )
}
