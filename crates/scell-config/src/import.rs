//! 从 OpenSSH 的 `~/.ssh/config` 导入主机。
//!
//! 每个不含通配符的 `Host` 别名成为一台主机，参数按 OpenSSH 的规则合并（含 `Host *` 等通配块）。
//! `ProxyJump` 指向别名时引用那台主机，否则按 `[user@]host[:port]` 另建一台跳板主机。
//! `LocalForward`、`RemoteForward`、`DynamicForward` 导入为主机的转发规则；Unix 套接字转发与 `Match` 块不支持，跳过。

use std::path::Path;

use ssh2_config::{ParseRule, SshConfig};

use crate::ConfigError;
use crate::hosts::{AuthPref, ForwardKind, ForwardRule, HostBook, HostId, HostProfile};

/// OpenSSH 的用户配置文件 `~/.ssh/config`。
pub fn default_ssh_config() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join(".ssh").join("config"))
}

/// 解析并合并进主机库，返回新增的主机数。与已有主机同名或同地址（主机、端口、用户都相同）的跳过，
/// 其他主机的跳板引用改指向已有的那台。
pub fn import_ssh_config(book: &mut HostBook, path: &Path) -> Result<usize, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    let config = SshConfig::default()
        .parse(
            &mut text.as_bytes(),
            ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
        )
        .map_err(|e| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
    Ok(merge(book, &config, &forward_blocks(&text)))
}

fn merge(book: &mut HostBook, config: &SshConfig, forwards: &[ForwardBlock]) -> usize {
    let mut importer = Importer {
        book,
        config,
        forwards,
        added: 0,
    };
    for alias in aliases(config) {
        importer.alias(&alias);
    }
    importer.added
}

/// 所有可直接连接的别名，按出现顺序去重。
fn aliases(config: &SshConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for host in config.get_hosts() {
        for clause in &host.pattern {
            let wildcard = clause.pattern.contains(['*', '?']);
            if !clause.negated && !wildcard && !out.contains(&clause.pattern) {
                out.push(clause.pattern.clone());
            }
        }
    }
    out
}

struct Importer<'a> {
    book: &'a mut HostBook,
    config: &'a SshConfig,
    forwards: &'a [ForwardBlock],
    added: usize,
}

impl Importer<'_> {
    /// 导入一个别名（及其跳板），返回它在主机库中的 id。
    fn alias(&mut self, alias: &str) -> HostId {
        if let Some(existing) = self.book.hosts.iter().find(|h| h.name == alias) {
            return existing.id.clone();
        }
        let profile = self.profile(alias, alias, None, None);
        self.insert(profile)
    }

    fn profile(
        &mut self,
        name: &str,
        query: &str,
        user: Option<String>,
        port: Option<u16>,
    ) -> HostProfile {
        let params = self.config.query(query);
        let host = params.host_name.clone().unwrap_or_else(|| query.to_owned());
        let mut profile = HostProfile::new(name, host);
        profile.port = port.or(params.port).unwrap_or(22);
        profile.user = user.or(params.user.clone());
        // 写了 IdentityFile 时保留 OpenSSH 的完整顺序；否则用默认顺序（auth 为空）
        if let Some(files) = params.identity_file.as_ref().filter(|f| !f.is_empty()) {
            profile.auth = std::iter::once(AuthPref::Agent)
                .chain(
                    files
                        .iter()
                        .map(|path| AuthPref::Key { path: path.clone() }),
                )
                .chain([AuthPref::KeyboardInteractive, AuthPref::Password])
                .collect();
        }
        // 与 OpenSSH 相同，所有匹配块里的转发都生效
        profile.forwards = self
            .forwards
            .iter()
            .filter(|b| b.matches(query))
            .flat_map(|b| b.rules.iter().cloned())
            .collect();
        for spec in params.proxy_jump.iter().flatten() {
            if spec.eq_ignore_ascii_case("none") {
                break;
            }
            let id = self.jump(spec);
            profile.jump.push(id);
        }
        profile
    }

    fn jump(&mut self, spec: &str) -> HostId {
        let (user, host, port) = parse_jump(spec);
        let is_alias = user.is_none() && port.is_none() && aliases(self.config).contains(&host);
        if is_alias {
            return self.alias(&host);
        }
        let mut profile = self.profile(spec, &host, user, port);
        // 只作跳板用的主机不建立转发，与 OpenSSH 处理 ProxyJump 时一致
        profile.forwards.clear();
        self.insert(profile)
    }

    fn insert(&mut self, profile: HostProfile) -> HostId {
        let duplicate = self.book.hosts.iter().find(|h| {
            h.name == profile.name
                || (h.host == profile.host && h.port == profile.port && h.user == profile.user)
        });
        if let Some(existing) = duplicate {
            return existing.id.clone();
        }
        let id = profile.id.clone();
        self.book.upsert(profile);
        self.added += 1;
        id
    }
}

