//! 集成测试共用：直接启动本机 OpenSSH 的 sftp-server，经标准输入输出通信。

use scell_vfs::SftpClient;
use tokio::process::Child;

const SFTP_SERVERS: &[&str] = &[
    "/usr/lib/ssh/sftp-server",
    "/usr/lib/openssh/sftp-server",
    "/usr/libexec/sftp-server",
    "/usr/libexec/openssh/sftp-server",
    r"C:\Windows\System32\OpenSSH\sftp-server.exe",
];

/// Unix 上找不到 sftp-server 算环境缺失，直接失败而不是静默通过；
/// Windows 默认不装 OpenSSH 服务端，此时返回 `None` 由调用方跳过。
pub async fn sftp() -> Option<(SftpClient, Child)> {
    let Some(server) = SFTP_SERVERS
        .iter()
        .find(|p| std::path::Path::new(p).exists())
    else {
        if cfg!(unix) {
            panic!("未找到 OpenSSH 的 sftp-server");
        }
        eprintln!("未找到 sftp-server，跳过");
        return None;
    };
    let mut child = tokio::process::Command::new(server)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stream = tokio::io::join(child.stdout.take()?, child.stdin.take()?);
    Some((SftpClient::new(stream).await.unwrap(), child))
}
