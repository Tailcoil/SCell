//! 服务器监控：远端用一条长驻 exec 通道运行 POSIX sh 循环脚本读取 /proc（脚本不落盘），
//! 本机用 sysinfo。只在调用方持有 [`MetricsWatch`] 期间采集，释放即停止并关闭通道。

mod local;
mod parse;
mod remote;

use std::time::{Duration, Instant};

pub(crate) use local::run as run_local;
pub(crate) use remote::run as run_remote;

/// 采样间隔。
pub(crate) const INTERVAL: Duration = Duration::from_secs(2);

/// 已用与总量，字节。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub used: u64,
    pub total: u64,
}

impl Usage {
    /// 使用比例（0–1）；总量为 0 时为 0。
    pub fn ratio(&self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            (self.used as f64 / self.total as f64) as f32
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskUsage {
    pub mount: String,
    pub usage: Usage,
}

/// 占用 CPU 最多的进程之一。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessUsage {
    pub pid: u32,
    pub name: String,
    /// 完整命令行；内核线程等没有命令行的为空。
    pub command: String,
    /// CPU 占用，1.0 为一个核满载（与 top 相同，多线程进程可超过 1）。
    pub cpu: f32,
    /// 常驻内存，字节。
    pub memory: u64,
}

/// 列出的进程数（界面按可用高度显示其中前若干个）。
pub(crate) const TOP_PROCESSES: usize = 15;

/// 一次采样的指标。需要两次采样才能算出的项（CPU 占用、网络速率）在首次采样时为 `None`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metrics {
    /// CPU 占用（0–1）。
    pub cpu: Option<f32>,
    pub memory: Usage,
    pub swap: Usage,
    /// 1、5、15 分钟平均负载；Windows 上没有。
    pub load: Option<[f32; 3]>,
    pub uptime: Duration,
    /// 网络收发速率（字节每秒），统计口径见 [`net_sum`]。
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
    pub disks: Vec<DiskUsage>,
    /// CPU 占用最高的几个进程，从高到低；首次采样时为空。
    pub processes: Vec<ProcessUsage>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MetricsUpdate {
    Sample(Metrics),
    /// 远端不是 Linux，附 `uname -s`。
    Unsupported(String),
    /// 无法采集或通道已关闭。
    Failed(String),
}

/// 正在进行的采集。释放即停止。
pub struct MetricsWatch {
    pub(crate) rx: flume::Receiver<MetricsUpdate>,
    pub(crate) _stop: tokio::sync::oneshot::Sender<()>,
}

impl MetricsWatch {
    /// 下一条更新；采集结束后为 `None`。
    pub async fn recv(&self) -> Option<MetricsUpdate> {
        self.rx.recv_async().await.ok()
    }
}

/// 一次原始采样：累计计数器与瞬时值。
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Raw {
    pub(crate) uptime: Duration,
    pub(crate) load: Option<[f32; 3]>,
    /// CPU 累计时间：总计与空闲（含 iowait）。
    pub(crate) cpu: Option<(u64, u64)>,
    pub(crate) memory: Usage,
    pub(crate) swap: Usage,
    /// 累计收发字节，统计口径见 [`net_sum`]。
    pub(crate) net: Option<(u64, u64)>,
    pub(crate) disks: Vec<DiskUsage>,
    pub(crate) procs: Option<Procs>,
}

/// 进程的累计 CPU 时间（时钟滴答）与常驻内存（页）。
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Procs {
    pub(crate) clock_ticks: u64,
    pub(crate) page_size: u64,
    /// （pid，utime + stime，rss 页数，名称）
    pub(crate) list: Vec<(u32, u64, u64, String)>,
}

/// 两次采样之间 CPU 时间增长最多的进程。新出现的进程没有上一次的值，不计入。
fn top_processes(now: &Procs, before: &Procs, secs: f64) -> Vec<ProcessUsage> {
    let previous: std::collections::HashMap<u32, u64> = before
        .list
        .iter()
        .map(|(pid, t, _, _)| (*pid, *t))
        .collect();
    let ticks = now.clock_ticks.max(1) as f64;
    let mut list: Vec<ProcessUsage> = now
        .list
        .iter()
        .filter_map(|(pid, t, rss, name)| {
            let delta = t.checked_sub(*previous.get(pid)?)?;
            Some(ProcessUsage {
                pid: *pid,
                name: name.clone(),
                command: String::new(),
                cpu: (delta as f64 / ticks / secs) as f32,
                memory: rss * now.page_size,
            })
        })
        .collect();
    list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
    list.truncate(TOP_PROCESSES);
    list
}

/// 由相邻两次原始采样算出速率与占用。
#[derive(Default)]
pub(crate) struct Rates {
    prev: Option<(Instant, Raw)>,
}

impl Rates {
    pub(crate) fn next(&mut self, raw: Raw, at: Instant) -> Metrics {
        let mut m = Metrics {
            cpu: None,
            memory: raw.memory,
            swap: raw.swap,
            load: raw.load,
            uptime: raw.uptime,
            net_rx: None,
            net_tx: None,
            disks: raw.disks.clone(),
            processes: Vec::new(),
        };
        if let Some((then, prev)) = &self.prev {
            m.cpu = cpu_ratio(raw.cpu, prev.cpu);
            let secs = at.duration_since(*then).as_secs_f64();
            if let (Some((r1, x1)), Some((r0, x0))) = (raw.net, prev.net)
                && secs > 0.
            {
                // 计数器回绕或接口重建时差值为负，按 0 计
                m.net_rx = Some(r1.saturating_sub(r0) as f64 / secs);
                m.net_tx = Some(x1.saturating_sub(x0) as f64 / secs);
            }
            if let (Some(now), Some(before)) = (&raw.procs, &prev.procs)
                && secs > 0.
            {
                m.processes = top_processes(now, before, secs);
            }
        }
        self.prev = Some((at, raw));
        m
    }
}

/// 非回环网卡（是否物理网卡，收，发）的合计；没有网卡时为 `None`。
/// 透明代理（dae 等）、容器与虚拟机的流量会同时经过虚拟网卡和物理网卡，全部相加会重复计数，
/// 因此有物理网卡时只计物理网卡；没有时（OpenVZ、LXC 容器）退回全部计入。
fn net_sum(ifaces: &[(bool, u64, u64)]) -> Option<(u64, u64)> {
    let physical_only = ifaces.iter().any(|i| i.0);
    ifaces
        .iter()
        .filter(|i| i.0 || !physical_only)
        .map(|&(_, rx, tx)| (rx, tx))
        .reduce(|a, b| (a.0 + b.0, a.1 + b.1))
}

/// 两次 `/proc/stat` 采样（总计，空闲）之间的 CPU 占用比例。
fn cpu_ratio(now: Option<(u64, u64)>, before: Option<(u64, u64)>) -> Option<f32> {
    let ((t1, i1), (t0, i0)) = (now?, before?);
    if t1 <= t0 {
        return None;
    }
    let busy = (t1 - t0).saturating_sub(i1.saturating_sub(i0));
    Some((busy as f64 / (t1 - t0) as f64) as f32)
}