/// 一个 `Host` 块的模式与其中的转发。ssh2-config 不解析转发（同一块的多条只保留最后一条），这里单独扫描。
struct ForwardBlock {
    /// （是否取反，小写模式）。
    patterns: Vec<(bool, String)>,
    rules: Vec<ForwardRule>,
}

impl ForwardBlock {
    /// 有取反模式命中时不匹配；否则任一正向模式命中即匹配。
    fn matches(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        let hit = |p: &String| glob(p.as_bytes(), host.as_bytes());
        !self.patterns.iter().any(|(neg, p)| *neg && hit(p))
            && self.patterns.iter().any(|(neg, p)| !*neg && hit(p))
    }
}

fn forward_blocks(text: &str) -> Vec<ForwardBlock> {
    let mut blocks = Vec::new();
    // `Match` 块之后、下一个 `Host` 之前的行不属于任何可匹配的块
    let mut current: Option<ForwardBlock> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or((line, ""));
        let args: Vec<&str> = rest
            .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
            .split_whitespace()
            .map(|a| a.trim_matches('"'))
            .collect();
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                blocks.extend(current.take());
                current = Some(ForwardBlock {
                    patterns: args
                        .iter()
                        .map(|p| match p.strip_prefix('!') {
                            Some(p) => (true, p.to_lowercase()),
                            None => (false, p.to_lowercase()),
                        })
                        .collect(),
                    rules: Vec::new(),
                });
            }
            "match" => blocks.extend(current.take()),
            key => {
                if let Some(block) = current.as_mut()
                    && let Some(rule) = forward_rule(key, &args)
                {
                    block.rules.push(rule);
                }
            }
        }
    }
    blocks.extend(current);
    blocks
}

fn forward_rule(key: &str, args: &[&str]) -> Option<ForwardRule> {
    let (kind, listen, target) = match (key, args) {
        ("localforward", [listen, target, ..]) => (ForwardKind::Local, listen, *target),
        ("remoteforward", [listen, target, ..]) => (ForwardKind::Remote, listen, *target),
        ("dynamicforward", [listen, ..]) => (ForwardKind::Dynamic, listen, ""),
        _ => return None,
    };
    let rule = ForwardRule {
        kind,
        listen: forward_address(listen)?,
        target: if target.is_empty() {
            String::new()
        } else {
            forward_address(target)?
        },
        note: String::new(),
    };
    rule.is_valid().then_some(rule)
}

/// OpenSSH 的转发地址可写成 `host:port` 或 `host/port`，`*` 表示所有地址；以 `/`、`~` 开头的是 Unix 套接字，不支持。
fn forward_address(spec: &str) -> Option<String> {
    if spec.starts_with(['/', '~']) {
        return None;
    }
    let spec = match spec.rsplit_once('/') {
        Some((host, port)) if !spec.starts_with('[') => format!("{host}:{port}"),
        _ => spec.to_owned(),
    };
    Some(match spec.strip_prefix("*:") {
        Some(port) => format!("0.0.0.0:{port}"),
        None => spec,
    })
}

/// ssh_config 的模式：`*` 匹配任意串，`?` 匹配单个字符。
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

