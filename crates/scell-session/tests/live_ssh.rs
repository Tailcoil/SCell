//! 经会话层连接真实 sshd 的端到端验证，默认忽略。
//!
//! 运行：`SCELL_SSH_IT=<主机>:<端口>:<用户>:<私钥路径>:<任意> cargo test -p scell-session --test live_ssh -- --ignored`
//! 可选 `SCELL_SSH_IT_HOP=<主机>:<端口>`：把第一台作为跳板连到这台。只用给定私钥认证，不会产生失败尝试。

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use scell_config::hosts::{AuthPref, HostProfile};
use scell_session::{PromptAnswer, PromptKind, SessionEvent, SessionService, SshTarget};
use scell_term::{CellAttrs, RenderSnapshot, TermSize};

fn host(name: &str, addr: &str, port: &str, user: &str, key: &str) -> HostProfile {
    let mut h = HostProfile::new(name, addr);
    h.port = port.parse().unwrap();
    h.user = Some(user.into());
    h.auth = vec![AuthPref::Key { path: key.into() }];
    h
}

fn screen(handle: &scell_session::SessionHandle) -> String {
    let mut snap = RenderSnapshot::default();
    handle.terminal().snapshot(&mut snap);
    (0..snap.rows)
        .map(|r| {
            snap.row(r)
                .iter()
                .filter(|c| !c.attrs.contains(CellAttrs::SPACER))
                .map(|c| c.ch)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "需要 SCELL_SSH_IT"]
fn shell_round_trip_with_new_host_prompt() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    let first = host("first", addr, port, user, key);
    let (target_host, jumps) = match std::env::var("SCELL_SSH_IT_HOP") {
        Ok(hop) => {
            let (h, p) = hop.split_once(':').unwrap();
            (host("hop", h, p, user, key), vec![first])
        }
        Err(_) => (first, Vec::new()),
    };
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: target_host,
        jumps,
        known_hosts: dir.path().join("known_hosts"),
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target, TermSize::new(80, 24, 8, 16));

    let mut host_prompts = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let event = events
            .recv_timeout(deadline - Instant::now())
            .expect("等待连接超时");
        match event {
            SessionEvent::Prompt(_, prompt) => {
                assert!(
                    matches!(prompt.kind, PromptKind::HostKey { .. }),
                    "{prompt:?}"
                );
                host_prompts += 1;
                prompt.answer(PromptAnswer::Accept);
            }
            SessionEvent::Connected(_) => break,
            SessionEvent::Exited { .. } => panic!("连接失败：\n{}", screen(&handle)),
            _ => {}
        }
    }
    // 每一跳都是新主机
    assert_eq!(
        host_prompts,
        1 + std::env::var("SCELL_SSH_IT_HOP").is_ok() as usize
    );
    assert!(
        std::fs::read_to_string(dir.path().join("known_hosts"))
            .unwrap()
            .lines()
            .count()
            >= host_prompts
    );

    handle.write(b"echo scell-$((6*7)); hostname\n".to_vec());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !screen(&handle).contains("scell-42") {
        assert!(
            Instant::now() < deadline,
            "未收到回显：\n{}",
            screen(&handle)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("{}", screen(&handle));

    handle.write(b"exit\n".to_vec());
    let code = loop {
        let event = events
            .recv_timeout(Duration::from_secs(10))
            .expect("等待退出超时");
        if let SessionEvent::Exited { code, .. } = event {
            break code;
        }
    };
    assert_eq!(code, Some(0));
}

/// 等到某个会话建立连接，自动信任新主机，返回期间收到的主机密钥提问数。
fn wait_connected(
    events: &flume::Receiver<SessionEvent>,
    handle: &scell_session::SessionHandle,
) -> usize {
    let mut prompts = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match events
            .recv_timeout(deadline - Instant::now())
            .expect("等待连接超时")
        {
            SessionEvent::Prompt(_, prompt) => {
                assert!(matches!(prompt.kind, PromptKind::HostKey { .. }));
                prompts += 1;
                prompt.answer(PromptAnswer::Accept);
            }
            SessionEvent::Connected(id) if id == handle.id() => return prompts,
            SessionEvent::Exited { id, .. } if id == handle.id() => {
                panic!("连接失败：\n{}", screen(handle))
            }
            _ => {}
        }
    }
}

