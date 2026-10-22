//! 鼠标：程序开启鼠标上报时转发给程序，否则用于选择与粘贴。按住 Shift 始终走本地选择。

use gpui_kit::{
    Context, MouseButton as GpuiButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollWheelEvent, Window,
};
use scell_term::input::{
    Key, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput, encode_key, encode_mouse,
    encode_paste,
};
use scell_term::{Link, SelectionKind, ViewportPoint};

use crate::TerminalView;
use crate::clipboard;
use crate::keys::to_modifiers;
use crate::scrollbar::ScrollbarState;

/// 视图里与鼠标相关的状态。
#[derive(Default)]
pub(crate) struct MouseState {
    /// 内容区左上角（窗口坐标），由 prepaint 更新。
    pub(crate) origin: Point<Pixels>,
    /// 正在拖动选择。
    selecting: bool,
    /// 已上报给程序、尚未释放的按钮。
    reported: Option<MouseButton>,
    /// 上一次上报的单元格，移动未跨格时不重复上报。
    last_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    /// 按住 Ctrl 时鼠标下的链接。
    pub(crate) link: Option<Link>,
    /// 最近一次鼠标位置，按下或松开 Ctrl 而鼠标未动时据此更新链接。
    last_position: Option<Point<Pixels>>,
    pub(crate) scrollbar: ScrollbarState,
}

impl TerminalView {
    fn cell_at(&self, position: Point<Pixels>) -> Option<ViewportPoint> {
        let (metrics, _) = self.metrics?;
        let (cols, rows) = (self.snapshot.cols, self.snapshot.rows);
        if cols == 0 || rows == 0 {
            return None;
        }
        let x = ((position.x - self.mouse.origin.x) / metrics.width).max(0.);
        let y = ((position.y - self.mouse.origin.y) / metrics.height).max(0.);
        let col = (x as usize).min(cols - 1);
        Some(ViewportPoint {
            row: (y as usize).min(rows - 1),
            col,
            // 越过行尾时整格都算选中
            right_half: x - col as f32 >= 0.5,
        })
    }

    /// 程序是否接管当前鼠标事件。
    fn reporting(&self, mods: &gpui_kit::Modifiers) -> bool {
        !mods.shift && self.terminal.input_mode().mouse_reporting()
    }

    fn report(
        &mut self,
        button: MouseButton,
        action: MouseAction,
        at: ViewportPoint,
        mods: Modifiers,
    ) {
        let input = MouseInput {
            button,
            action,
            col: at.col,
            row: at.row,
            mods,
        };
        if let Some(bytes) = encode_mouse(&input, self.terminal.input_mode()) {
            self.io.write(bytes);
        }
    }

