use std::fmt;

/// 以 `/` 分隔的路径，内部为原始字节。SFTP 路径与 Unix 本地路径都直接对应；
/// Windows 本地路径写作 `C:/Users/...`。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct VfsPath(Vec<u8>);

impl VfsPath {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// 在末尾追加一级。`name` 不应含 `/`。
    pub fn join(&self, name: &[u8]) -> Self {
        let mut bytes = self.0.clone();
        if !bytes.is_empty() && !bytes.ends_with(b"/") {
            bytes.push(b'/');
        }
        bytes.extend_from_slice(name);
        Self(bytes)
    }

    fn trimmed(&self) -> &[u8] {
        let mut b = self.0.as_slice();
        while b.len() > 1 && b.ends_with(b"/") && !is_drive_root(b) {
            b = &b[..b.len() - 1];
        }
        b
    }

    /// 上一级目录；根（`/`、`C:/`）或单个相对名没有上一级。
    pub fn parent(&self) -> Option<Self> {
        let b = self.trimmed();
        if b == b"/" || is_drive_root(b) {
            return None;
        }
        let i = b.iter().rposition(|&c| c == b'/')?;
        let parent = &b[..i];
        Some(Self(if parent.is_empty() {
            b"/".to_vec()
        } else if parent.ends_with(b":") {
            [parent, b"/"].concat()
        } else {
            parent.to_vec()
        }))
    }

    /// 最后一级的名称。
    pub fn file_name(&self) -> Option<&[u8]> {
        let b = self.trimmed();
        if b == b"/" || is_drive_root(b) || b.is_empty() {
            return None;
        }
        Some(match b.iter().rposition(|&c| c == b'/') {
            Some(i) => &b[i + 1..],
            None => b,
        })
    }

    /// 按 UTF-8 有损解码，仅用于日志与不关心编码的场合。
    pub fn to_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

/// `C:/` 这样的盘符根。
fn is_drive_root(b: &[u8]) -> bool {
    b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

impl fmt::Debug for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self.0.escape_ascii())
    }
}

impl From<&str> for VfsPath {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }
}

impl From<String> for VfsPath {
    fn from(s: String) -> Self {
        Self(s.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> VfsPath {
        s.into()
    }

    #[test]
    fn join_parent_and_name() {
        assert_eq!(p("/").join(b"etc"), p("/etc"));
        assert_eq!(p("/etc/").join(b"ssh"), p("/etc/ssh"));
        assert_eq!(p("/etc/ssh").parent(), Some(p("/etc")));
        assert_eq!(p("/etc/").parent(), Some(p("/")));
        assert_eq!(p("/").parent(), None);
        assert_eq!(p("C:/Users").parent(), Some(p("C:/")));
        assert_eq!(p("C:/").parent(), None);
        assert_eq!(p("rel").parent(), None);
        assert_eq!(p("/a/b/").file_name(), Some(&b"b"[..]));
        assert_eq!(p("/").file_name(), None);
        // 非 UTF-8 字节原样保留
        let gbk = p("/srv").join(b"\xd6\xd0\xce\xc4.txt");
        assert_eq!(gbk.file_name(), Some(&b"\xd6\xd0\xce\xc4.txt"[..]));
        assert_eq!(format!("{gbk:?}"), r#""/srv/\xd6\xd0\xce\xc4.txt""#);
    }
}