/// 等到会话断开，返回自动重连前的等待时间。
fn wait_disconnected(events: &flume::Receiver<SessionEvent>) -> Option<Duration> {
    loop {
        match events
            .recv_timeout(Duration::from_secs(10))
            .expect("等待断开超时")
        {
            SessionEvent::Disconnected { retry_in, .. } => return retry_in,
            SessionEvent::Exited { .. } => panic!("断线不应结束会话"),
            _ => {}
        }
    }
}

/// 远端看到的连接来源（`SSH_CONNECTION` 的前两段：客户端地址与端口）。
fn ssh_source(handle: &scell_session::SessionHandle) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // 每次查询用不同的标记，屏幕上残留的旧输出不会被误认
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let tag = format!("SRC{}=", NEXT.fetch_add(1, Ordering::Relaxed));
    handle
        .write(format!("echo \"{tag}$(echo $SSH_CONNECTION | cut -d' ' -f1,2)=\"\n").into_bytes());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = screen(handle);
        // 跳过回显的命令行本身，取输出行
        if let Some(line) = text
            .lines()
            .find(|l| l.trim_start().starts_with(&tag) && l.trim_end().ends_with('='))
        {
            return line.trim().trim_start_matches(&tag).to_owned();
        }
        assert!(Instant::now() < deadline, "未收到输出：\n{text}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "需要 SCELL_SSH_IT"]
fn second_session_reuses_connection() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: host("first", addr, port, user, key),
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let size = TermSize::new(120, 24, 8, 16);

    let a = service.spawn_ssh(target.clone(), size);
    assert_eq!(wait_connected(&events, &a), 1);
    let b = service.spawn_ssh(target.clone(), size);
    assert_eq!(wait_connected(&events, &b), 0);
    let (src_a, src_b) = (ssh_source(&a), ssh_source(&b));
    println!("{src_a}\n{src_b}");
    assert_eq!(src_a, src_b, "两个会话应共用同一条 TCP 连接");

    // 全部关闭后再开，应是一条新连接
    service.close(a.id());
    service.close(b.id());
    std::thread::sleep(Duration::from_millis(500));
    let c = service.spawn_ssh(target, size);
    wait_connected(&events, &c);
    let src_c = ssh_source(&c);
    println!("{src_c}");
    assert_ne!(src_a, src_c);
    service.close(c.id());
}

/// 只针对本机的 `cargo xtask sshd`：测试会杀掉服务端的会话进程来模拟断线。
#[test]
#[ignore = "需要 SCELL_SSH_IT 指向 127.0.0.1"]
fn reconnects_in_place_after_connection_drops() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    if addr != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: host("local", addr, port, user, key),
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target, TermSize::new(100, 24, 8, 16));
    wait_connected(&events, &handle);

    handle.write(b"echo mark-$((40+2))\n".to_vec());
    // 留足余量：机器负载高时 shell 启动可能较慢
    let deadline = Instant::now() + Duration::from_secs(20);
    while !screen(&handle).contains("mark-42") {
        assert!(Instant::now() < deadline, "未收到回显");
        std::thread::sleep(Duration::from_millis(100));
    }
    let before = ssh_source(&handle);

    // shell 的父进程是 sshd 为这条连接开的会话进程，杀掉它连接即断开
    handle.write(b"kill -9 $PPID\n".to_vec());
    assert_eq!(wait_disconnected(&events), Some(Duration::from_secs(1)));
    // 主机已在 known_hosts 中，重连不再提问
    assert_eq!(wait_connected(&events, &handle), 0);
    assert!(screen(&handle).contains("mark-42"), "回滚内容应保留");
    let after = ssh_source(&handle);
    println!("{before}\n{after}");
    assert_ne!(before, after, "应是一条新连接");

    // 连上没多久又断开，失败计数不清零，等待时间翻倍
    handle.write(b"kill -9 $PPID\n".to_vec());
    assert_eq!(wait_disconnected(&events), Some(Duration::from_secs(2)));
    wait_connected(&events, &handle);

    // 手动重连：断开后关掉自动重连，会话停在断开状态，直到调用 reconnect
    service.set_auto_reconnect(false);
    handle.write(b"kill -9 $PPID\n".to_vec());
    assert_eq!(wait_disconnected(&events), None);
    assert!(
        events.recv_timeout(Duration::from_secs(2)).is_err(),
        "不应自动重连"
    );
    handle.reconnect();
    wait_connected(&events, &handle);
    service.close(handle.id());
}