    pub(crate) fn mouse_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if e.button == GpuiButton::Left && self.scrollbar_down(e.position, cx) {
            return;
        }
        let Some(at) = self.cell_at(e.position) else {
            return;
        };
        if e.button == GpuiButton::Left && e.modifiers.secondary() {
            self.update_link(Some(e.position), true, cx);
            if let Some(link) = &self.mouse.link {
                cx.open_url(&link.uri);
                return;
            }
        }
        if self.reporting(&e.modifiers) {
            if let Some(button) = to_button(e.button) {
                self.report(button, MouseAction::Press, at, to_modifiers(&e.modifiers));
                self.mouse.reported = Some(button);
                self.mouse.last_cell = Some((at.row, at.col));
            }
            return;
        }
        match e.button {
            GpuiButton::Left => {
                let kind = match e.click_count {
                    2 => SelectionKind::Word,
                    n if n >= 3 => SelectionKind::Line,
                    _ if e.modifiers.alt => SelectionKind::Block,
                    _ => SelectionKind::Simple,
                };
                if e.modifiers.shift
                    && e.click_count == 1
                    && self.terminal.selection_text().is_some()
                {
                    self.terminal.extend_selection(at);
                } else {
                    self.terminal.start_selection(kind, at);
                }
                self.mouse.selecting = true;
                cx.notify();
            }
            GpuiButton::Middle => {
                if let Some(text) = clipboard::read_primary(cx) {
                    self.request_paste(text, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn mouse_move(
        &mut self,
        e: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrollbar_move(e.position, cx) {
            return;
        }
        self.update_link(Some(e.position), e.modifiers.secondary(), cx);
        let Some(at) = self.cell_at(e.position) else {
            return;
        };
        if self.mouse.selecting {
            self.terminal.extend_selection(at);
            cx.notify();
            return;
        }
        if !self.reporting(&e.modifiers) || self.mouse.last_cell == Some((at.row, at.col)) {
            return;
        }
        self.mouse.last_cell = Some((at.row, at.col));
        let button = self.mouse.reported.unwrap_or(MouseButton::None);
        self.report(button, MouseAction::Motion, at, to_modifiers(&e.modifiers));
    }

    pub(crate) fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_up() {
            return;
        }
        if let Some(button) = self.mouse.reported.take() {
            if let Some(at) = self.cell_at(e.position) {
                self.report(button, MouseAction::Release, at, to_modifiers(&e.modifiers));
            }
            return;
        }
        if std::mem::take(&mut self.mouse.selecting)
            && let Some(text) = self.terminal.selection_text()
        {
            if self.copy_on_select {
                clipboard::write(text.clone(), cx);
            }
            clipboard::write_primary(text, cx);
        }
    }

    pub(crate) fn scroll(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((metrics, _)) = self.metrics else {
            return;
        };
        let height = metrics.height.as_f32();
        let delta = e.delta.pixel_delta(metrics.height).y.as_f32() + self.mouse.scroll_remainder;
        let lines = (delta / height).trunc();
        self.mouse.scroll_remainder = delta - lines * height;
        let lines = lines as i32;
        if lines == 0 {
            return;
        }
        let count = lines.unsigned_abs() as usize;
        let mode = self.terminal.input_mode();
        if self.reporting(&e.modifiers) {
            if let Some(at) = self.cell_at(e.position) {
                let button = if lines > 0 {
                    MouseButton::WheelUp
                } else {
                    MouseButton::WheelDown
                };
                for _ in 0..count {
                    self.report(button, MouseAction::Press, at, to_modifiers(&e.modifiers));
                }
            }
        } else if mode.alt_screen && mode.alternate_scroll {
            // 备用屏下滚轮转为方向键，less、vim 等程序可直接滚动
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let input = KeyInput {
                key,
                mods: Modifiers::default(),
                text: None,
                kind: Default::default(),
            };
            if let Some(bytes) = encode_key(&input, mode) {
                self.io.write(bytes.repeat(count));
            }
        } else {
            self.terminal.scroll(lines);
            cx.notify();
        }
    }

    /// 按住 Ctrl（macOS 为 Cmd）时识别鼠标下的链接，变化时重绘以更新下划线。
    pub(crate) fn update_link(
        &mut self,
        position: Option<Point<Pixels>>,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if position.is_some() {
            self.mouse.last_position = position;
        }
        let link = match self.mouse.last_position {
            Some(p) if active && !self.mouse.selecting => {
                self.cell_at(p).and_then(|at| self.terminal.link_at(at))
            }
            _ => None,
        };
        if link != self.mouse.link {
            self.mouse.link = link;
            cx.notify();
        }
    }

    pub(crate) fn paste_text(&mut self, text: &str) {
        let bytes = encode_paste(text, self.terminal.input_mode());
        self.send(bytes);
    }
}

fn to_button(button: GpuiButton) -> Option<MouseButton> {
    match button {
        GpuiButton::Left => Some(MouseButton::Left),
        GpuiButton::Middle => Some(MouseButton::Middle),
        GpuiButton::Right => Some(MouseButton::Right),
        _ => None,
    }
}
