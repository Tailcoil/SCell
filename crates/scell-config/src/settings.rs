//! 用户设置（`settings.toml`）。缺省字段取默认值，越界数值收敛到可用范围。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Settings {
    pub appearance: Appearance,
    pub terminal: TerminalSettings,
    pub shell: ShellSettings,
    pub ssh: SshSettings,
    pub files: FileSettings,
    /// 改键：命令名到按键，只列要改的命令。命令名由界面层定义。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, KeyList>,
}

/// `[keys]` 中一个命令的按键：一个字符串或字符串数组，写空数组取消该命令的默认按键。
/// 按键写法如 `"ctrl-shift-t"`，空格分隔的多个按键表示依次按下。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    /// 非空的按键。
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::One(key) => std::slice::from_ref(key),
            Self::Many(keys) => keys.as_slice(),
        }
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Appearance {
    pub theme: ThemeChoice,
    pub window: WindowMaterial,
    /// 窗口背景不透明度，仅对非 `opaque` 材质生效。
    pub opacity: f32,
    /// 界面字体；未设置时跟随系统。终端字体见 `[terminal]`。
    pub ui_font: Option<String>,
    /// 界面基准字号（逻辑像素）；文字与按 rem 计的间距随之缩放。未设置时跟随系统。
    pub ui_font_size: Option<f32>,
    /// 界面过渡动画。
    pub animations: bool,
    /// 界面语言（`zh-CN`、`en`）；未设置时跟随系统。
    pub language: Option<String>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            window: WindowMaterial::Opaque,
            opacity: 0.85,
            ui_font: None,
            ui_font_size: None,
            animations: true,
            language: None,
        }
    }
}

/// 固定主题名，或按系统明暗在两个主题间切换。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThemeChoice {
    Fixed(String),
    System { light: String, dark: String },
}

impl Default for ThemeChoice {
    fn default() -> Self {
        Self::System {
            light: "SCell Light".into(),
            dark: "SCell Dark".into(),
        }
    }
}

impl ThemeChoice {
    pub fn resolve(&self, dark: bool) -> &str {
        match self {
            Self::Fixed(name) => name,
            Self::System { light, dark: d } => {
                if dark {
                    d
                } else {
                    light
                }
            }
        }
    }
}

/// 窗口背景：不透明、半透明，或半透明加背景模糊（合成器不提供模糊时只有半透明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowMaterial {
    Opaque,
    Translucent,
    Blur,
}

impl WindowMaterial {
    pub const ALL: [Self; 3] = [Self::Opaque, Self::Translucent, Self::Blur];
}

/// 单元枚举在设置文件中的写法，直接取自 serde 的重命名规则，不另外维护一份映射。
pub fn spelling<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TerminalSettings {
    pub font_family: String,
    /// 主字体缺字时依次尝试，用于 CJK、Emoji 与图标。
    pub font_fallbacks: Vec<String>,
    pub font_size: f32,
    /// 行高相对字号的倍数。
    pub line_height: f32,
    pub scrollback: usize,
    pub cursor_shape: CursorStyle,
    pub cursor_blink: bool,
    pub copy_on_select: bool,
    /// 粘贴多行文本前确认。程序开启了括号粘贴（多数现代 shell）时粘贴不会直接执行，不再询问。
    pub confirm_multiline_paste: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "FiraCode Nerd Font Mono".into(),
            font_fallbacks: vec![
                "Noto Sans Mono CJK SC".into(),
                "WenQuanYi Zen Hei Mono".into(),
                "Noto Color Emoji".into(),
            ],
            font_size: 14.,
            line_height: 1.25,
            scrollback: 10_000,
            cursor_shape: CursorStyle::Block,
            cursor_blink: true,
            copy_on_select: false,
            confirm_multiline_paste: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
}

/// 新建本地标签时启动的程序（未设置时用系统默认 shell）与启动行为。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ShellSettings {
    pub program: Option<String>,
    pub args: Vec<String>,
    /// 启动时恢复上次退出时的窗口与标签；SSH 标签恢复为未连接，按 Enter 再连。
    pub restore_session: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            program: None,
            args: Vec::new(),
            restore_session: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SshSettings {
    /// 主机密钥记录文件；未设置时与 OpenSSH 共用 `~/.ssh/known_hosts`。
    pub known_hosts: Option<std::path::PathBuf>,
    /// 已建立的连接因网络原因断开后，按退避间隔自动重连。认证失败等不会重试。
    pub auto_reconnect: bool,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            known_hosts: None,
            auto_reconnect: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct FileSettings {
    /// 从文件面板下载时保存到的目录；未设置时为系统的下载目录。
    pub download_dir: Option<std::path::PathBuf>,
    /// 文件列表显示隐藏文件（以 `.` 开头）。
    pub show_hidden: bool,
}

impl FileSettings {
    /// 实际使用的下载目录。
    pub fn download_dir(&self) -> std::path::PathBuf {
        self.download_dir
            .clone()
            .or_else(dirs::download_dir)
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| ".".into())
    }
}

impl Settings {
    /// 读取设置文件；文件不存在时返回默认值。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|message| ConfigError::Parse {
                path: path.to_owned(),
                message,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// 首次打开设置文件时写入的内容：全部默认值，便于用户照着修改。
    pub fn template() -> String {
        toml::to_string_pretty(&Self::default()).unwrap_or_default()
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str::<Self>(text)
            .map(Self::sanitized)
            .map_err(|e| e.to_string())
    }

