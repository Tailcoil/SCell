//! xterm 鼠标上报编码（X10 / UTF-8 / SGR）。

use super::{InputMode, Modifiers, MouseEncoding, MouseMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// 未按任何按钮时的移动（仅 1003 模式上报）。
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub button: MouseButton,
    pub action: MouseAction,
    /// 视口内从 0 开始的单元格坐标。
    pub col: usize,
    pub row: usize,
    pub mods: Modifiers,
}

/// 当前模式不需要上报，或坐标超出默认编码的上限（223）时返回 `None`。
pub fn encode_mouse(input: &MouseInput, mode: InputMode) -> Option<Vec<u8>> {
    let reported = match (mode.mouse, input.action, input.button) {
        (MouseMode::Off, ..) => false,
        (_, MouseAction::Press | MouseAction::Release, _) => true,
        (MouseMode::Click, MouseAction::Motion, _) => false,
        (MouseMode::Drag, MouseAction::Motion, MouseButton::None) => false,
        (MouseMode::Drag | MouseMode::Motion, MouseAction::Motion, _) => true,
    };
    // 滚轮没有释放事件
    let wheel = matches!(input.button, MouseButton::WheelUp | MouseButton::WheelDown);
    if !reported || (wheel && input.action == MouseAction::Release) {
        return None;
    }

    let mut code: u32 = match input.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if input.action == MouseAction::Motion {
        code += 32;
    }
    let m = input.mods;
    code += 4 * m.shift as u32 + 8 * m.alt as u32 + 16 * m.ctrl as u32;
    let (x, y) = (input.col as u32 + 1, input.row as u32 + 1);

    if mode.mouse_encoding == MouseEncoding::Sgr {
        let suffix = if input.action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes());
    }

    // 旧编码无法区分释放的是哪个按钮，统一报 3
    if input.action == MouseAction::Release {
        code = (code & !0b11) | 3;
    }
    let mut out = b"\x1b[M".to_vec();
    out.push((code + 32) as u8);
    for v in [x, y] {
        match mode.mouse_encoding {
            MouseEncoding::Utf8 => {
                let c = char::from_u32(v + 32)?;
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            _ => out.push(u8::try_from(v + 32).ok().filter(|&b| b < 255)?),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(button: MouseButton, action: MouseAction, col: usize, row: usize) -> MouseInput {
        MouseInput {
            button,
            action,
            col,
            row,
            mods: Modifiers::default(),
        }
    }

    fn mode(mouse: MouseMode, mouse_encoding: MouseEncoding) -> InputMode {
        InputMode {
            mouse,
            mouse_encoding,
            ..Default::default()
        }
    }

    #[test]
    fn sgr_press_release_and_wheel() {
        let m = mode(MouseMode::Click, MouseEncoding::Sgr);
        let press = input(MouseButton::Left, MouseAction::Press, 9, 4);
        assert_eq!(encode_mouse(&press, m).unwrap(), b"\x1b[<0;10;5M");
        let release = input(MouseButton::Right, MouseAction::Release, 0, 0);
        assert_eq!(encode_mouse(&release, m).unwrap(), b"\x1b[<2;1;1m");
        let wheel = input(MouseButton::WheelDown, MouseAction::Press, 1, 1);
        assert_eq!(encode_mouse(&wheel, m).unwrap(), b"\x1b[<65;2;2M");
    }

    #[test]
    fn motion_follows_mode() {
        let drag = input(MouseButton::Left, MouseAction::Motion, 0, 0);
        let hover = input(MouseButton::None, MouseAction::Motion, 0, 0);
        let click = mode(MouseMode::Click, MouseEncoding::Sgr);
        let button = mode(MouseMode::Drag, MouseEncoding::Sgr);
        let any = mode(MouseMode::Motion, MouseEncoding::Sgr);
        assert_eq!(encode_mouse(&drag, click), None);
        assert_eq!(encode_mouse(&drag, button).unwrap(), b"\x1b[<32;1;1M");
        assert_eq!(encode_mouse(&hover, button), None);
        assert_eq!(encode_mouse(&hover, any).unwrap(), b"\x1b[<35;1;1M");
    }

    #[test]
    fn default_encoding_and_limits() {
        let m = mode(MouseMode::Click, MouseEncoding::Default);
        let mut press = input(MouseButton::Middle, MouseAction::Press, 0, 2);
        press.mods.ctrl = true;
        assert_eq!(encode_mouse(&press, m).unwrap(), b"\x1b[M\x31\x21\x23");
        let release = input(MouseButton::Middle, MouseAction::Release, 0, 0);
        assert_eq!(encode_mouse(&release, m).unwrap(), b"\x1b[M\x23\x21\x21");
        let far = input(MouseButton::Left, MouseAction::Press, 300, 0);
        assert_eq!(encode_mouse(&far, m), None);

        let utf8 = mode(MouseMode::Click, MouseEncoding::Utf8);
        let out = encode_mouse(&far, utf8).unwrap();
        assert_eq!(&out[..4], b"\x1b[M\x20");
        assert_eq!(std::str::from_utf8(&out[4..]).unwrap(), "\u{14d}!");
    }

    #[test]
    fn off_reports_nothing() {
        let press = input(MouseButton::Left, MouseAction::Press, 0, 0);
        assert_eq!(encode_mouse(&press, InputMode::default()), None);
    }
}
