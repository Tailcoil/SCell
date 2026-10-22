//! 选区。调用方用视口坐标交互；内部存的是网格坐标，回滚滚动时选区跟随内容移动。

use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::viewport_to_point;

use crate::Terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// 按字符连续选择。
    Simple,
    /// 矩形选择。
    Block,
    /// 按词选择（双击），以空白和常见分隔符为边界。
    Word,
    /// 整行选择（三击）。
    Line,
}

/// 视口内的位置，`row` 从可见区域顶部算起。`right_half` 表示落在单元格右半边，
/// 用于决定字符是否被选中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportPoint {
    pub row: usize,
    pub col: usize,
    pub right_half: bool,
}

impl Terminal {
    pub fn start_selection(&self, kind: SelectionKind, at: ViewportPoint) {
        let mut term = self.term.lock();
        let (point, side) = grid_point(term.grid().display_offset(), at);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Block => SelectionType::Block,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn extend_selection(&self, to: ViewportPoint) {
        let mut term = self.term.lock();
        let (point, side) = grid_point(term.grid().display_offset(), to);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// 选区为空（单击未拖动）时返回 `None`。
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock();
        if term.selection.as_ref().is_none_or(Selection::is_empty) {
            return None;
        }
        term.selection_to_string().filter(|s| !s.is_empty())
    }
}

fn grid_point(display_offset: usize, at: ViewportPoint) -> (Point, Side) {
    let point = viewport_to_point(display_offset, Point::new(at.row, Column(at.col)));
    let side = if at.right_half {
        Side::Right
    } else {
        Side::Left
    };
    (point, side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TermSize, Terminal};

    fn term_with(text: &[u8]) -> Terminal {
        let t = Terminal::new(TermSize::new(20, 3, 8, 16), Default::default(), 100);
        t.feed(text, &mut Vec::new());
        t
    }

    fn at(row: usize, col: usize, right_half: bool) -> ViewportPoint {
        ViewportPoint {
            row,
            col,
            right_half,
        }
    }

    #[test]
    fn simple_selection_across_lines() {
        let t = term_with(b"hello world\r\nsecond line");
        t.start_selection(SelectionKind::Simple, at(0, 6, false));
        t.extend_selection(at(1, 5, true));
        assert_eq!(t.selection_text().as_deref(), Some("world\nsecond"));
    }

    #[test]
    fn click_without_drag_is_empty() {
        let t = term_with(b"hello");
        t.start_selection(SelectionKind::Simple, at(0, 1, false));
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn word_and_line_selection() {
        let t = term_with(b"ls /usr/bin -la\r\nnext");
        t.start_selection(SelectionKind::Word, at(0, 5, false));
        assert_eq!(t.selection_text().as_deref(), Some("/usr/bin"));
        t.start_selection(SelectionKind::Line, at(1, 0, false));
        // 整行选择带行尾换行，与 Alacritty 一致
        assert_eq!(t.selection_text().as_deref(), Some("next\n"));
    }

    #[test]
    fn selection_marks_snapshot_cells() {
        let t = term_with(b"abcdef");
        t.start_selection(SelectionKind::Simple, at(0, 1, false));
        t.extend_selection(at(0, 2, true));
        let mut snap = crate::RenderSnapshot::default();
        t.snapshot(&mut snap);
        let selected: Vec<bool> = snap.row(0)[..4]
            .iter()
            .map(|c| c.attrs.contains(crate::CellAttrs::SELECTED))
            .collect();
        assert_eq!(selected, [false, true, true, false]);
    }
}
