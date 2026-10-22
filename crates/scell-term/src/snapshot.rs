use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{self, Color, NamedColor};

use crate::search::{SearchState, visible_matches};
use crate::{Palette, Rgb};

/// 可见区域的只读副本。颜色已解析为最终 RGB（含反色、暗淡、隐藏处理），
/// 渲染层无需了解 VT 语义。
#[derive(Debug, Default, Clone)]
pub struct RenderSnapshot {
    pub cols: usize,
    pub rows: usize,
    /// 行优先，长度为 `cols * rows`。
    pub cells: Vec<SnapshotCell>,
    /// 组合字符（变音符、ZWJ 序列等），由 [`SnapshotCell::combining`] 索引。
    pub combining: Vec<String>,
    pub cursor: Option<SnapshotCursor>,
    pub display_offset: usize,
    /// 回滚区行数（备用屏下为 0）。
    pub history: usize,
    pub background: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub attrs: CellAttrs,
    pub combining: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCursor {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// 光标所在单元格是否为宽字符，决定光标宽度。
    pub wide: bool,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    HollowBlock,
    Underline,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs(u16);

impl CellAttrs {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 3);
    pub const UNDERCURL: Self = Self(1 << 4);
    pub const STRIKEOUT: Self = Self(1 << 5);
    /// 宽字符的首格，字形占两格。
    pub const WIDE: Self = Self(1 << 6);
    /// 宽字符的第二格或行尾占位格，不绘制字形。
    pub const SPACER: Self = Self(1 << 7);
    /// 位于选区内，背景已替换为选区色。
    pub const SELECTED: Self = Self(1 << 8);
    /// 查找命中。
    pub const MATCH: Self = Self(1 << 9);
    /// 当前查找命中，同时带有 `MATCH`。
    pub const FOCUSED_MATCH: Self = Self(1 << 10);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    fn set(&mut self, other: Self, on: bool) {
        if on {
            self.0 |= other.0;
        } else {
            self.0 &= !other.0;
        }
    }
}

impl SnapshotCell {
    const BLANK: Self = Self {
        ch: ' ',
        fg: Rgb::new(0, 0, 0),
        bg: Rgb::new(0, 0, 0),
        attrs: CellAttrs(0),
        combining: None,
    };

    /// 需要绘制字形的单元格。
    pub fn has_glyph(&self) -> bool {
        !self.attrs.contains(CellAttrs::SPACER) && (self.ch != ' ' || self.combining.is_some())
    }
}

impl RenderSnapshot {
    pub fn row(&self, row: usize) -> &[SnapshotCell] {
        &self.cells[row * self.cols..(row + 1) * self.cols]
    }

    pub fn combining_of(&self, cell: &SnapshotCell) -> Option<&str> {
        cell.combining.map(|i| self.combining[i as usize].as_str())
    }

