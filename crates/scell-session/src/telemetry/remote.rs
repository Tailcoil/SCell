//! 远端采集：在连接上开 exec 通道运行探针脚本，按帧解析。

use std::collections::HashMap;
use std::time::Instant;

use scell_ssh::{Connection, ShellEvent};
use tokio::sync::oneshot;

use super::{INTERVAL, MetricsUpdate, Rates, parse};

const PROBE: &str = include_str!("probe.sh");

/// 探针命令：登录 shell 可能不是 sh（fish、csh），统一交给 `sh -s`，脚本经标准输入送入，不落盘也不用转义。
/// 参数 `scell-probe` 只为在进程列表里认得出来。
const COMMAND: &str = "sh -s scell-probe";

const FRAME_BEGIN: &[u8] = b"@@begin\n";
const FRAME_END: &[u8] = b"@@end\n";

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn script() -> String {
    PROBE.replace("INTERVAL", &INTERVAL.as_secs().max(1).to_string())
}

/// 采集直到 `stop` 触发（调用方释放）或通道关闭。停止时主动关闭通道，远端脚本随即在下次写出时退出。
pub(crate) async fn run(
    conn: &Connection,
    tx: flume::Sender<MetricsUpdate>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut channel = match conn.open_exec(COMMAND).await {
        Ok(channel) => channel,
        Err(e) => {
            let _ = tx.send(MetricsUpdate::Failed(
                rust_i18n::t!("monitor.start_failed", err = e).into(),
            ));
            return;
        }
    };
    // 标准输入保持打开：sh 读完整个循环后才执行，之后不再读取
    if let Err(e) = channel.write(script().as_bytes()).await {
        let _ = tx.send(MetricsUpdate::Failed(
            rust_i18n::t!("monitor.start_failed", err = e).into(),
        ));
        return;
    }
    let mut rates = Rates::default();
    // 按字节缓冲：一帧可能跨多段数据到达，多字节字符也可能被切开
    let mut buf: Vec<u8> = Vec::new();
    let mut commands: HashMap<u32, String> = HashMap::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            event = channel.next_event() => match event {
                ShellEvent::Data(data) => {
                    buf.extend_from_slice(&data);
                    if let Some(line) = buf.strip_prefix(b"@@unsupported") {
                        let Some(end) = line.iter().position(|b| *b == b'\n') else {
                            continue;
                        };
                        let os = String::from_utf8_lossy(&line[..end]).trim().to_owned();
                        let _ = tx.send(MetricsUpdate::Unsupported(os));
                        break;
                    }
                    while let Some(end) = find(&buf, FRAME_END) {
                        let frame: Vec<u8> = buf.drain(..end + FRAME_END.len()).collect();
                        let Some(start) = find(&frame, FRAME_BEGIN) else {
                            continue;
                        };
                        let body = String::from_utf8_lossy(&frame[start + FRAME_BEGIN.len()..end]);
                        let mut m = rates.next(parse::frame(&body), Instant::now());
                        fill_commands(conn, &mut m.processes, &mut commands).await;
                        if tx.send(MetricsUpdate::Sample(m)).is_err() {
                            channel.close().await;
                            return;
                        }
                    }
                }
                ShellEvent::ExitStatus(_) => {}
                ShellEvent::Closed => {
                    let _ = tx.send(MetricsUpdate::Failed(rust_i18n::t!("monitor.channel_closed").into()));
                    return;
                }
            },
        }
    }
    channel.close().await;
}

/// 补上进程的命令行。每帧的进程数据只有名称，命令行按需另开一条短通道读取并缓存（进程号复用的概率很低）。
async fn fill_commands(
    conn: &Connection,
    processes: &mut [crate::telemetry::ProcessUsage],
    cache: &mut HashMap<u32, String>,
) {
    let missing: Vec<u32> = processes
        .iter()
        .map(|p| p.pid)
        .filter(|pid| !cache.contains_key(pid))
        .collect();
    if !missing.is_empty() {
        if cache.len() > 512 {
            cache.clear();
        }
        let list = missing
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            "for p in {list}; do printf '%s ' \"$p\"; tr '\\000' ' ' < /proc/$p/cmdline 2>/dev/null; echo; done\n"
        );
        for pid in &missing {
            cache.insert(*pid, String::new());
        }
        if let Ok(output) = run_script(conn, &script).await {
            for line in output.lines() {
                if let Some((pid, cmd)) = line.split_once(' ')
                    && let Ok(pid) = pid.parse()
                {
                    cache.insert(pid, cmd.trim().to_owned());
                }
            }
        }
    }
    for p in processes {
        p.command = cache.get(&p.pid).cloned().unwrap_or_default();
    }
}

/// 以 `sh -s` 运行一段脚本，返回全部输出。
async fn run_script(conn: &Connection, script: &str) -> Result<String, scell_ssh::SshError> {
    let mut channel = conn.open_exec("sh -s").await?;
    channel.write(script.as_bytes()).await?;
    channel.eof().await;
    let mut out = Vec::new();
    loop {
        match channel.next_event().await {
            ShellEvent::Data(d) => out.extend(d),
            ShellEvent::ExitStatus(_) => {}
            ShellEvent::Closed => break,
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}
