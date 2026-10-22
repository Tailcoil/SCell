//! 按键编码：程序开启 kitty 键盘协议时按该协议，否则按 xterm 传统方式。

use super::{InputMode, kitty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// 可打印键，取未经 Shift 变换的基础字符（如 Ctrl+Shift+A 为 'a'）。
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl Modifiers {
    /// xterm 修饰参数：1 + Shift + 2·Alt + 4·Ctrl。
    fn xterm_param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.ctrl || self.alt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyEventKind {
    #[default]
    Press,
    /// 按住不放产生的自动重复。
    Repeat,
    /// 只有 kitty 协议开启事件类型上报时才会发送。
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Modifiers,
    /// 键盘布局产生的文本（已应用 Shift），可打印键优先使用它。
    pub text: Option<String>,
    pub kind: KeyEventKind,
}

/// 返回 `None` 表示该按键不向程序发送任何内容（如单独的 Super 组合、传统模式下的释放）。
pub fn encode_key(input: &KeyInput, mode: InputMode) -> Option<Vec<u8>> {
    if mode.kitty_flags & (kitty::DISAMBIGUATE | kitty::ALL_AS_ESCAPES) != 0 {
        return kitty::encode(input, mode);
    }
    if input.kind == KeyEventKind::Release {
        return None;
    }
    legacy(input, mode)
}

/// xterm 传统编码，不区分按下与重复。
pub(super) fn legacy(input: &KeyInput, mode: InputMode) -> Option<Vec<u8>> {
    let mods = input.mods;
    if mods.super_key {
        return None;
    }
    let alt_prefix = |mut bytes: Vec<u8>| {
        if mods.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };

    let bytes = match input.key {
        Key::Char(c) => {
            if mods.ctrl {
                alt_prefix(vec![ctrl_byte(c)?])
            } else {
                let text = match &input.text {
                    Some(t) if !t.is_empty() => t.clone(),
                    _ => c.to_string(),
                };
                alt_prefix(text.into_bytes())
            }
        }
        Key::Enter => alt_prefix(b"\r".to_vec()),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => alt_prefix(b"\t".to_vec()),
        Key::Backspace if mods.ctrl => alt_prefix(vec![0x08]),
        Key::Backspace => alt_prefix(vec![0x7f]),
        Key::Escape => alt_prefix(vec![0x1b]),
        Key::Up => cursor_key(b'A', mods, mode),
        Key::Down => cursor_key(b'B', mods, mode),
        Key::Right => cursor_key(b'C', mods, mode),
        Key::Left => cursor_key(b'D', mods, mode),
        Key::Home => cursor_key(b'H', mods, mode),
        Key::End => cursor_key(b'F', mods, mode),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::F(n @ 1..=4) => ss3_or_csi(b"PQRS"[n as usize - 1], mods),
        Key::F(n @ 5..=12) => tilde_key([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5], mods),
        Key::F(_) => return None,
    };
    Some(bytes)
}

/// Ctrl 组合对应的 C0 控制码。
fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

fn cursor_key(final_byte: u8, mods: Modifiers, mode: InputMode) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[1;{}{}", mods.xterm_param(), final_byte as char).into_bytes()
    } else if mode.app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn ss3_or_csi(final_byte: u8, mods: Modifiers) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[1;{}{}", mods.xterm_param(), final_byte as char).into_bytes()
    } else {
        vec![0x1b, b'O', final_byte]
    }
}

fn tilde_key(code: u8, mods: Modifiers) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{code};{}~", mods.xterm_param()).into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: Modifiers, mode: InputMode) -> Vec<u8> {
        encode_key(
            &KeyInput {
                key,
                mods,
                text: None,
                kind: KeyEventKind::Press,
            },
            mode,
        )
        .expect("有输出")
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        super_key: false,
    };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };

    #[test]
    fn control_codes() {
        let m = InputMode::default();
        assert_eq!(enc(Key::Char('c'), CTRL, m), [0x03]);
        assert_eq!(enc(Key::Char('['), CTRL, m), [0x1b]);
        assert_eq!(enc(Key::Char(' '), CTRL, m), [0x00]);
        assert_eq!(
            enc(Key::Char('b'), Modifiers { alt: true, ..CTRL }, m),
            [0x1b, 0x02]
        );
    }

    #[test]
    fn printable_prefers_layout_text() {
        let input = KeyInput {
            key: Key::Char('1'),
            mods: SHIFT,
            text: Some("!".into()),
            kind: KeyEventKind::Press,
        };
        assert_eq!(
            encode_key(&input, InputMode::default()),
            Some(b"!".to_vec())
        );
        assert_eq!(enc(Key::Char('x'), ALT, InputMode::default()), b"\x1bx");
    }

    #[test]
    fn cursor_keys_follow_app_mode_and_modifiers() {
        let normal = InputMode::default();
        let app = InputMode {
            app_cursor: true,
            ..normal
        };
        assert_eq!(enc(Key::Up, NONE, normal), b"\x1b[A");
        assert_eq!(enc(Key::Up, NONE, app), b"\x1bOA");
        assert_eq!(enc(Key::Left, CTRL, app), b"\x1b[1;5D");
        assert_eq!(enc(Key::Delete, SHIFT, normal), b"\x1b[3;2~");
    }

    #[test]
    fn function_keys() {
        let m = InputMode::default();
        assert_eq!(enc(Key::F(1), NONE, m), b"\x1bOP");
        assert_eq!(enc(Key::F(5), NONE, m), b"\x1b[15~");
        assert_eq!(enc(Key::F(12), CTRL, m), b"\x1b[24;5~");
        assert_eq!(enc(Key::Tab, SHIFT, m), b"\x1b[Z");
    }

    #[test]
    fn super_combos_are_not_sent() {
        let input = KeyInput {
            key: Key::Char('c'),
            mods: Modifiers {
                super_key: true,
                ..NONE
            },
            text: None,
            kind: KeyEventKind::Press,
        };
        assert_eq!(encode_key(&input, InputMode::default()), None);
    }
}
