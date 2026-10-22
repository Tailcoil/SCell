//! 混合显卡笔记本上不让 GPUI 碰 NVIDIA 独显。
//!
//! GPUI 选卡时枚举全部 Vulkan 与 EGL 驱动，NVIDIA 驱动一加载就会唤醒处于运行时 D3 的独显（启动慢上数百毫秒到数秒），
//! 并且一直持有 `/dev/nvidia*`，独显在程序退出前无法再休眠。混成器跑在核显上时最终选中的也是核显，独显纯属白唤醒。

#[cfg(target_os = "linux")]
/// 设置后不做屏蔽，与 GPUI 默认行为一致。
const OPT_OUT: &str = "SCELL_DISCRETE_GPU";

#[cfg(target_os = "linux")]
/// 用户已自行指定驱动或 PRIME 卸载时不干预。
const USER_OVERRIDES: &[&str] = &[
    OPT_OUT,
    "ZED_DEVICE_ID",
    "VK_ICD_FILENAMES",
    "VK_DRIVER_FILES",
    "VK_LOADER_DRIVERS_SELECT",
    "VK_LOADER_DRIVERS_DISABLE",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "__NV_PRIME_RENDER_OFFLOAD",
    "DRI_PRIME",
];

#[cfg(target_os = "linux")]
const NVIDIA: &str = "0x10de";

#[cfg(target_os = "linux")]
const EGL_VENDOR_DIRS: &[&str] = &["/etc/glvnd/egl_vendor.d", "/usr/share/glvnd/egl_vendor.d"];

/// 必须在创建任何线程之前调用（修改环境变量）。返回设置了的变量名，这些变量不应传给子进程。
#[cfg(target_os = "linux")]
pub(crate) fn avoid_discrete_gpu() -> Vec<&'static str> {
    if USER_OVERRIDES.iter().any(|v| std::env::var_os(v).is_some()) || !hybrid_with_nvidia() {
        return Vec::new();
    }
    let egl: Vec<String> = EGL_VENDOR_DIRS
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !p.to_string_lossy().to_lowercase().contains("nvidia")
        })
        .map(|p| p.display().to_string())
        .collect();
    // SAFETY: 在 main 的第一行调用，此时进程只有主线程，没有并发读写环境变量
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("VK_LOADER_DRIVERS_DISABLE", "*nvidia*");
        if egl.is_empty() {
            return vec!["VK_LOADER_DRIVERS_DISABLE"];
        }
        std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", egl.join(":"));
    }
    vec![
        "VK_LOADER_DRIVERS_DISABLE",
        "__EGL_VENDOR_LIBRARY_FILENAMES",
    ]
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn avoid_discrete_gpu() -> Vec<&'static str> {
    Vec::new()
}

/// 开机主显卡不是 NVIDIA，另有一块 NVIDIA 显卡：典型的核显 + 独显笔记本。
#[cfg(target_os = "linux")]
fn hybrid_with_nvidia() -> bool {
    let Ok(devices) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return false;
    };
    let read = |dir: &std::path::Path, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let (mut primary_other, mut secondary_nvidia) = (false, false);
    for dir in devices.filter_map(|e| e.ok().map(|e| e.path())) {
        // 0x03xxxx：显示控制器
        if !read(&dir, "class").starts_with("0x03") {
            continue;
        }
        let nvidia = read(&dir, "vendor") == NVIDIA;
        let primary = read(&dir, "boot_vga") == "1";
        primary_other |= primary && !nvidia;
        secondary_nvidia |= !primary && nvidia;
    }
    primary_other && secondary_nvidia
}
