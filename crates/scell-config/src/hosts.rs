//! 主机库（`hosts.toml`）。由界面整体读写，不保留手写注释；不含任何机密，密码只在连接时询问。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// 主机的稳定标识，跳板引用与最近使用都按它关联，改名不受影响。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(pub String);

impl HostId {
    pub fn generate() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(nanos);
        Self(format!("{:016x}", hasher.finish()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AuthPref {
    Agent,
    Key { path: PathBuf },
    Password,
    KeyboardInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// `-L`：本机监听，转到远端可达的目标。
    #[default]
    Local,
    /// `-R`：服务器监听，转回本机可达的目标。
    Remote,
    /// `-D`：本机 SOCKS5 代理。
    Dynamic,
}

/// 连接建立后自动启动的端口转发。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRule {
    #[serde(rename = "type")]
    pub kind: ForwardKind,
    /// 监听地址 `[主机:]端口`：本地与动态转发在本机，远程转发在服务器上；省略主机时只监听回环地址。
    pub listen: String,
    /// 目标 `主机:端口`；动态转发没有。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl ForwardRule {
    /// 解析后的监听地址；格式不对时为 `None`。
    pub fn listen_addr(&self) -> Option<(String, u16)> {
        let (host, port) = parse_address(&self.listen)?;
        Some((host.unwrap_or_else(|| "127.0.0.1".into()), port))
    }

    /// 解析后的目标地址，必须写明主机。动态转发或格式不对时为 `None`。
    pub fn target_addr(&self) -> Option<(String, u16)> {
        match parse_address(&self.target)? {
            (Some(host), port) => Some((host, port)),
            (None, _) => None,
        }
    }

    /// 规则是否完整可用。
    pub fn is_valid(&self) -> bool {
        self.listen_addr().is_some()
            && (self.kind == ForwardKind::Dynamic || self.target_addr().is_some())
    }
}

/// `端口`、`主机:端口` 或 `[IPv6]:端口`。
pub fn parse_address(s: &str) -> Option<(Option<String>, u16)> {
    let s = s.trim();
    if let Ok(port) = s.parse::<u16>() {
        return Some((None, port));
    }
    let (host, port) = match s.strip_prefix('[') {
        Some(rest) => {
            let (host, tail) = rest.split_once(']')?;
            (host, tail.strip_prefix(':')?)
        }
        None => s.rsplit_once(':')?,
    };
    let host = host.trim();
    (!host.is_empty() && !host.contains(char::is_whitespace))
        .then_some(())
        .and(port.parse().ok().map(|p| (Some(host.to_owned()), p)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HostProfile {
    pub id: HostId,
    pub name: String,
    /// 分组路径，层级以 `/` 分隔，如 `生产/web`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 自由标签，参与搜索。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub favorite: bool,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// 为空时使用本机用户名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// 按顺序尝试；为空时依次尝试 ssh-agent、默认私钥、keyboard-interactive 与密码。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth: Vec<AuthPref>,
    /// 跳板链，从最外层开始；每一跳是主机库中的另一台主机。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jump: Vec<HostId>,
    /// 图标颜色的名称（如 `blue`）；为空时用默认前景色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// 列表与标签上显示的图标名（Lucide 名称，如 `database`）；为空或不认识时用默认的服务器图标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<ForwardRule>,
    /// 终端编码名，如 `gbk`；为空时为 UTF-8。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// 状态面板可见时在服务器上运行监控脚本。
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub monitor: bool,
}

fn default_port() -> u16 {
    22
}

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

impl HostProfile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            id: HostId::generate(),
            name: name.into(),
            group: None,
            tags: Vec::new(),
            favorite: false,
            host: host.into(),
            port: 22,
            user: None,
            auth: Vec::new(),
            jump: Vec::new(),
            color: None,
            icon: None,
            forwards: Vec::new(),
            encoding: None,
            notes: String::new(),
            monitor: true,
        }
    }

    /// `user@host:port` 形式的地址，端口为 22 时省略。
    pub fn address(&self) -> String {
        let mut s = String::new();
        if let Some(user) = &self.user {
            s.push_str(user);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != 22 {
            s.push_str(&format!(":{}", self.port));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HostBook {
    #[serde(default, rename = "host")]
    pub hosts: Vec<HostProfile>,
}

impl HostBook {
    /// 文件不存在时为空库。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::Parse {
                path: path.to_owned(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// 原子写回。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        crate::paths::write_atomic(path, &text)
    }

    pub fn get(&self, id: &HostId) -> Option<&HostProfile> {
        self.hosts.iter().find(|h| &h.id == id)
    }

    /// 列表的显示顺序：未分组的在前，其余按分组名聚在一起，组内保持库中的顺序。
    pub fn display_order(&self) -> Vec<&HostProfile> {
        let mut hosts: Vec<&HostProfile> = self.hosts.iter().collect();
        hosts.sort_by(|a, b| a.group.cmp(&b.group));
        hosts
    }

    /// 拖动排序：把 `id` 放到 `target` 的位置并归入它的分组，返回是否有变化。
    /// 与拖动标签一样按显示顺序先移走再插入，往下拖落在目标之后，往上拖落在目标之前；
    /// 移动后库中的顺序即显示顺序。
    pub fn move_host(&mut self, id: &HostId, target: &HostId) -> bool {
        let mut order: Vec<usize> = (0..self.hosts.len()).collect();
        order.sort_by(|&a, &b| self.hosts[a].group.cmp(&self.hosts[b].group));
        let position = |id: &HostId| order.iter().position(|&i| &self.hosts[i].id == id);
        let (Some(from), Some(to)) = (position(id), position(target)) else {
            return false;
        };
        if from == to {
            return false;
        }
        let group = self.hosts[order[to]].group.clone();
        let moved = order.remove(from);
        order.insert(to, moved);
        self.hosts[moved].group = group;
        let mut slots: Vec<Option<HostProfile>> = std::mem::take(&mut self.hosts)
            .into_iter()
            .map(Some)
            .collect();
        self.hosts = order.into_iter().filter_map(|i| slots[i].take()).collect();
        true
    }

    /// 新增或按 id 替换。
    pub fn upsert(&mut self, host: HostProfile) {
        match self.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(slot) => *slot = host,
            None => self.hosts.push(host),
        }
    }

    /// 删除主机，并从其他主机的跳板链中去掉它。
    pub fn remove(&mut self, id: &HostId) {
        self.hosts.retain(|h| &h.id != id);
        for h in &mut self.hosts {
            h.jump.retain(|j| j != id);
        }
    }

    /// 解析跳板链为主机列表（最外层在前）。跳板自己的跳板递归展开：C 经 B、B 经 A 时 C 的链为 A、B。
    /// `jump` 列了多跳时只展开第一跳，之后各跳本就经由前一跳到达。引用缺失或成环时返回出问题的 id。
    pub fn jump_chain(&self, host: &HostProfile) -> Result<Vec<&HostProfile>, HostId> {
        let mut chain = Vec::new();
        self.expand(host, &mut vec![host.id.clone()], &mut chain)?;
        Ok(chain)
    }

    /// 把 `host` 的跳板链追加到 `chain`；`path` 为正在展开的主机，用于发现环。
    fn expand<'a>(
        &'a self,
        host: &HostProfile,
        path: &mut Vec<HostId>,
        chain: &mut Vec<&'a HostProfile>,
    ) -> Result<(), HostId> {
        for (i, id) in host.jump.iter().enumerate() {
            let hop = self.get(id).ok_or_else(|| id.clone())?;
            if path.contains(&hop.id) || chain.iter().any(|h| h.id == hop.id) {
                return Err(id.clone());
            }
            if i == 0 {
                path.push(hop.id.clone());
                self.expand(hop, path, chain)?;
                path.pop();
            }
            if chain.iter().any(|h| h.id == hop.id) {
                return Err(id.clone());
            }
            chain.push(hop);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_forward_addresses() {
        assert_eq!(parse_address("8080"), Some((None, 8080)));
        assert_eq!(
            parse_address(" 0.0.0.0:80 "),
            Some((Some("0.0.0.0".into()), 80))
        );
        assert_eq!(parse_address("[::1]:22"), Some((Some("::1".into()), 22)));
        assert_eq!(
            parse_address("db.local:5432"),
            Some((Some("db.local".into()), 5432))
        );
        assert_eq!(parse_address("host"), None);
        assert_eq!(parse_address(":80"), None);
        assert_eq!(parse_address("99999"), None);
        let rule = ForwardRule {
            kind: ForwardKind::Local,
            listen: "8080".into(),
            target: "80".into(),
            note: String::new(),
        };
        assert!(!rule.is_valid(), "目标必须写主机");
        let dynamic = ForwardRule {
            kind: ForwardKind::Dynamic,
            listen: "1080".into(),
            target: String::new(),
            note: String::new(),
        };
        assert_eq!(dynamic.listen_addr(), Some(("127.0.0.1".into(), 1080)));
        assert!(dynamic.is_valid());
    }

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        let mut book = HostBook::default();
        let mut a = HostProfile::new("跳板", "bastion.example");
        a.user = Some("ops".into());
        a.auth = vec![AuthPref::Agent];
        let mut b = HostProfile::new("web", "10.0.0.2");
        b.port = 2222;
        b.jump = vec![a.id.clone()];
        b.color = Some("red".into());
        b.icon = Some("database".into());
        b.forwards = vec![
            ForwardRule {
                kind: ForwardKind::Local,
                listen: "8080".into(),
                target: "localhost:80".into(),
                note: "后台".into(),
            },
            ForwardRule {
                kind: ForwardKind::Dynamic,
                listen: "1080".into(),
                target: String::new(),
                note: String::new(),
            },
        ];
        book.upsert(a.clone());
        book.upsert(b.clone());
        book.save(&path).unwrap();

        let loaded = HostBook::load(&path).unwrap();
        assert_eq!(loaded, book);
        assert_eq!(loaded.jump_chain(&b).unwrap()[0].name, "跳板");
        assert_eq!(b.address(), "10.0.0.2:2222");
        assert_eq!(a.address(), "ops@bastion.example");

        let minimal: HostBook =
            toml::from_str("[[host]]\nid = \"x\"\nname = \"n\"\nhost = \"h\"\n").unwrap();
        assert_eq!(minimal.hosts[0].port, 22);
    }

    #[test]
    fn remove_cleans_jump_references_and_cycles_are_rejected() {
        let mut book = HostBook::default();
        let a = HostProfile::new("a", "a");
        let mut b = HostProfile::new("b", "b");
        b.jump = vec![a.id.clone()];
        let mut c = HostProfile::new("c", "c");
        c.jump = vec![c.id.clone()];
        book.upsert(a.clone());
        book.upsert(b.clone());
        book.upsert(c.clone());
        assert!(book.jump_chain(&c).is_err());
        // 两层跳板：d 经 b，b 经 a
        let mut d = HostProfile::new("d", "d");
        d.jump = vec![b.id.clone()];
        book.upsert(d.clone());
        let names: Vec<&str> = book
            .jump_chain(&d)
            .unwrap()
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        // 间接成环：a 经 d
        let mut a2 = a.clone();
        a2.jump = vec![d.id.clone()];
        book.upsert(a2);
        assert!(book.jump_chain(&d).is_err());
        let mut a3 = a.clone();
        a3.jump.clear();
        book.upsert(a3);
        book.remove(&a.id);
        assert!(book.get(&b.id).unwrap().jump.is_empty());
    }

    #[test]
    fn move_host_follows_display_order() {
        let mut book = HostBook::default();
        let mut add = |name: &str, group: Option<&str>| {
            let mut h = HostProfile::new(name, name);
            h.id = HostId(name.into());
            h.group = group.map(Into::into);
            book.upsert(h);
        };
        // 库中顺序与显示顺序不同：显示为 u1 u2 | a1 a2 | b1 b2
        add("b1", Some("b"));
        add("a1", Some("a"));
        add("u1", None);
        add("a2", Some("a"));
        add("b2", Some("b"));
        add("u2", None);
        let names = |book: &HostBook| -> Vec<String> {
            let order: Vec<String> = book
                .display_order()
                .iter()
                .map(|h| h.name.clone())
                .collect();
            let stored: Vec<String> = book.hosts.iter().map(|h| h.name.clone()).collect();
            assert_eq!(order, stored, "移动后库中顺序即显示顺序");
            order
        };
        let id = |s: &str| HostId(s.into());

        // 往下拖到组内另一台上：落在它之后
        assert!(book.move_host(&id("a1"), &id("a2")));
        assert_eq!(names(&book), ["u1", "u2", "a2", "a1", "b1", "b2"]);
        // 往上跨组拖：落在目标之前，并归入目标的分组
        assert!(book.move_host(&id("b2"), &id("a2")));
        assert_eq!(names(&book), ["u1", "u2", "b2", "a2", "a1", "b1"]);
        assert_eq!(book.get(&id("b2")).unwrap().group.as_deref(), Some("a"));
        // 往下跨组拖：落在目标之后
        assert!(book.move_host(&id("u1"), &id("b1")));
        assert_eq!(names(&book), ["u2", "b2", "a2", "a1", "b1", "u1"]);
        assert_eq!(book.get(&id("u1")).unwrap().group.as_deref(), Some("b"));
        // 拖回原处、目标不存在都不算变化
        assert!(!book.move_host(&id("a1"), &id("a1")));
        assert!(!book.move_host(&id("x"), &id("a1")));
    }
}
