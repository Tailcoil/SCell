//! 针对真实 sshd 的端到端验证，默认忽略。
//!
//! 运行：`SCELL_SSH_IT=<主机>:<端口>:<用户>:<私钥路径>:<样本目录> cargo test -p scell-ssh -- --ignored`
//!
//! 可选 `SCELL_SSH_IT_HOP=<主机>:<端口>`：经第一台跳到这台（同一用户与私钥）；未设置时跳回第一台自己。
//!
//! 可选 `SCELL_SSH_IT_AGENT=<套接字>`：`cargo xtask sshd up` 启动的、只装有测试私钥的 agent，仅用于 127.0.0.1。

#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use scell_ssh::{
    AuthMethod, ConnectParams, Connection, Endpoint, HostKeyStatus, HostKeyVerifier,
    KnownHostsFile, PublicKey, ShellEvent,
};

struct Env {
    params: ConnectParams,
    sample_dir: String,
}

fn env() -> Env {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [host, port, user, key, dir] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    Env {
        params: ConnectParams::new(
            Endpoint::new(host, port.parse().expect("端口")),
            user,
            vec![AuthMethod::KeyFile {
                path: key.into(),
                passphrase: None,
            }],
        ),
        sample_dir: dir.into(),
    }
}

/// 记录收到的状态；新主机自动信任并写入 known_hosts。
struct Recorder {
    file: KnownHostsFile,
    seen: Mutex<Vec<HostKeyStatus>>,
}

impl HostKeyVerifier for Recorder {
    fn verify(
        &self,
        host: String,
        port: u16,
        key: PublicKey,
        status: HostKeyStatus,
    ) -> Pin<Box<dyn Future<Output = bool> + Send>> {
        self.seen.lock().unwrap().push(status);
        let accept = match status {
            HostKeyStatus::Known => true,
            HostKeyStatus::New => self.file.learn(&host, port, &key).is_ok(),
            HostKeyStatus::Changed => false,
        };
        Box::pin(async move { accept })
    }
}

fn temp_known_hosts(name: &str) -> KnownHostsFile {
    let path = std::env::temp_dir().join(format!("scell-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    KnownHostsFile { path }
}

#[tokio::test]
#[ignore]
async fn host_key_new_then_known_and_shell_roundtrip() {
    let env = env();
    let file = temp_known_hosts("shell");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });

    drop(
        Connection::connect(&env.params, file.clone(), verifier.clone())
            .await
            .expect("首次连接"),
    );
    let conn = Connection::connect(&env.params, file.clone(), verifier.clone())
        .await
        .expect("二次连接");
    assert_eq!(
        *verifier.seen.lock().unwrap(),
        vec![HostKeyStatus::New, HostKeyStatus::Known]
    );

    let mut shell = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("shell");
    shell
        .write(b"printf 'spike-%s\\n' ok; exit 7\n")
        .await
        .expect("写入");
    let (mut out, mut code) = (Vec::new(), None);
    loop {
        match shell.next_event().await {
            ShellEvent::Data(d) => out.extend(d),
            ShellEvent::ExitStatus(c) => code = Some(c),
            ShellEvent::Closed => break,
        }
    }
    assert!(String::from_utf8_lossy(&out).contains("spike-ok"));
    assert_eq!(code, Some(7));
}

#[tokio::test]
#[ignore]
async fn changed_host_key_is_reported() {
    let env = env();
    let file = temp_known_hosts("changed");
    // 同算法、不同密钥的伪造记录
    let fake = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    // OpenSSH 对 22 端口只写主机名，其他端口写 [主机]:端口
    let ep = &env.params.endpoint;
    let host = if ep.port == 22 {
        ep.host.clone()
    } else {
        format!("[{}]:{}", ep.host, ep.port)
    };
    std::fs::write(&file.path, format!("{host} ssh-ed25519 {fake}\n")).unwrap();
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });

    let result = Connection::connect(&env.params, file, verifier.clone()).await;
    assert!(result.is_err());
    assert_eq!(*verifier.seen.lock().unwrap(), vec![HostKeyStatus::Changed]);
}

#[tokio::test]
#[ignore]
async fn jump_to_another_host() {
    let env = env();
    let mut hop_params = env.params.clone();
    // 未指定第二台时经第一台跳回它自己，同样走 direct-tcpip 与嵌套握手
    if let Ok(hop) = std::env::var("SCELL_SSH_IT_HOP") {
        let (host, port) = hop.rsplit_once(':').expect("SCELL_SSH_IT_HOP 格式错误");
        hop_params.endpoint = Endpoint::new(host, port.parse().expect("端口"));
    }

    let file = temp_known_hosts("hop");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });
    let jump = Connection::connect(&env.params, file.clone(), verifier.clone())
        .await
        .expect("跳板");
    let target = jump
        .connect_via(&hop_params, file, verifier)
        .await
        .expect("经跳板连接");
    let mut shell = target.open_shell("xterm", 80, 24).await.expect("shell");
    shell
        .write(b"printf 'hop:%s\\n' \"$(hostname)\"; exit\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    loop {
        match shell.next_event().await {
            ShellEvent::Data(d) => out.extend(d),
            ShellEvent::Closed => break,
            ShellEvent::ExitStatus(_) => {}
        }
    }
    let out = String::from_utf8_lossy(&out);
    eprintln!("{out}");
    assert!(out.contains("hop:"));
}

