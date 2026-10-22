//! 窗口外框控件：拖动区与最小化/最大化/关闭按钮，放在侧栏顶部，右侧完全留给终端。
//!
//! gpui-component 的 `TitleBar` 只能横向铺满一整行，折叠为图标栏时放不下，所以这里按它的平台行为另做一份：
//! Windows 交给系统命中测试（保留贴靠布局），Linux 仅在客户端装饰时绘制并自行处理点击。

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, WindowControlArea, div, prelude::FluentBuilder,
};

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Close,
}

/// 是否需要自绘窗口按钮：服务端装饰时窗口管理器已经画了，macOS 用系统按钮。
pub(super) fn draws_controls(window: &Window) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    #[cfg(target_os = "linux")]
    if !matches!(
        window.window_decorations(),
        gpui_kit::Decorations::Client { .. }
    ) {
        return false;
    }
    let _ = window;
    true
}

/// 窗口按钮，顺序为最小化、最大化、关闭。
pub(super) fn window_controls(window: &Window, cx: &App) -> Option<AnyElement> {
    if !draws_controls(window) {
        return None;
    }
    let supported = window.window_controls();
    let mut controls = Vec::with_capacity(3);
    if supported.minimize {
        controls.push(Control::Minimize);
    }
    if supported.maximize {
        controls.push(Control::Maximize);
    }
    controls.push(Control::Close);
    let maximized = window.is_maximized();
    Some(
        div()
            .flex()
            .flex_row()
            .gap_0p5()
            .flex_none()
            .children(controls.into_iter().map(|c| button(c, maximized, cx)))
            .into_any_element(),
    )
}

fn button(control: Control, maximized: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (id, icon, area) = match control {
        Control::Minimize => (
            "window-minimize",
            IconName::WindowMinimize,
            WindowControlArea::Min,
        ),
        Control::Maximize if maximized => (
            "window-restore",
            IconName::WindowRestore,
            WindowControlArea::Max,
        ),
        Control::Maximize => (
            "window-maximize",
            IconName::WindowMaximize,
            WindowControlArea::Max,
        ),
        Control::Close => (
            "window-close",
            IconName::WindowClose,
            WindowControlArea::Close,
        ),
    };
    // 关闭只把图标染成危险色、底色轻微着色，不整块填满
    let (hover_bg, hover_fg) = match control {
        Control::Close => (theme.danger.opacity(0.15), theme.danger),
        _ => (theme.secondary_hover, theme.secondary_foreground),
    };
    div()
        .id(id)
        .size(crate::ui::metrics::CHROME_BUTTON)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .text_color(theme.muted_foreground)
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .when(cfg!(windows), |this| this.window_control_area(area))
        .when(cfg!(not(windows)), |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                // 不让按下事件落到拖动区，否则会开始移动窗口
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match control {
                    Control::Minimize => window.minimize_window(),
                    Control::Maximize => window.zoom_window(),
                    // 交给工作区：有多条活跃连接时先确认
                    Control::Close => {
                        window.dispatch_action(Box::new(crate::actions::CloseWindow), cx)
                    }
                }
            })
        })
        .child(Icon::new(icon).small())
        .into_any_element()
}

/// 可拖动移动窗口的区域：双击最大化，右键打开窗口菜单（Linux）。
pub(super) fn drag_area(id: &'static str) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .when(cfg!(windows), |this| {
            this.window_control_area(WindowControlArea::Drag)
        })
        .when(cfg!(not(windows)), |this| {
            this.on_mouse_down(MouseButton::Left, |e, window, _| {
                if e.click_count >= 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .on_mouse_down(MouseButton::Right, |e, window, _| {
                window.show_window_menu(e.position)
            })
        })
}
