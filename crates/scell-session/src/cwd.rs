//! 解析 OSC 7 上报的工作目录（`file://host/path`）。

use std::path::PathBuf;

/// 指向本机时返回路径；远端主机（如 SSH 里的 shell）返回 `None`。
pub fn local_cwd(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if !host.is_empty() && host != "localhost" && !is_local_host(host) {
        return None;
    }
    let path = percent_decode(path)?;
    // Windows 下形如 /C:/Users
    #[cfg(windows)]
    let path = path.strip_prefix('/').unwrap_or(&path).to_owned();
    Some(PathBuf::from(path))
}

/// OSC 7 中的路径部分，按原始字节返回，不论指向哪台主机。
/// 非 ASCII 字符一般已被 shell 百分号编码，解码后即为远端文件名的原始字节。
pub fn osc7_path(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    percent_decode_bytes(path)
}

fn is_local_host(host: &str) -> bool {
    hostname().is_some_and(|h| h.eq_ignore_ascii_case(host))
}

fn hostname() -> Option<String> {
    sysinfo::System::host_name()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
}

fn percent_decode(s: &str) -> Option<String> {
    String::from_utf8(percent_decode_bytes(s)?).ok()
}

fn percent_decode_bytes(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_rejects_remote() {
        assert_eq!(
            local_cwd("file:///home/u/my%20dir"),
            Some(PathBuf::from("/home/u/my dir"))
        );
        assert_eq!(
            local_cwd("file://localhost/tmp"),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(local_cwd("file://some-remote-host-xyz/tmp"), None);
        assert_eq!(local_cwd("https://x/y"), None);
        assert_eq!(local_cwd("file:///bad%zz"), None);
        assert_eq!(
            osc7_path("file://remote/srv/%D6%D0").as_deref(),
            Some(&b"/srv/\xd6\xd0"[..])
        );
    }
}
