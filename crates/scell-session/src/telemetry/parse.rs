//! 解析探针脚本输出的一帧（`@@begin` 与 `@@end` 之间）。

use std::time::Duration;

use super::{DiskUsage, Procs, Raw, Usage, net_sum};

/// 解析一帧；缺失或格式不对的项留空，不让整帧作废。
pub(crate) fn frame(text: &str) -> Raw {
    let mut raw = Raw::default();
    let mut section = "head";
    let mut head = 0;
    let mut mem = MemInfo::default();
    let mut physical: Vec<&str> = Vec::new();
    let mut net = Vec::new();
    // 同一设备挂在多处（btrfs 子卷、bind mount）只取第一个挂载点
    let mut devices: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("@@") {
            let mut words = header.split_whitespace();
            section = words.next().unwrap_or_default();
            if section == "net" {
                physical = words.collect();
            } else if section == "proc" {
                let mut num = || words.next().and_then(|v| v.parse::<u64>().ok());
                raw.procs = Some(Procs {
                    clock_ticks: num().unwrap_or(100),
                    page_size: num().unwrap_or(4096),
                    list: Vec::new(),
                });
            }
            continue;
        }
        match section {
            "head" => {
                match head {
                    0 => raw.uptime = uptime(line).unwrap_or_default(),
                    1 => raw.load = loadavg(line),
                    2 => raw.cpu = cpu(line),
                    _ => {}
                }
                head += 1;
            }
            "mem" => mem.line(line),
            "net" => {
                if let Some((name, rx, tx)) = net_line(line) {
                    net.push((physical.contains(&name), rx, tx));
                }
            }
            "proc" => {
                if let (Some(procs), Some(p)) = (raw.procs.as_mut(), proc_line(line)) {
                    procs.list.push(p);
                }
            }
            "df" => {
                if let Some((device, disk)) = df_line(line)
                    && !devices.contains(&device)
                {
                    devices.push(device);
                    raw.disks.push(disk);
                }
            }
            _ => {}
        }
    }
    raw.memory = Usage {
        total: mem.total,
        used: mem.total.saturating_sub(mem.available),
    };
    raw.swap = Usage {
        total: mem.swap_total,
        used: mem.swap_total.saturating_sub(mem.swap_free),
    };
    raw.net = net_sum(&net);
    raw
}

fn uptime(line: &str) -> Option<Duration> {
    let secs: f64 = line.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_secs_f64(secs.max(0.)))
}

