use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Dimensions;

/// 终端网格尺寸。像素尺寸只用于回答 CSI 14/16/18 t 等查询。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TermSize {
    pub const MIN_COLS: u16 = 2;
    pub const MIN_ROWS: u16 = 1;

    pub fn new(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> Self {
        Self {
            cols: cols.max(Self::MIN_COLS),
            rows: rows.max(Self::MIN_ROWS),
            cell_width,
            cell_height,
        }
    }
}

impl Default for TermSize {
    fn default() -> Self {
        Self::new(80, 24, 8, 16)
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl From<TermSize> for WindowSize {
    fn from(size: TermSize) -> Self {
        WindowSize {
            num_lines: size.rows,
            num_cols: size.cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}