    fn sanitized(mut self) -> Self {
        let a = &mut self.appearance;
        a.opacity = clamp_or(a.opacity, limits::OPACITY, 0.85);
        a.ui_font_size = a
            .ui_font_size
            .map(|s| clamp_or(s, limits::UI_FONT_SIZE, 16.));
        if a.ui_font.as_deref().is_some_and(|f| f.trim().is_empty()) {
            a.ui_font = None;
        }
        let t = &mut self.terminal;
        t.font_size = clamp_or(t.font_size, limits::FONT_SIZE, 14.);
        t.line_height = clamp_or(t.line_height, limits::LINE_HEIGHT, 1.25);
        t.scrollback = t.scrollback.min(limits::MAX_SCROLLBACK);
        self
    }
}

/// 设置文件中的值，供 [`write_value`] 使用。
pub use toml_edit::{InlineTable, Value};

/// 修改设置文件中 `[table]` 下的一个键并原子写回，保留其余内容与注释；`None` 删除该键。文件不存在时新建。
pub fn write_value(
    path: &Path,
    table: &str,
    key: &str,
    value: Option<Value>,
) -> Result<(), ConfigError> {
    let io_err = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(e)),
    };
    let mut doc: toml_edit::DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Parse {
                path: path.to_owned(),
                message: e.to_string(),
            })?;
    let section = doc
        .entry(table)
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| ConfigError::Parse {
            path: path.to_owned(),
            message: rust_i18n::t!("error.not_table", table = table).into_owned(),
        })?;
    match value {
        Some(value) => {
            section.insert(key, toml_edit::Item::Value(value));
        }
        None => {
            section.remove(key);
        }
    }
    crate::paths::write_atomic(path, &doc.to_string())
}

/// 各数值设置的取值范围。读取配置时据此修正，设置页的输入框也用同一组范围。
pub mod limits {
    use std::ops::RangeInclusive;

    /// 卡片与控件还有各自的主题底色，窗口背景可以相当透明。
    pub const OPACITY: RangeInclusive<f32> = 0.1..=1.;
    pub const UI_FONT_SIZE: RangeInclusive<f32> = 10.0..=24.;
    pub const FONT_SIZE: RangeInclusive<f32> = 6.0..=72.;
    pub const LINE_HEIGHT: RangeInclusive<f32> = 1.0..=2.5;
    pub const MAX_SCROLLBACK: usize = 1_000_000;
}

fn clamp_or(v: f32, range: std::ops::RangeInclusive<f32>, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(*range.start(), *range.end())
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_default() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
    }

    #[test]
    fn keys_accept_one_or_many() {
        let s = Settings::parse(
            "[keys]\nnew_tab = \"ctrl-t\"\npaste = [\"ctrl-v\", \" shift-insert \"]\ncopy = []\n",
        )
        .unwrap();
        let keys = |name: &str| s.keys[name].iter().collect::<Vec<_>>();
        assert_eq!(keys("new_tab"), ["ctrl-t"]);
        assert_eq!(keys("paste"), ["ctrl-v", "shift-insert"]);
        assert!(keys("copy").is_empty());
        // 没改过键时模板里不出现空的 [keys]
        assert!(!Settings::template().contains("[keys]"));
    }

    #[test]
    fn theme_choice_forms() {
        let s = Settings::parse("[appearance]\ntheme = \"Catppuccin Mocha\"").unwrap();
        assert_eq!(s.appearance.theme.resolve(false), "Catppuccin Mocha");

        let s = Settings::parse("[appearance]\ntheme = { light = \"L\", dark = \"D\" }").unwrap();
        assert_eq!(s.appearance.theme.resolve(true), "D");
        assert_eq!(s.appearance.theme.resolve(false), "L");
    }

    #[test]
    fn values_are_clamped() {
        let s = Settings::parse(
            "[appearance]\nwindow = \"blur\"\nopacity = 0.05\n[terminal]\nfont_size = 200\n",
        )
        .unwrap();
        assert_eq!(s.appearance.window, WindowMaterial::Blur);
        assert_eq!(s.appearance.opacity, 0.1);
        assert_eq!(s.terminal.font_size, 72.);
        // 未写的字段保持默认
        assert_eq!(s.terminal.line_height, 1.25);
    }

    #[test]
    fn invalid_value_reports_location() {
        let err = Settings::parse("[appearance]\nwindow = \"frosted\"").unwrap_err();
        assert!(err.contains("window"), "{err}");
    }

    #[test]
    fn write_value_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "# 我的配置\n[terminal]\nfont_size = 15 # 大一点\ncursor_blink = false\n",
        )
        .unwrap();
        write_value(
            &path,
            "appearance",
            "theme",
            Some("Catppuccin Mocha".into()),
        )
        .unwrap();
        write_value(&path, "appearance", "window", Some("blur".into())).unwrap();
        write_value(&path, "terminal", "line_height", Some(1.5.into())).unwrap();
        write_value(&path, "terminal", "cursor_blink", None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# 我的配置") && text.contains("# 大一点"),
            "{text}"
        );
        let s = Settings::parse(&text).unwrap();
        assert_eq!(s.appearance.theme.resolve(true), "Catppuccin Mocha");
        assert_eq!(s.appearance.window, WindowMaterial::Blur);
        assert_eq!(s.terminal.font_size, 15.);
        assert_eq!(s.terminal.line_height, 1.5);
        assert!(s.terminal.cursor_blink);
    }

    #[test]
    fn template_round_trips() {
        assert_eq!(
            Settings::parse(&Settings::template()).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load(&dir.path().join("settings.toml")).unwrap();
        assert_eq!(s, Settings::default());
    }
}
