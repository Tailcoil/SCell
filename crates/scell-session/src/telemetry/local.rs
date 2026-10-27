//! 本机采集，基于 sysinfo，各平台通用。

use std::time::{Duration, Instant};

use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};
use tokio::sync::oneshot;

use super::{DiskUsage, INTERVAL, Metrics, MetricsUpdate, ProcessUsage, TOP_PROCESSES, Usage};

pub(crate) async fn run(tx: flume::Sender<MetricsUpdate>, mut stop: oneshot::Receiver<()>) {
    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::new().with_cpu_usage())
            .with_memory(MemoryRefreshKind::everything()),
    );
    let mut networks = Networks::new_with_refreshed_list();
    let mut disks = Disks::new_with_refreshed_list();
    let mut last = Instant::now();
    let mut first = true;
    let mut cpu_times = None;
    loop {
        // 遍历全部进程、对每个挂载点 statvfs 都是阻塞调用（网络盘无响应时可能一直卡住），
        // 不能占着运行时仅有的两个工作线程，否则 SSH 收发跟着停顿
        tokio::task::block_in_place(|| {
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                ProcessRefreshKind::new()
                    .with_cpu()
                    .with_memory()
                    .with_cmd(sysinfo::UpdateKind::OnlyIfNotSet),
            );
            networks.refresh();
            disks.refresh();
        });
        let now = Instant::now();
        let secs = now.duration_since(last).as_secs_f64();
        last = now;
        let (rx, tx_bytes) = networks
            .iter()
            .filter(|(name, _)| !is_loopback(name))
            .fold((0u64, 0u64), |(r, t), (_, n)| {
                (r + n.received(), t + n.transmitted())
            });
        let load = (!cfg!(windows)).then(|| {
            let l = System::load_average();
            [l.one as f32, l.five as f32, l.fifteen as f32]
        });
        let mut seen = Vec::new();
        let disk_list = disks
            .iter()
            .filter(|d| d.total_space() > 0 && !d.is_removable())
            .filter(|d| {
                // 同一设备挂在多处（bind mount、btrfs 子卷）只计一次
                let name = d.name().to_os_string();
                let new = !seen.contains(&name);
                seen.push(name);
                new
            })
            .map(|d| DiskUsage {
                mount: d.mount_point().display().to_string(),
                usage: Usage {
                    used: d.total_space() - d.available_space(),
                    total: d.total_space(),
                },
            })
            .collect();
        let mut processes: Vec<ProcessUsage> = if first {
            Vec::new()
        } else {
            // Linux 上 sysinfo 把各线程也列为进程，内存取的是整个进程的值，会重复出现
            sys.processes()
                .iter()
                .filter(|(_, p)| p.thread_kind().is_none())
                .map(|(pid, p)| ProcessUsage {
                    pid: pid.as_u32(),
                    name: p.name().to_string_lossy().into_owned(),
                    command: p
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                    cpu: p.cpu_usage() / 100.,
                    memory: p.memory(),
                })
                .collect()
        };
        processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
        processes.truncate(TOP_PROCESSES);
        let metrics = Metrics {
            cpu: global_cpu(&sys, &mut cpu_times, first),
            memory: Usage {
                used: sys.used_memory(),
                total: sys.total_memory(),
            },
            swap: Usage {
                used: sys.used_swap(),
                total: sys.total_swap(),
            },
            load,
            uptime: Duration::from_secs(System::uptime()),
            net_rx: (!first && secs > 0.).then(|| rx as f64 / secs),
            net_tx: (!first && secs > 0.).then(|| tx_bytes as f64 / secs),
            disks: disk_list,
            processes,
        };
        first = false;
        if tx.send(MetricsUpdate::Sample(metrics)).is_err() {
            return;
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(INTERVAL) => {}
        }
    }
}

/// sysinfo 0.31 在 Linux 上把 guest（虚拟机）时间算进总时间却不算进工作时间，
/// 宿主跑虚拟机时占用会明显偏低，因此 Linux 上与远程端一样直接读 `/proc/stat`。
#[cfg(target_os = "linux")]
fn global_cpu(_: &System, prev: &mut Option<(u64, u64)>, _first: bool) -> Option<f32> {
    let now = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| s.lines().next().and_then(super::parse::cpu));
    super::cpu_ratio(now, std::mem::replace(prev, now))
}

/// sysinfo 的 CPU 占用要两次刷新之间才有意义
#[cfg(not(target_os = "linux"))]
fn global_cpu(sys: &System, _: &mut Option<(u64, u64)>, first: bool) -> Option<f32> {
    (!first).then(|| sys.global_cpu_usage() / 100.)
}

fn is_loopback(name: &str) -> bool {
    name == "lo" || name.starts_with("Loopback")
}