/// 按主机编码双向转码：远端输出的 GBK 字节显示为中文，输入的中文以 GBK 字节到达远端。
/// 只运行只读命令，可指向任何 sshd。
#[test]
#[ignore = "需要 SCELL_SSH_IT"]
fn gbk_host_round_trip() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    let mut profile = host("gbk", addr, port, user, key);
    profile.encoding = Some("gbk".into());
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: profile,
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target, TermSize::new(100, 24, 8, 16));
    wait_connected(&events, &handle);

    let wait_for = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !screen(&handle).contains(needle) {
            assert!(
                Instant::now() < deadline,
                "未出现 {needle}：\n{}",
                screen(&handle)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    // 登录 shell 的提示符可能含 UTF-8 符号，按 GBK 解码后会错位，换成简单的 sh
    handle.write(b"exec env PS1='gbk$ ' sh\n".to_vec());
    wait_for("gbk$");
    // “中文”的 GBK 编码为 D6 D0 CE C4，命令行本身只含 ASCII
    handle.write(b"printf 'out:\\326\\320\\316\\304\\n'\n".to_vec());
    wait_for("out:中文");
    handle.write("printf %s 'in:中文' | od -An -tx1\n".as_bytes().to_vec());
    wait_for("69 6e 3a d6 d0 ce c4");
    service.close(handle.id());
}

/// 在已建立的终端连接上打开 SFTP（不再认证），终端关闭后 SFTP 仍可用；不开 shell 的文件会话复用同一连接。只读。
#[test]
#[ignore = "需要 SCELL_SSH_IT"]
fn sftp_rides_on_shared_connection() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: host("sftp", addr, port, user, key),
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target.clone(), TermSize::new(80, 24, 8, 16));
    wait_connected(&events, &handle);

    let vfs = rt.block_on(handle.open_vfs()).expect("打开 SFTP");
    let home = rt.block_on(vfs.home()).unwrap();
    let before = rt.block_on(vfs.list(&home)).unwrap().len();
    service.close(handle.id());
    std::thread::sleep(Duration::from_millis(300));
    let after = rt.block_on(vfs.list(&home)).unwrap().len();
    assert_eq!(before, after);

    // 不开 shell 的文件会话：连接仍在池中，复用时不再询问主机密钥
    let files = service.spawn_files(target, false);
    assert_eq!(wait_connected(&events, &files), 0);
    let vfs = rt.block_on(files.open_vfs()).expect("文件会话打开 SFTP");
    assert_eq!(rt.block_on(vfs.list(&home)).unwrap().len(), after);
    service.close(files.id());
}

/// 两层跳板：C 经 B、B 经 A，C 的链由主机库递归展开。三跳都指向同一个 sshd，经前一跳的 direct-tcpip 到达。
#[test]
#[ignore = "需要 SCELL_SSH_IT 指向 127.0.0.1"]
fn nested_jump_hosts_connect() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    if addr != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    let a = host("A", addr, port, user, key);
    let mut b = host("B", addr, port, user, key);
    b.jump = vec![a.id.clone()];
    let mut c = host("C", addr, port, user, key);
    c.jump = vec![b.id.clone()];
    let mut book = scell_config::HostBook::default();
    for h in [&a, &b, &c] {
        book.upsert(h.clone());
    }
    let jumps: Vec<HostProfile> = book.jump_chain(&c).unwrap().into_iter().cloned().collect();
    assert_eq!(
        jumps.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
        ["A", "B"]
    );
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: c,
        jumps,
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target, TermSize::new(80, 24, 8, 16));
    wait_connected(&events, &handle);
    service.close(handle.id());
}

