use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{FontId, GlyphId, Hsla, Pixels, TextRun, Window};

use crate::TerminalStyle;
use crate::metrics::styled_font;

/// 已整形的单元格文本。整形结果包含回退字体的选择，按（文本, 样式）缓存，字体或字号变化时整体失效。
#[derive(Default)]
pub(crate) struct GlyphCache {
    entries: HashMap<GlyphKey, Arc<ShapedCell>>,
    font_size: Option<Pixels>,
}

#[derive(Hash, PartialEq, Eq)]
struct GlyphKey {
    text: Box<str>,
    bold: bool,
    italic: bool,
}

pub(crate) struct ShapedCell {
    pub glyphs: Vec<CellGlyph>,
    /// 整形后的总宽度，用于在单元格内居中。
    pub width: Pixels,
}

#[derive(Clone, Copy)]
pub(crate) struct CellGlyph {
    pub font_id: FontId,
    pub glyph_id: GlyphId,
    pub x: Pixels,
    pub is_emoji: bool,
}

impl GlyphCache {
    pub(crate) fn invalidate(&mut self) {
        self.entries.clear();
        self.font_size = None;
    }

    pub(crate) fn get(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
        style: &TerminalStyle,
        window: &Window,
    ) -> Arc<ShapedCell> {
        if self.font_size != Some(style.font_size) {
            self.entries.clear();
            self.font_size = Some(style.font_size);
        }
        let key = GlyphKey {
            text: text.into(),
            bold,
            italic,
        };
        if let Some(cell) = self.entries.get(&key) {
            return cell.clone();
        }
        let run = TextRun {
            len: text.len(),
            font: styled_font(style, bold, italic),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = window
            .text_system()
            .layout_line(text, style.font_size, &[run], None);
        let glyphs = layout
            .runs
            .iter()
            .flat_map(|run| {
                run.glyphs.iter().map(|g| CellGlyph {
                    font_id: run.font_id,
                    glyph_id: g.id,
                    x: g.position.x,
                    is_emoji: g.is_emoji,
                })
            })
            .collect();
        let shaped = Arc::new(ShapedCell {
            glyphs,
            width: layout.width,
        });
        self.entries.insert(key, shaped.clone());
        shaped
    }
}