/// `[user@]host[:port]` 或 `ssh://[user@]host[:port]`；IPv6 地址需写成 `[addr]:port`。
fn parse_jump(spec: &str) -> (Option<String>, String, Option<u16>) {
    let rest = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, rest) = match rest.rsplit_once('@') {
        Some((u, r)) => (Some(u.to_owned()), r),
        None => (None, rest),
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        match inner.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (inner, None),
        }
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse().ok()),
            _ => (rest, None),
        }
    };
    (user, host.to_owned(), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
Host *
    User ops
    ServerAliveInterval 30

Host bastion
    HostName 203.0.113.1
    Port 2222
    IdentityFile ~/.ssh/bastion

Host web db-*
    HostName 10.0.0.2
    ProxyJump bastion
    LocalForward 8080 localhost:80

Host legacy
    HostName 10.0.0.9
    User root
    ProxyJump admin@198.51.100.7:22022,bastion
    RemoteForward 9000 localhost/9000
    DynamicForward *:1080
    LocalForward /tmp/sock /run/app.sock

Host * !legacy
    LocalForward=5432 db.internal:5432
";

    fn import(text: &str, book: &mut HostBook) -> usize {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, text).unwrap();
        import_ssh_config(book, &path).unwrap()
    }

    #[test]
    fn imports_aliases_with_inherited_params_and_jumps() {
        let mut book = HostBook::default();
        assert_eq!(import(SAMPLE, &mut book), 4);
        let by_name = |n: &str| book.hosts.iter().find(|h| h.name == n).unwrap();

        let bastion = by_name("bastion");
        assert_eq!(
            (bastion.host.as_str(), bastion.port, bastion.user.as_deref()),
            ("203.0.113.1", 2222, Some("ops"))
        );
        assert_eq!(bastion.auth[0], AuthPref::Agent);
        assert!(
            matches!(&bastion.auth[1], AuthPref::Key { path } if path.ends_with(".ssh/bastion"))
        );

        let web = by_name("web");
        assert_eq!(web.jump, vec![bastion.id.clone()]);
        assert!(web.auth.is_empty());
        let rule = |kind, listen: &str, target: &str| ForwardRule {
            kind,
            listen: listen.into(),
            target: target.into(),
            note: String::new(),
        };
        assert_eq!(
            web.forwards,
            [
                rule(ForwardKind::Local, "8080", "localhost:80"),
                rule(ForwardKind::Local, "5432", "db.internal:5432"),
            ]
        );

        // 与 OpenSSH 相同，先出现的块优先：`Host *` 在前，它的 User 覆盖后面的
        let legacy = by_name("legacy");
        assert_eq!(legacy.user.as_deref(), Some("ops"));
        assert_eq!(legacy.jump.len(), 2);
        let hop = book.get(&legacy.jump[0]).unwrap();
        assert_eq!(
            (hop.host.as_str(), hop.port, hop.user.as_deref()),
            ("198.51.100.7", 22022, Some("admin"))
        );
        assert!(hop.forwards.is_empty());
        assert_eq!(legacy.jump[1], bastion.id);
        // 取反模式排除了最后一块；Unix 套接字转发跳过
        assert_eq!(
            legacy.forwards,
            [
                rule(ForwardKind::Remote, "9000", "localhost:9000"),
                rule(ForwardKind::Dynamic, "0.0.0.0:1080", ""),
            ]
        );
        assert!(book.jump_chain(legacy).is_ok());
    }

    #[test]
    fn reimport_adds_nothing() {
        let mut book = HostBook::default();
        import(SAMPLE, &mut book);
        let before = book.hosts.len();
        assert_eq!(import(SAMPLE, &mut book), 0);
        assert_eq!(book.hosts.len(), before);
    }

    #[test]
    fn parses_jump_specs() {
        assert_eq!(parse_jump("h"), (None, "h".into(), None));
        assert_eq!(
            parse_jump("ssh://u@h:2200"),
            (Some("u".into()), "h".into(), Some(2200))
        );
        assert_eq!(
            parse_jump("u@[::1]:22"),
            (Some("u".into()), "::1".into(), Some(22))
        );
    }
}