/// 远端监控：指标与本机 /proc 一致（127.0.0.1 上的 sshd 就是本机），释放后探针进程退出。
#[test]
#[ignore = "需要 SCELL_SSH_IT 指向 127.0.0.1"]
fn remote_metrics_probe_exits_after_release() {
    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    if addr != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: host("metrics", addr, port, user, key),
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_files(target, false);
    wait_connected(&events, &handle);

    let watch = handle.watch_metrics();
    let mut samples = Vec::new();
    while samples.len() < 2 {
        let update = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), watch.recv()).await })
            .expect("等待采样超时")
            .expect("采集提前结束");
        match update {
            scell_session::MetricsUpdate::Sample(m) => samples.push(m),
            other => panic!("采集失败：{other:?}"),
        }
    }
    let m = &samples[1];
    println!("{m:?}");
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap();
    let total_kb: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap();
    assert_eq!(m.memory.total, total_kb * 1024);
    assert!(m.cpu.is_some_and(|c| (0. ..=1.).contains(&c)));
    assert!(m.net_rx.is_some());
    assert!(m.disks.iter().any(|d| d.mount == "/"));
    assert!(!m.processes.is_empty());

    drop(watch);
    // 脚本每 2 秒写一次，写到已关闭的通道时退出
    std::thread::sleep(Duration::from_secs(5));
    let left = std::process::Command::new("pgrep")
        .args(["-f", "scell-prob[e]"])
        .output()
        .unwrap();
    assert!(
        left.stdout.is_empty(),
        "探针仍在运行：{}",
        String::from_utf8_lossy(&left.stdout)
    );
    service.close(handle.id());
}