    pub(crate) fn fill<T: EventListener>(
        &mut self,
        term: &Term<T>,
        palette: &Palette,
        search: Option<&mut SearchState>,
    ) {
        let (matches, focused) = match search {
            Some(state) => (visible_matches(term, state), state.focused.clone()),
            None => (Vec::new(), None),
        };
        let mut next_match = 0;
        let content = term.renderable_content();
        let cols = term.grid().columns();
        let rows = term.grid().screen_lines();
        let overrides = content.colors;
        let selection = content.selection;

        self.cols = cols;
        self.rows = rows;
        self.display_offset = content.display_offset;
        self.history = term.grid().history_size();
        self.background = palette.resolve(Color::Named(NamedColor::Background), overrides);
        self.combining.clear();
        self.cells.clear();
        self.cells.resize(
            cols * rows,
            SnapshotCell {
                bg: self.background,
                ..SnapshotCell::BLANK
            },
        );

        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let row = (indexed.point.line.0 + offset) as usize;
            let col = indexed.point.column.0;
            if row >= rows || col >= cols {
                continue;
            }
            let cell = &indexed.cell;
            let flags = cell.flags;

            let mut fg = cell.fg;
            if flags.contains(Flags::DIM)
                && let Color::Named(n) = fg
            {
                fg = Color::Named(n.to_dim());
            }
            let mut fg = palette.resolve(fg, overrides);
            let mut bg = palette.resolve(cell.bg, overrides);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let point = indexed.point;
            let selected = selection.is_some_and(|s| s.contains(point));
            // 匹配按位置有序，显示迭代也按位置递增，只需向前推进
            while next_match < matches.len() && *matches[next_match].end() < point {
                next_match += 1;
            }
            let matched = matches.get(next_match).is_some_and(|m| m.contains(&point));
            if selected {
                bg = palette.selection;
            }

            let mut attrs = CellAttrs::default();
            attrs.set(CellAttrs::BOLD, flags.contains(Flags::BOLD));
            attrs.set(CellAttrs::ITALIC, flags.contains(Flags::ITALIC));
            attrs.set(
                CellAttrs::UNDERLINE,
                flags.intersects(
                    Flags::UNDERLINE | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE,
                ),
            );
            attrs.set(
                CellAttrs::DOUBLE_UNDERLINE,
                flags.contains(Flags::DOUBLE_UNDERLINE),
            );
            attrs.set(CellAttrs::UNDERCURL, flags.contains(Flags::UNDERCURL));
            attrs.set(CellAttrs::STRIKEOUT, flags.contains(Flags::STRIKEOUT));
            attrs.set(CellAttrs::SELECTED, selected);
            attrs.set(CellAttrs::MATCH, matched);
            attrs.set(
                CellAttrs::FOCUSED_MATCH,
                focused.as_ref().is_some_and(|m| m.contains(&point)),
            );
            attrs.set(CellAttrs::WIDE, flags.contains(Flags::WIDE_CHAR));
            attrs.set(
                CellAttrs::SPACER,
                flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            );

            let combining = cell.zerowidth().map(|zw| {
                self.combining.push(zw.iter().collect());
                (self.combining.len() - 1) as u32
            });

            self.cells[row * cols + col] = SnapshotCell {
                ch: cell.c,
                fg,
                bg,
                attrs,
                combining,
            };
        }

        let cursor = content.cursor;
        let blinking = term.cursor_style().blinking;
        let row = cursor.point.line.0 + offset;
        self.cursor = cursor_shape(cursor.shape)
            .filter(|_| row >= 0 && (row as usize) < rows && cursor.point.column.0 < cols)
            .map(|shape| {
                let (row, col) = (row as usize, cursor.point.column.0);
                SnapshotCursor {
                    row,
                    col,
                    shape,
                    wide: self.cells[row * cols + col].attrs.contains(CellAttrs::WIDE),
                    blinking,
                }
            });
    }
}

fn cursor_shape(shape: ansi::CursorShape) -> Option<CursorShape> {
    match shape {
        ansi::CursorShape::Block => Some(CursorShape::Block),
        ansi::CursorShape::HollowBlock => Some(CursorShape::HollowBlock),
        ansi::CursorShape::Underline => Some(CursorShape::Underline),
        ansi::CursorShape::Beam => Some(CursorShape::Beam),
        ansi::CursorShape::Hidden => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{TermSize, Terminal};

    use super::*;

    fn snapshot_of(bytes: &[u8]) -> RenderSnapshot {
        let t = Terminal::new(TermSize::new(10, 3, 8, 16), Palette::default(), 100);
        t.feed(bytes, &mut Vec::new());
        let mut snap = RenderSnapshot::default();
        t.snapshot(&mut snap);
        snap
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let snap = snapshot_of("中a".as_bytes());
        let row = snap.row(0);
        assert_eq!(row[0].ch, '中');
        assert!(row[0].attrs.contains(CellAttrs::WIDE));
        assert!(row[1].attrs.contains(CellAttrs::SPACER));
        assert!(!row[1].has_glyph());
        assert_eq!(row[2].ch, 'a');
        assert_eq!(snap.cursor.map(|c| c.col), Some(3));
    }

    #[test]
    fn inverse_and_truecolor_are_resolved() {
        let snap = snapshot_of(b"\x1b[38;2;1;2;3;7mx");
        let cell = snap.row(0)[0];
        assert_eq!(cell.bg, Rgb::new(1, 2, 3));
        assert_eq!(cell.fg, snap.background);
    }

    #[test]
    fn combining_marks_are_kept() {
        let snap = snapshot_of("e\u{301}".as_bytes());
        let cell = snap.row(0)[0];
        assert_eq!(snap.combining_of(&cell), Some("\u{301}"));
    }

    #[test]
    fn hidden_cursor_is_none() {
        assert!(snapshot_of(b"\x1b[?25l").cursor.is_none());
    }
}