#[tokio::test]
#[ignore]
async fn agent_auth_with_dedicated_agent() {
    let Ok(socket) = std::env::var("SCELL_SSH_IT_AGENT") else {
        eprintln!("未设置 SCELL_SSH_IT_AGENT，跳过");
        return;
    };
    let mut params = env().params;
    // 测试 agent 里只有本机 sshd 认可的密钥，对其他主机只会产生失败认证
    assert_eq!(
        params.endpoint.host, "127.0.0.1",
        "agent 测试只针对本机 sshd"
    );
    params.auth = vec![AuthMethod::Agent];
    params.agent_socket = Some(socket.into());
    let file = temp_known_hosts("agent");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });
    let conn = Connection::connect(&params, file, verifier)
        .await
        .expect("agent 认证");
    let mut shell = conn.open_shell("xterm", 80, 24).await.expect("shell");
    shell.write(b"exit 0\n").await.unwrap();
    while !matches!(shell.next_event().await, ShellEvent::Closed) {}
}

/// scell-vfs 的 SFTP 客户端经真实 SSH 通道：文件名保留原始字节。
#[tokio::test]
#[ignore]
async fn scell_vfs_sftp_keeps_raw_file_names() {
    use scell_vfs::{SftpClient, Vfs, VfsPath};
    let env = env();
    let file = temp_known_hosts("vfs");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });
    let conn = Connection::connect(&env.params, file, verifier)
        .await
        .expect("连接");
    let sftp = SftpClient::new(conn.open_subsystem("sftp").await.expect("sftp 子系统"))
        .await
        .expect("sftp 握手");
    let names: Vec<Vec<u8>> = sftp
        .list(&VfsPath::from(env.sample_dir.as_str()))
        .await
        .expect("列目录")
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"utf8-中文.txt".as_bytes().to_vec()));
    assert!(
        names.contains(&b"\xd6\xd0\xce\xc4.txt".to_vec()),
        "{names:?}"
    );
}

/// 经 SSH 通道的流水线读写吞吐。会在本机写临时文件，只针对 127.0.0.1。
#[tokio::test]
#[ignore]
async fn scell_vfs_sftp_throughput() {
    use bytes::Bytes;
    use scell_vfs::{LocalFs, SftpClient, Vfs, WriteMode};
    let env = env();
    if env.params.endpoint.host != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    let file = temp_known_hosts("vfs-speed");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });
    let conn = Connection::connect(&env.params, file, verifier)
        .await
        .unwrap();
    let sftp = SftpClient::new(conn.open_subsystem("sftp").await.unwrap())
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = LocalFs::from_path(&dir.path().join("speed.bin"));
    const MIB: usize = 1024 * 1024;
    let block = Bytes::from(vec![0x5a; MIB]);

    let start = std::time::Instant::now();
    let mut w = sftp.open_write(&path, WriteMode::Truncate).await.unwrap();
    for _ in 0..64 {
        w.write(block.clone()).await.unwrap();
    }
    w.finish().await.unwrap();
    let up = start.elapsed();

    let start = std::time::Instant::now();
    let mut r = sftp.open_read(&path, 0).await.unwrap();
    let mut total = 0;
    while let Some(chunk) = r.read().await.unwrap() {
        total += chunk.len();
    }
    let down = start.elapsed();
    assert_eq!(total, 64 * MIB);
    eprintln!(
        "上传 {:.0} MiB/s，下载 {:.0} MiB/s",
        64.0 / up.as_secs_f64(),
        64.0 / down.as_secs_f64()
    );
}

/// 本地、远程、动态三种转发都能接通本机的回显服务；释放句柄后不再监听。
/// 会让服务器开监听端口，只针对 127.0.0.1。
#[tokio::test]
#[ignore]
async fn port_forwards_reach_echo_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let env = env();
    if env.params.endpoint.host != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    async fn round_trip(mut s: TcpStream, msg: &[u8]) {
        s.write_all(msg).await.unwrap();
        let mut buf = vec![0; msg.len()];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, msg);
    }

    let file = temp_known_hosts("forward");
    let verifier = Arc::new(Recorder {
        file: file.clone(),
        seen: Mutex::default(),
    });
    let conn = Connection::connect(&env.params, file, verifier)
        .await
        .unwrap();
    let target = ("127.0.0.1".to_owned(), echo_port);

    let local = conn
        .forward_local(("127.0.0.1", 0), target.clone())
        .await
        .unwrap();
    let s = TcpStream::connect(("127.0.0.1", local.port)).await.unwrap();
    round_trip(s, b"local").await;

    let remote = conn
        .forward_remote(("127.0.0.1", 0), target.clone())
        .await
        .unwrap();
    assert_ne!(remote.port, 0);
    let s = TcpStream::connect(("127.0.0.1", remote.port))
        .await
        .unwrap();
    round_trip(s, b"remote").await;

    let socks = conn.forward_dynamic(("127.0.0.1", 0)).await.unwrap();
    let mut s = TcpStream::connect(("127.0.0.1", socks.port)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut greet = [0; 2];
    s.read_exact(&mut greet).await.unwrap();
    assert_eq!(greet, [5, 0]);
    // CONNECT 域名 localhost:echo_port
    let mut req = vec![5, 1, 0, 3, 9];
    req.extend_from_slice(b"localhost");
    req.extend_from_slice(&echo_port.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut reply = [0; 10];
    s.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0, "SOCKS 回复 {reply:?}");
    round_trip(s, b"socks").await;

    let port = local.port;
    drop(local);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "释放后不应再监听"
    );
    drop((remote, socks));
}
