//! 把与 UI 框架无关的输入事件编码为发往程序的字节。

mod keyboard;
mod kitty;
mod mouse;
mod paste;

pub use keyboard::{Key, KeyEventKind, KeyInput, Modifiers, encode_key};
pub use mouse::{MouseAction, MouseButton, MouseInput, encode_mouse};
pub use paste::encode_paste;

/// 编码输入时需要的终端模式快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputMode {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    pub alternate_scroll: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// kitty 键盘协议的渐进增强标志位，0 表示未启用。
    pub kitty_flags: u32,
}

impl InputMode {
    /// 程序是否接管了鼠标。
    pub fn mouse_reporting(&self) -> bool {
        self.mouse != MouseMode::Off
    }

    /// kitty 协议要求上报按键重复与释放。
    pub fn reports_key_events(&self) -> bool {
        self.kitty_flags & kitty::EVENT_TYPES != 0
    }

    /// kitty 协议要求文本键也以转义序列发送。
    pub fn all_keys_as_escapes(&self) -> bool {
        self.kitty_flags & kitty::ALL_AS_ESCAPES != 0
    }
}

/// 程序请求的鼠标上报范围（DECSET 1000 / 1002 / 1003）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    Off,
    /// 只报告按下、释放与滚轮。
    Click,
    /// 另外报告按住按钮时的移动。
    Drag,
    /// 报告所有移动。
    Motion,
}

/// 鼠标上报的坐标编码（默认 / DECSET 1005 / DECSET 1006）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}
