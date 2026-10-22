use gpui_kit::{Font, FontFallbacks, FontStyle, FontWeight, Pixels, Window, font, px};

use crate::TerminalStyle;

/// 单元格几何。宽高按设备像素取整，避免相邻背景块之间出现缝隙。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellMetrics {
    pub width: Pixels,
    pub height: Pixels,
    /// 基线相对单元格顶部的偏移。
    pub baseline: Pixels,
    pub font_size: Pixels,
    pub underline_offset: Pixels,
    pub stroke: Pixels,
}

impl CellMetrics {
    pub(crate) fn measure(style: &TerminalStyle, window: &Window) -> Self {
        let scale = window.scale_factor();
        let snap = |v: Pixels| px((v.as_f32() * scale).round().max(1.) / scale);

        let text = window.text_system();
        let font_id = text.resolve_font(&base_font(style));
        let size = style.font_size;
        let advance = text
            .advance(font_id, size, 'm')
            .map(|s| s.width)
            .unwrap_or(size * 0.6);
        let ascent = text.ascent(font_id, size);
        let descent = text.descent(font_id, size).abs();

        let width = snap(advance);
        let height = snap(size * style.line_height).max(snap(ascent + descent));
        let baseline = snap((height - ascent - descent) / 2. + ascent);
        Self {
            width,
            height,
            baseline,
            font_size: size,
            underline_offset: baseline + snap(descent / 2.),
            stroke: snap(px(1.)),
        }
    }
}

pub(crate) fn base_font(style: &TerminalStyle) -> Font {
    let mut f = font(style.font_family.clone());
    f.fallbacks = Some(FontFallbacks::from_fonts(style.fallbacks.clone()));
    f
}

pub(crate) fn styled_font(style: &TerminalStyle, bold: bool, italic: bool) -> Font {
    let mut f = base_font(style);
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f.style = FontStyle::Italic;
    }
    f
}
