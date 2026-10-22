use gpui_kit::{Hsla, Pixels, SharedString};
use scell_term::{CursorShape, Palette};

/// 终端外观。字体回退列表按顺序尝试，用于 CJK、Emoji 与图标字形。
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalStyle {
    pub font_family: SharedString,
    pub fallbacks: Vec<String>,
    pub font_size: Pixels,
    /// 行高相对字号的倍数。
    pub line_height: f32,
    pub palette: Palette,
    /// 默认背景色的不透明度。为 0 时不绘制，由宿主容器提供背景（圆角、毛玻璃）。
    pub background_alpha: f32,
    /// 查找命中的高亮，叠加在单元格背景之上，应带透明度。
    pub match_background: Hsla,
    pub focused_match_background: Hsla,
    /// 程序未指定光标样式时的默认值。
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
}
