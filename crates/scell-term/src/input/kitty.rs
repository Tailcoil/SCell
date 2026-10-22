//! kitty 键盘协议（渐进增强）编码。规范见 <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>。
//!
//! 未实现：小键盘键的独立编码、单独按下修饰键的上报（宿主拿不到这类事件）。

use super::InputMode;
use super::keyboard::{Key, KeyEventKind, KeyInput, Modifiers, legacy};

pub(super) const DISAMBIGUATE: u32 = 1;
pub(super) const EVENT_TYPES: u32 = 2;
pub(super) const ALTERNATE_KEYS: u32 = 4;
pub(super) const ALL_AS_ESCAPES: u32 = 8;
pub(super) const ASSOCIATED_TEXT: u32 = 16;

pub(super) fn encode(input: &KeyInput, mode: InputMode) -> Option<Vec<u8>> {
    let flags = mode.kitty_flags;
    let mods = input.mods;
    let all = flags & ALL_AS_ESCAPES != 0;
    let release = input.kind == KeyEventKind::Release;
    if release && flags & EVENT_TYPES == 0 {
        return None;
    }
    // 不加修饰（或只加 Shift）的可打印键在未要求全部转义时照常发文本
    let text = input.text.as_deref().filter(|t| !t.is_empty());
    let plain_text = matches!(input.key, Key::Char(_))
        && !mods.ctrl
        && !mods.alt
        && !mods.super_key
        && text.is_some();
    if !all && plain_text {
        return (!release).then(|| text.unwrap_or_default().as_bytes().to_vec());
    }
    // Enter、Tab、Backspace 未加修饰时保持传统编码，释放事件也只在全部转义时上报
    if !all && !mods.any_with_super() && matches!(input.key, Key::Enter | Key::Tab | Key::Backspace)
    {
        return (!release).then(|| legacy(input, mode)).flatten();
    }

    let event = match input.kind {
        KeyEventKind::Press => None,
        _ if flags & EVENT_TYPES == 0 => None,
        KeyEventKind::Repeat => Some(2),
        KeyEventKind::Release => Some(3),
    };
    let modifier = mods.kitty_param();
    let (number, terminator) = match input.key {
        Key::Char(c) => (c.to_lowercase().next().unwrap_or(c) as u32, 'u'),
        Key::Escape => (27, 'u'),
        Key::Enter => (13, 'u'),
        Key::Tab => (9, 'u'),
        Key::Backspace => (127, 'u'),
        Key::Up => (1, 'A'),
        Key::Down => (1, 'B'),
        Key::Right => (1, 'C'),
        Key::Left => (1, 'D'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::F(1) => (1, 'P'),
        Key::F(2) => (1, 'Q'),
        Key::F(3) => (13, '~'),
        Key::F(4) => (1, 'S'),
        Key::F(n @ 5..=12) => ([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5], '~'),
        Key::F(_) => return None,
    };
    // 无修饰、无事件类型的功能键与传统编码相同，交给传统编码以保留应用光标模式
    if terminator != 'u' && modifier == 1 && event.is_none() {
        return legacy(input, mode);
    }

    let mut key = number.to_string();
    if terminator == 'u'
        && flags & ALTERNATE_KEYS != 0
        && mods.shift
        && let Some(shifted) = text.and_then(single_char).filter(|s| *s as u32 != number)
    {
        key.push_str(&format!(":{}", shifted as u32));
    }
    let mut params = String::new();
    if modifier != 1 || event.is_some() {
        params.push_str(&format!(";{modifier}"));
        if let Some(e) = event {
            params.push_str(&format!(":{e}"));
        }
    }
    if terminator == 'u'
        && flags & ASSOCIATED_TEXT != 0
        && !release
        && let Some(t) = text.filter(|t| t.chars().all(|c| !c.is_control()))
    {
        if params.is_empty() {
            params.push_str(";1");
        }
        let codes: Vec<String> = t.chars().map(|c| (c as u32).to_string()).collect();
        params.push_str(&format!(";{}", codes.join(":")));
    }
    let body = if terminator == 'u' || terminator == '~' {
        format!("{key}{params}")
    } else if params.is_empty() {
        // CSI A 形式省略参数
        String::new()
    } else {
        format!("1{params}")
    };
    Some(format!("\x1b[{body}{terminator}").into_bytes())
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

impl Modifiers {
    /// kitty 修饰参数：1 + Shift + 2·Alt + 4·Ctrl + 8·Super。
    fn kitty_param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8 + 8 * self.super_key as u8
    }

    fn any_with_super(self) -> bool {
        self.shift || self.ctrl || self.alt || self.super_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::encode_key;

    fn mode(flags: u32) -> InputMode {
        InputMode {
            kitty_flags: flags,
            ..InputMode::default()
        }
    }

    fn key(key: Key, mods: Modifiers, text: Option<&str>, kind: KeyEventKind) -> KeyInput {
        KeyInput {
            key,
            mods,
            text: text.map(str::to_owned),
            kind,
        }
    }

    fn enc(input: KeyInput, flags: u32) -> Option<String> {
        encode_key(&input, mode(flags)).map(|b| String::from_utf8(b).unwrap())
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        super_key: false,
    };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    use KeyEventKind::{Press, Release, Repeat};

    #[test]
    fn disambiguate() {
        let f = DISAMBIGUATE;
        assert_eq!(
            enc(key(Key::Escape, NONE, None, Press), f).unwrap(),
            "\x1b[27u"
        );
        assert_eq!(
            enc(key(Key::Char('c'), CTRL, None, Press), f).unwrap(),
            "\x1b[99;5u"
        );
        assert_eq!(
            enc(key(Key::Char('a'), NONE, Some("a"), Press), f).unwrap(),
            "a"
        );
        assert_eq!(
            enc(key(Key::Char('a'), SHIFT, Some("A"), Press), f).unwrap(),
            "A"
        );
        assert_eq!(enc(key(Key::Enter, NONE, None, Press), f).unwrap(), "\r");
        assert_eq!(
            enc(key(Key::Enter, SHIFT, None, Press), f).unwrap(),
            "\x1b[13;2u"
        );
        assert_eq!(enc(key(Key::Up, NONE, None, Press), f).unwrap(), "\x1b[A");
        assert_eq!(
            enc(key(Key::Up, CTRL, None, Press), f).unwrap(),
            "\x1b[1;5A"
        );
        // 无修饰的 F3 沿用 SS3 R；带修饰时用 CSI 13 ~，避开与光标位置报告 CSI R 的冲突
        assert_eq!(enc(key(Key::F(3), NONE, None, Press), f).unwrap(), "\x1bOR");
        assert_eq!(
            enc(key(Key::F(3), CTRL, None, Press), f).unwrap(),
            "\x1b[13;5~"
        );
        // 未开启事件类型时不上报释放
        assert_eq!(enc(key(Key::Escape, NONE, None, Release), f), None);
    }

    #[test]
    fn event_types() {
        let f = DISAMBIGUATE | EVENT_TYPES;
        assert_eq!(
            enc(key(Key::Escape, NONE, None, Repeat), f).unwrap(),
            "\x1b[27;1:2u"
        );
        assert_eq!(
            enc(key(Key::Char('c'), CTRL, None, Release), f).unwrap(),
            "\x1b[99;5:3u"
        );
        assert_eq!(
            enc(key(Key::Up, NONE, None, Release), f).unwrap(),
            "\x1b[1;1:3A"
        );
        // 传统编码的文本键与 Enter 不上报释放
        assert_eq!(enc(key(Key::Enter, NONE, None, Release), f), None);
    }

    #[test]
    fn all_keys_with_alternates_and_text() {
        let f = DISAMBIGUATE | ALL_AS_ESCAPES | ALTERNATE_KEYS | ASSOCIATED_TEXT;
        assert_eq!(
            enc(key(Key::Char('a'), NONE, Some("a"), Press), f).unwrap(),
            "\x1b[97;1;97u"
        );
        assert_eq!(
            enc(key(Key::Char('a'), SHIFT, Some("A"), Press), f).unwrap(),
            "\x1b[97:65;2;65u"
        );
        assert_eq!(
            enc(key(Key::Enter, NONE, None, Press), f).unwrap(),
            "\x1b[13u"
        );
        assert_eq!(
            enc(key(Key::Char('a'), NONE, Some("a"), Press), ALL_AS_ESCAPES).unwrap(),
            "\x1b[97u"
        );
    }
}