fn loadavg(line: &str) -> Option<[f32; 3]> {
    let mut it = line.split_whitespace().map(|v| v.parse::<f32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// `/proc/stat` 的 `cpu` 汇总行：返回（总计，空闲 + iowait）。guest 时间已含在 user 中，不重复计。
pub(super) fn cpu(line: &str) -> Option<(u64, u64)> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = it.take(8).filter_map(|v| v.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((values.iter().sum(), idle))
}

#[derive(Default)]
struct MemInfo {
    total: u64,
    available: u64,
    swap_total: u64,
    swap_free: u64,
}

impl MemInfo {
    fn line(&mut self, line: &str) {
        let Some((key, rest)) = line.split_once(':') else {
            return;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
            return;
        };
        let bytes = kb * 1024;
        match key {
            "MemTotal" => self.total = bytes,
            "MemAvailable" => self.available = bytes,
            "SwapTotal" => self.swap_total = bytes,
            "SwapFree" => self.swap_free = bytes,
            _ => {}
        }
    }
}

/// `/proc/net/dev` 的一行：接口名后依次是 8 个接收计数与 8 个发送计数。回环接口不计。
fn net_line(line: &str) -> Option<(&str, u64, u64)> {
    let (name, rest) = line.split_once(':')?;
    let name = name.trim();
    if name == "lo" {
        return None;
    }
    let values: Vec<u64> = rest
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    Some((name, *values.first()?, *values.get(8)?))
}

/// 探针里 awk 输出的一行：`pid 累计滴答 rss页数 名称`（名称可含空格）。
fn proc_line(line: &str) -> Option<(u32, u64, u64, String)> {
    let mut it = line.splitn(4, ' ');
    let pid = it.next()?.parse().ok()?;
    let ticks = it.next()?.parse().ok()?;
    let rss = it.next()?.parse().ok()?;
    Some((pid, ticks, rss, it.next().unwrap_or_default().to_owned()))
}

/// `df -kP` 的一行：设备与用量。只保留块设备上的文件系统（排除 tmpfs、overlay 与 snap 的 loop 设备）。
fn df_line(line: &str) -> Option<(String, DiskUsage)> {
    let mut it = line.split_whitespace();
    let device = it.next()?;
    if !device.starts_with("/dev/") || device.starts_with("/dev/loop") {
        return None;
    }
    let total: u64 = it.next()?.parse().ok()?;
    let used: u64 = it.next()?.parse().ok()?;
    let _available = it.next()?;
    let _capacity = it.next()?;
    // 挂载点可能含空格，取剩余部分
    let mount = it.collect::<Vec<_>>().join(" ");
    if mount.is_empty() || total == 0 {
        return None;
    }
    Some((
        device.to_owned(),
        DiskUsage {
            mount,
            usage: Usage {
                used: used * 1024,
                total: total * 1024,
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::telemetry::Rates;

    const SAMPLE: &str = "\
12345.67 98765.43
0.52 0.40 0.35 2/345 6789
cpu  1000 20 300 8000 100 0 10 0 0 0
@@mem
MemTotal:        2000000 kB
MemFree:          500000 kB
MemAvailable:    1500000 kB
SwapTotal:       1000000 kB
SwapFree:         750000 kB
@@net eth0
    lo: 9999 10 0 0 0 0 0 0 9999 10 0 0 0 0 0 0
  eth0: 1000 10 0 0 0 0 0 0 2000 20 0 0 0 0 0 0
docker0: 700 7 0 0 0 0 0 0 800 8 0 0 0 0 0 0
vethab12: 800 8 0 0 0 0 0 0 700 7 0 0 0 0 0 0
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
udev               1000000       0   1000000       0% /dev
/dev/vda1         20000000 5000000  15000000      25% /
/dev/vda1         20000000 5000000  15000000      25% /home
/dev/loop0           60000   60000         0     100% /snap/core/1
/dev/vdb1          1000000  100000    900000      10% /mnt/my data
@@proc 100 4096
1 50 300 systemd
42 1000 2000 tmux: server
77 10 5 sh
";

    #[test]
    fn parses_a_frame() {
        let raw = frame(SAMPLE);
        assert_eq!(raw.uptime.as_secs(), 12345);
        assert_eq!(raw.load, Some([0.52, 0.40, 0.35]));
        assert_eq!(raw.cpu, Some((9430, 8100)));
        assert_eq!(raw.memory.total, 2_000_000 * 1024);
        assert_eq!(raw.memory.used, 500_000 * 1024);
        assert_eq!(raw.swap.used, 250_000 * 1024);
        assert_eq!(raw.net, Some((1000, 2000)));
        let mounts: Vec<&str> = raw.disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/mnt/my data"]);
        assert!((raw.disks[0].usage.ratio() - 0.25).abs() < 1e-6);
        let procs = raw.procs.unwrap();
        assert_eq!((procs.clock_ticks, procs.page_size), (100, 4096));
        assert_eq!(procs.list[1], (42, 1000, 2000, "tmux: server".into()));
    }

    #[test]
    fn counts_virtual_interfaces_without_physical_ones() {
        let raw = frame(
            "@@net\n    lo: 9 1 0 0 0 0 0 0 9 1 0 0 0 0 0 0\n\
             venet0: 300 3 0 0 0 0 0 0 400 4 0 0 0 0 0 0\n\
             tun0: 10 1 0 0 0 0 0 0 20 2 0 0 0 0 0 0\n",
        );
        assert_eq!(raw.net, Some((310, 420)));
    }

    #[test]
    fn rates_need_two_samples() {
        let mut rates = Rates::default();
        let t0 = Instant::now();
        let first = rates.next(frame(SAMPLE), t0);
        assert_eq!(first.cpu, None);
        assert_eq!(first.net_rx, None);

        let later = SAMPLE
            .replace("cpu  1000 20 300 8000", "cpu  1100 20 400 8800")
            .replace("eth0: 1000", "eth0: 5000")
            .replace("42 1000 2000", "42 1300 2000")
            .replace("77 10 5", "77 50 5");
        let second = rates.next(frame(&later), t0 + Duration::from_secs(2));
        // 总计增加 1000，其中空闲 800
        assert!((second.cpu.unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(second.net_rx, Some(2000.));
        assert_eq!(second.net_tx, Some(0.));
        // 2 秒内 300 滴答（100/秒）即 1.5 个核
        let top: Vec<(u32, f32)> = second.processes.iter().map(|p| (p.pid, p.cpu)).collect();
        assert_eq!(top, [(42, 1.5), (77, 0.2), (1, 0.)]);
        assert_eq!(second.processes[0].memory, 2000 * 4096);
    }

    #[test]
    fn tolerates_garbage() {
        let raw = frame("oops\n@@mem\nMemTotal: x kB\n@@net\nbad\n@@df\n/dev/sda1 a b c d /\n");
        assert_eq!(raw.memory.total, 0);
        assert!(raw.net.is_none());
        assert!(raw.disks.is_empty());
    }
}
