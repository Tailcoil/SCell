//! 窗口背景模糊。Wayland 上由这里按可见窗口框与圆角设置模糊区域（GPUI 只认 KDE 的旧协议，
//! 且会连阴影边距一起模糊）；X11、Windows 交给 GPUI。

use gpui_kit::{Window, WindowId};

#[cfg(target_os = "linux")]
mod wayland;

/// 按窗口材质申请或取消模糊。每帧调用，区域与开关都没变时不发请求。
pub(crate) fn sync(window: &Window, blur: bool) {
    #[cfg(target_os = "linux")]
    wayland::sync(window, blur);
    #[cfg(not(target_os = "linux"))]
    let _ = (window, blur);
}

/// 窗口关闭后释放它的模糊对象。
pub(crate) fn forget(window: WindowId) {
    #[cfg(target_os = "linux")]
    wayland::forget(window);
    #[cfg(not(target_os = "linux"))]
    let _ = window;
}
