//! 剪贴板。Linux 另有 PRIMARY 选区：选中即写入，中键粘贴；其他平台没有对应概念。

use gpui_kit::{App, ClipboardItem};

pub(crate) fn write(text: String, cx: &App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

pub(crate) fn read(cx: &App) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) fn write_primary(text: String, cx: &App) {
    cx.write_to_primary(ClipboardItem::new_string(text));
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub(crate) fn write_primary(_: String, _: &App) {}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) fn read_primary(cx: &App) -> Option<String> {
    cx.read_from_primary().and_then(|item| item.text())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub(crate) fn read_primary(_: &App) -> Option<String> {
    None
}
