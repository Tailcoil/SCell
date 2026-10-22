//! 浮层：查找条、命令面板、粘贴确认、通知。Esc 关闭后焦点回到原窗格。

use gpui_kit::App;
use gpui_kit::component::WindowExt;
use gpui_kit::component::notification::Notification;

mod find;
mod palette;
pub(crate) mod paste;

pub(crate) use find::{FindBar, FindEvent};
pub(crate) use palette::{Palette, PaletteEvent, PaletteMode};

/// 显示在当前窗口；没有窗口时丢弃。用于后台任务结束后没有窗口句柄的场合。
pub(crate) fn show_notification(note: Notification, cx: &mut App) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    let _ = window.update(cx, |_, window, cx| window.push_notification(note, cx));
}
