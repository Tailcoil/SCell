use std::path::Path;

use russh::keys::agent::client::AgentClient;

use crate::SshError;

/// 连接 ssh-agent：`socket` 为指定的套接字（Windows 上为命名管道），未指定时 Unix 读取 `SSH_AUTH_SOCK`，
/// Windows 使用 OpenSSH Agent 的默认命名管道。
#[cfg(unix)]
pub(crate) async fn connect(
    socket: Option<&Path>,
) -> Result<AgentClient<tokio::net::UnixStream>, SshError> {
    match socket {
        Some(path) => AgentClient::connect_uds(path).await,
        None => AgentClient::connect_env().await,
    }
    .map_err(|e| SshError::Agent(e.to_string()))
}

#[cfg(windows)]
pub(crate) async fn connect(
    socket: Option<&Path>,
) -> Result<AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>, SshError> {
    let pipe = socket.map_or(r"\\.\pipe\openssh-ssh-agent".as_ref(), Path::as_os_str);
    AgentClient::connect_named_pipe(pipe)
        .await
        .map_err(|e| SshError::Agent(e.to_string()))
}
