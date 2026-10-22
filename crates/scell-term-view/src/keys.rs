use gpui_kit::Keystroke;
use scell_term::input::{Key, KeyEventKind, KeyInput, Modifiers};

/// 该按键产生的文本应交给输入法通道（`replace_text_in_range`）处理，而不是按键编码。
/// 与 GPUI 平台层的判定一致：仅 Shift 修饰且有可打印字符。
pub(crate) fn is_text_input(keystroke: &Keystroke, prefer_character_input: bool) -> bool {
    keystroke.key_char.is_some()
        && (prefer_character_input
            || keystroke
                .modifiers
                .is_subset_of(&gpui_kit::Modifiers::shift()))
}

pub(crate) fn to_key_input(keystroke: &Keystroke, kind: KeyEventKind) -> Option<KeyInput> {
    let key = match keystroke.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        k => {
            if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                Key::F(n)
            } else {
                let mut chars = k.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Char(c),
                    _ => return None,
                }
            }
        }
    };
    Some(KeyInput {
        key,
        mods: to_modifiers(&keystroke.modifiers),
        text: keystroke.key_char.clone(),
        kind,
    })
}

pub(crate) fn to_modifiers(m: &gpui_kit::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        ctrl: m.control,
        alt: m.alt,
        super_key: m.platform,
    }
}
