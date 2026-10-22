//! 主题文件：gpui-component 主题集 JSON，每个主题可附带 `terminal` 段定义终端配色。
//!
//! UI 配色由表现层交给组件库解析，这里只解析终端部分；未给出的颜色由表现层从 UI 配色或内置调色板补齐。

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexColor(pub [u8; 3]);

impl HexColor {
    /// 接受 `#rrggbb` 或 `rrggbb`。
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?]))
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(rust_i18n::t!("error.color", value = format!("{s:?}")))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TerminalColors {
    pub background: Option<HexColor>,
    pub foreground: Option<HexColor>,
    pub cursor: Option<HexColor>,
    pub selection: Option<HexColor>,
    pub black: Option<HexColor>,
    pub red: Option<HexColor>,
    pub green: Option<HexColor>,
    pub yellow: Option<HexColor>,
    pub blue: Option<HexColor>,
    pub magenta: Option<HexColor>,
    pub cyan: Option<HexColor>,
    pub white: Option<HexColor>,
    pub bright_black: Option<HexColor>,
    pub bright_red: Option<HexColor>,
    pub bright_green: Option<HexColor>,
    pub bright_yellow: Option<HexColor>,
    pub bright_blue: Option<HexColor>,
    pub bright_magenta: Option<HexColor>,
    pub bright_cyan: Option<HexColor>,
    pub bright_white: Option<HexColor>,
}

impl TerminalColors {
    /// ANSI 0–15，顺序同 SGR 30–37、90–97。
    pub fn ansi(&self) -> [Option<HexColor>; 16] {
        [
            self.black,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.magenta,
            self.cyan,
            self.white,
            self.bright_black,
            self.bright_red,
            self.bright_green,
            self.bright_yellow,
            self.bright_blue,
            self.bright_magenta,
            self.bright_cyan,
            self.bright_white,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThemeEntry {
    pub name: String,
    #[serde(default)]
    pub mode: ThemeMode,
    #[serde(default)]
    pub terminal: Option<TerminalColors>,
}

/// 从主题集 JSON 中取出各主题的名称、明暗与终端配色，忽略其余字段。
pub fn parse_theme_set(json: &str) -> Result<Vec<ThemeEntry>, String> {
    #[derive(Deserialize)]
    struct ThemeSet {
        themes: Vec<ThemeEntry>,
    }
    serde_json::from_str::<ThemeSet>(json)
        .map(|s| s.themes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terminal_section_and_ignores_ui_colors() {
        let json = r##"{
            "name": "X",
            "themes": [
                { "name": "X Dark", "mode": "dark",
                  "colors": { "background": "neutral-950" },
                  "terminal": { "red": "#ff0000", "bright_blue": "0000FF" } },
                { "name": "X Light", "colors": {} }
            ]
        }"##;
        let themes = parse_theme_set(json).unwrap();
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].mode, ThemeMode::Dark);
        let term = themes[0].terminal.as_ref().unwrap();
        assert_eq!(term.ansi()[1], Some(HexColor([255, 0, 0])));
        assert_eq!(term.ansi()[12], Some(HexColor([0, 0, 255])));
        assert_eq!(themes[1].mode, ThemeMode::Light);
        assert!(themes[1].terminal.is_none());
    }

    #[test]
    fn rejects_bad_color() {
        let json = r##"{ "themes": [{ "name": "A", "terminal": { "red": "#ff00" } }] }"##;
        assert!(parse_theme_set(json).is_err());
    }
}