/// 主机配置的本地转发随连接启动，复用连接的会话不重复绑定，连接全部关闭后停止。
/// 会在本机监听端口，只针对 127.0.0.1。
#[test]
#[ignore = "需要 SCELL_SSH_IT 指向 127.0.0.1"]
fn host_forwards_follow_the_connection() {
    use scell_config::hosts::{ForwardKind, ForwardRule};
    use std::io::{Read, Write};

    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    if addr != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }
    // 回显服务与一个空闲端口
    let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in echo.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0; 64];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 || s.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = free.local_addr().unwrap().port();
    drop(free);

    let mut profile = host("fwd", addr, port, user, key);
    profile.forwards = vec![ForwardRule {
        kind: ForwardKind::Local,
        listen: listen.to_string(),
        target: format!("127.0.0.1:{echo_port}"),
        note: String::new(),
    }];
    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: profile,
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let size = TermSize::new(100, 24, 8, 16);
    let a = service.spawn_ssh(target.clone(), size);
    wait_connected(&events, &a);
    let b = service.spawn_ssh(target, size);
    wait_connected(&events, &b);
    assert!(!screen(&b).contains("未能启动"), "复用连接不应重复启动转发");

    let mut s = std::net::TcpStream::connect(("127.0.0.1", listen)).unwrap();
    s.write_all(b"hello").unwrap();
    let mut buf = [0; 5];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    // 流量实时计入，连接未关闭也能看到
    std::thread::sleep(Duration::from_millis(100));
    let status = a.forwards();
    assert_eq!(status.len(), 1);
    assert!(status[0].from_host && status[0].active == 1);
    assert_eq!((status[0].sent, status[0].received), (5, 5));
    drop(s);

    // 运行时添加的 SOCKS 转发：同一连接上的会话都能看到，curl 经它访问本机 HTTP 服务
    let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in http.incoming().flatten() {
            let mut buf = [0; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let socks = free.local_addr().unwrap().port();
    drop(free);
    rt.block_on(a.add_forward(ForwardRule {
        kind: ForwardKind::Dynamic,
        listen: socks.to_string(),
        target: String::new(),
        note: String::new(),
    }))
    .expect("添加 SOCKS 转发");
    assert_eq!(b.forwards().len(), 2);
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "5", "--socks5-hostname"])
        .arg(format!("127.0.0.1:{socks}"))
        .arg(format!("http://127.0.0.1:{http_port}/"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
    let dynamic = b.forwards().into_iter().find(|f| !f.from_host).unwrap();
    assert!(dynamic.received > 0);
    b.remove_forward(dynamic.id);
    assert_eq!(a.forwards().len(), 1);
    std::thread::sleep(Duration::from_millis(100));
    assert!(std::net::TcpStream::connect(("127.0.0.1", socks)).is_err());

    service.close(a.id());
    service.close(b.id());
    std::thread::sleep(Duration::from_millis(300));
    assert!(std::net::TcpStream::connect(("127.0.0.1", listen)).is_err());
}

/// 登录时自动 su 提权（含密码错误重试）、按序执行后续步骤，同一连接上打开提权 SFTP 复用已验证密码，
/// 以及会话运行期间手动触发自动操作步骤。只针对 127.0.0.1 的 `cargo xtask it`。
#[test]
#[ignore = "需要 SCELL_SSH_IT 指向 127.0.0.1"]
fn automation_switch_user_and_sftp() {
    use scell_config::hosts::{Automation, Step, SwitchMethod};

    let raw = std::env::var("SCELL_SSH_IT").expect("需要设置 SCELL_SSH_IT");
    let parts: Vec<&str> = raw.splitn(5, ':').collect();
    let [addr, port, user, key, _] = parts[..] else {
        panic!("SCELL_SSH_IT 格式错误");
    };
    if addr != "127.0.0.1" {
        eprintln!("只在本机 sshd 上运行，跳过");
        return;
    }

    // xtask 在私钥旁的 bin/ 里放了假的 su、sudo。exec 通道经 sshd 的 SetEnv 拿到这个 PATH，
    // 登录 shell 会被 /etc/profile 重设 PATH（如 Debian），要先自己 export
    let bin = std::path::Path::new(key).parent().unwrap().join("bin");
    let mut profile = host("auto", addr, port, user, key);
    profile.automations = vec![
        Automation {
            name: "切换到运维管理员".into(),
            on_login: true,
            steps: vec![
                Step::Send {
                    text: format!("export PATH={}:$PATH", bin.display()),
                },
                Step::SwitchUser {
                    method: SwitchMethod::Su,
                    user: "opsroot".into(),
                    login: true,
                },
                Step::Expect {
                    wait: "#".into(),
                    send: "echo auto-switched-$SCELL_SWITCHED_USER".into(),
                },
                Step::Send {
                    text: "echo login-step-done".into(),
                },
            ],
        },
        Automation {
            name: "巡检".into(),
            on_login: false,
            steps: vec![Step::Send {
                text: "echo manual-check-$((20+22))".into(),
            }],
        },
    ];
    assert_eq!(profile.login_switch(), Some((SwitchMethod::Su, "opsroot")));

    let dir = tempfile::tempdir().unwrap();
    let target = SshTarget {
        host: profile.clone(),
        jumps: Vec::new(),
        known_hosts: dir.path().join("known_hosts"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service.spawn_ssh(target, TermSize::new(100, 24, 8, 16));

    // 1. 信任主机密钥并等待连接建立
    wait_connected(&events, &handle);

    // 2. 登录自动执行 SwitchUser：第一次故意给错密码，验证触发 retry=true 的第二次询问
    let first_pw = loop {
        if let SessionEvent::Prompt(_, prompt) = events
            .recv_timeout(Duration::from_secs(15))
            .expect("等待首次 su 密码提问超时")
        {
            break prompt;
        }
    };
    assert!(
        matches!(
            &first_pw.kind,
            PromptKind::Password {
                user,
                retry: false,
                ..
            } if user == "opsroot"
        ),
        "预期首次询问 opsroot 密码，实际：{:?}",
        first_pw.kind
    );
    first_pw.answer(PromptAnswer::Secret {
        value: "wrong-password".into(),
        remember: false,
    });

    let retry_pw = loop {
        if let SessionEvent::Prompt(_, prompt) = events
            .recv_timeout(Duration::from_secs(15))
            .expect("等待重试 su 密码提问超时")
        {
            break prompt;
        }
    };
    assert!(
        matches!(
            &retry_pw.kind,
            PromptKind::Password {
                user,
                retry: true,
                ..
            } if user == "opsroot"
        ),
        "预期重试询问 opsroot 密码，实际：{:?}",
        retry_pw.kind
    );
    retry_pw.answer(PromptAnswer::Secret {
        value: "scell-su-pw".into(),
        remember: false,
    });

    let wait_for = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !screen(&handle).contains(needle) {
            assert!(
                Instant::now() < deadline,
                "未出现 {needle}：\n{}",
                screen(&handle)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    wait_for("auto-switched-opsroot");
    wait_for("login-step-done");

    // 3. 打开提权 SFTP：应复用连接缓存中已验证通过的 opsroot 密码，不再弹窗询问
    let vfs = rt.block_on(handle.open_vfs()).expect("打开提权 SFTP");
    let home = rt.block_on(vfs.home()).unwrap();
    assert!(!rt.block_on(vfs.list(&home)).unwrap().is_empty());

    // 4. 手动触发第二组自动操作步骤
    handle.run_steps(profile.automations[1].steps.clone());
    wait_for("manual-check-42");

    service.close(handle.id());
}
