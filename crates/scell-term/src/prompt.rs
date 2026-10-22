//! OSC 133 提示符标记的位置跟踪，用于在回滚区里按命令跳转。
//!
//! alacritty 的网格既没有稳定的行标识，也不通知滚动，这里自己累计“滚入回滚区的总行数”：
//! 每段输出解析前后比较主屏首行的存储地址（行在环形缓冲里移动时单元格缓冲不变），
//! 它在回滚区里的新位置就是这段输出滚动的行数。标记存为“总行数 + 屏幕行号”的绝对行号，
//! 滚出回滚区后丢弃。缩放会重排行，按标记距光标隔了几个逻辑行重新定位。
//!
//! 回滚区满后最早的行会被循环复用：一次滚动超过回滚上限时锚点可能已换了内容，地址比较不再可信，
//! 这时丢弃全部标记。扫描器为每段输出给出可能滚动行数的上界，据此判断。
//!
//! 只跟踪主屏。备用屏期间主屏不变，锚点留到回到主屏时再找；但备用屏期间的缩放会重排
//! 看不到的主屏（alacritty 不公开它），这时也只能丢弃全部标记。

use std::collections::VecDeque;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};

pub(crate) struct PromptMarks {
    /// 绝对行号，严格递增。
    marks: VecDeque<i64>,
    /// 至今滚入回滚区的总行数（含已被丢弃的）。
    scrolled: i64,
    /// 上次采样时主屏首行的单元格缓冲地址，只用于比较。
    anchor: Option<usize>,
    /// 取锚点以来最多可能滚动的行数。
    pending: usize,
    /// 回滚区上限。
    limit: usize,
}

/// 标记的逻辑行位置：距光标所在逻辑行隔了几个逻辑行，由近到远。
pub(crate) struct Saved(Option<Vec<usize>>);

impl PromptMarks {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            marks: VecDeque::new(),
            scrolled: 0,
            anchor: None,
            pending: 0,
            limit,
        }
    }

    /// 解析一段输出之后调用，累计这段输出滚动的行数。`max_scroll` 是它最多可能滚动的行数；
    /// `deferred` 表示解析器还压着同步更新（DEC 2026）的字节，它们的滚动要到应用时才发生。
    pub(crate) fn track<T: EventListener>(
        &mut self,
        term: &Term<T>,
        max_scroll: usize,
        deferred: bool,
    ) {
        self.pending = self.pending.saturating_add(max_scroll);
        if deferred || term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let grid = term.grid();
        if let Some(anchor) = self.anchor {
            let history = grid.history_size() as i32;
            if self.pending > self.limit {
                self.marks.clear();
            } else if let Some(k) = (0..=history).find(|&k| row_addr(grid, Line(-k)) == anchor) {
                self.scrolled += i64::from(k);
            } else if (1..grid.screen_lines() as i32).any(|l| row_addr(grid, Line(l)) == anchor) {
                // 首行被下推（反向滚动、插入行）：回滚区不变，屏幕上的标记已错位
                let first_screen = self.scrolled;
                self.marks.retain(|&m| m < first_screen);
            } else {
                self.marks.clear();
            }
        }
        self.pending = 0;
        self.anchor = Some(row_addr(grid, Line(0)));
        let top = self.scrolled - grid.history_size() as i64;
        while self.marks.front().is_some_and(|&m| m < top) {
            self.marks.pop_front();
        }
    }

    /// 光标所在行是一个提示符的开始。须在 [`Self::track`] 之后调用。
    pub(crate) fn add<T: EventListener>(&mut self, term: &Term<T>) {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let mark = self.scrolled + i64::from(term.grid().cursor.point.line.0);
        // 新提示符画在已有标记之上（如清屏后重画）时，下面的标记已被覆盖
        while self.marks.back().is_some_and(|&m| m >= mark) {
            self.marks.pop_back();
        }
        self.marks.push_back(mark);
    }

    /// 当前有效的标记所在行（屏幕行号，回滚区为负），从上到下。
    pub(crate) fn lines(&self) -> impl DoubleEndedIterator<Item = i32> + '_ {
        self.marks.iter().map(|&m| (m - self.scrolled) as i32)
    }

    /// 缩放前调用，结果交给 [`Self::restore`]。
    pub(crate) fn save<T: EventListener>(&self, term: &Term<T>) -> Saved {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return Saved(None);
        }
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        let cursor = grid.cursor.point.line.0;
        let mut row = cursor;
        // 光标以下的标记（程序把光标移到了上方）缩放后无从对应，直接放弃
        let mut lines = self.lines().rev().skip_while(|&l| l > cursor).peekable();
        let mut depth = 0;
        let mut saved = Vec::new();
        while let Some(&line) = lines.peek() {
            if line == row {
                saved.push(depth);
                lines.next();
                continue;
            }
            if row == top {
                break;
            }
            if !wraps(grid, row - 1) {
                depth += 1;
            }
            row -= 1;
        }
        Saved(Some(saved))
    }

    /// 缩放后调用：按逻辑行位置重新定位标记，并重新取锚点。
    pub(crate) fn restore<T: EventListener>(&mut self, term: &Term<T>, saved: Saved) {
        self.marks.clear();
        self.pending = 0;
        let Saved(Some(saved)) = saved else {
            // 备用屏期间主屏被重排过，旧锚点所在的行也可能已重新分配
            self.anchor = None;
            return;
        };
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        let mut row = grid.cursor.point.line.0;
        let mut depth = 0;
        for target in saved {
            while depth < target && row > top {
                if !wraps(grid, row - 1) {
                    depth += 1;
                }
                row -= 1;
            }
            if depth < target {
                break;
            }
            let mut start = row;
            while start > top && wraps(grid, start - 1) {
                start -= 1;
            }
            let mark = self.scrolled + i64::from(start);
            if self.marks.front() != Some(&mark) {
                self.marks.push_front(mark);
            }
        }
        self.anchor = Some(row_addr(grid, Line(0)));
    }
}

/// 行的单元格缓冲地址。行在环形缓冲里滚动、交换时缓冲本身不动。
fn row_addr(grid: &Grid<Cell>, line: Line) -> usize {
    std::ptr::from_ref(&grid[line][Column(0)]).addr()
}

/// 这一行是否折行到下一行（属于同一逻辑行）。
fn wraps(grid: &Grid<Cell>, line: i32) -> bool {
    grid[Line(line)][grid.last_column()]
        .flags
        .contains(Flags::WRAPLINE)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};

    use crate::{Palette, SearchDirection, TermSize, Terminal};

    fn terminal(cols: u16, rows: u16, scrollback: usize) -> Terminal {
        Terminal::new(
            TermSize::new(cols, rows, 8, 16),
            Palette::default(),
            scrollback,
        )
    }

    fn feed(t: &Terminal, bytes: &[u8]) {
        t.feed(bytes, &mut Vec::new());
    }

    /// 每条命令：提示符一行，输出 `lines` 行，最后停在一个新提示符上。
    fn session(t: &Terminal, commands: usize, lines: usize) {
        for i in 0..commands {
            feed(t, format!("\x1b]133;A\x07$ cmd{i}\r\n").as_bytes());
            for j in 0..lines {
                feed(t, format!("out{i}-{j}\r\n").as_bytes());
            }
        }
        feed(t, b"\x1b]133;A\x07$ ");
    }

    /// 视口顶部那一行的文字。
    fn top(t: &Terminal) -> String {
        let term = t.term.lock();
        let grid = term.grid();
        let row = &grid[Line(-(grid.display_offset() as i32))];
        let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
        text.trim_end().to_owned()
    }

    fn older(t: &Terminal) -> bool {
        t.scroll_to_prompt(SearchDirection::Older)
    }

    fn newer(t: &Terminal) -> bool {
        t.scroll_to_prompt(SearchDirection::Newer)
    }

    #[test]
    fn jumps_between_prompts_in_scrollback() {
        let t = terminal(20, 5, 100);
        session(&t, 6, 3);
        assert_eq!(top(&t), "$ cmd5");
        assert!(older(&t));
        assert_eq!(top(&t), "$ cmd4");
        assert!(older(&t));
        assert_eq!(top(&t), "$ cmd3");
        assert!(newer(&t));
        assert_eq!(top(&t), "$ cmd4");
        assert!(newer(&t));
        assert_eq!(top(&t), "$ cmd5");
        assert!(!newer(&t), "已在底部");
        for _ in 0..5 {
            assert!(older(&t));
        }
        assert_eq!(top(&t), "$ cmd0");
        assert!(!older(&t));
    }

    #[test]
    fn marks_follow_reflow() {
        let t = terminal(20, 5, 100);
        for i in 0..4 {
            feed(&t, format!("\x1b]133;A\x07$ cmd{i}\r\n").as_bytes());
            feed(&t, format!("{}\r\n", "x".repeat(35)).as_bytes());
        }
        feed(&t, b"\x1b]133;A\x07$ ");
        for cols in [8, 40, 13, 20] {
            t.resize(TermSize::new(cols, 5, 8, 16));
            t.scroll_to_bottom();
            // 一路向上，经过的都是提示符行，编号连续递减到 0；已在屏幕上的提示符不算
            let mut seen = Vec::new();
            while older(&t) {
                seen.push(top(&t));
            }
            let first: usize = seen[0].trim_start_matches("$ cmd").parse().unwrap();
            let expect: Vec<_> = (0..=first).rev().map(|i| format!("$ cmd{i}")).collect();
            assert_eq!(seen, expect, "{cols} 列");
        }
    }

    #[test]
    fn marks_scrolled_out_of_history_are_dropped() {
        let t = terminal(20, 5, 10);
        session(&t, 1, 30);
        assert!(!older(&t));
    }

    #[test]
    fn huge_chunk_cannot_be_trusted() {
        // 一次滚动超过回滚上限时锚点所在的行可能已被复用，只能丢掉旧标记
        let t = terminal(20, 5, 10);
        feed(&t, b"\x1b]133;A\x07$ old\r\n");
        feed(&t, "y\r\n".repeat(30).as_bytes());
        feed(&t, b"\x1b]133;A\x07$ ");
        assert!(!older(&t));

        let t = terminal(20, 5, 10);
        feed(&t, b"\x1b]133;A\x07$ old\r\n");
        feed(&t, "y\r\n".repeat(6).as_bytes());
        feed(&t, b"\x1b]133;A\x07$ ");
        assert!(older(&t));
        assert_eq!(top(&t), "$ old");
    }

    #[test]
    fn alternate_screen_leaves_marks_alone() {
        let t = terminal(20, 5, 100);
        session(&t, 3, 3);
        feed(&t, b"\x1b[?1049h");
        feed(&t, "full screen\r\n".repeat(20).as_bytes());
        assert!(!older(&t), "备用屏里不跳转");
        feed(&t, b"\x1b[?1049l\r\n\x1b]133;A\x07$ back");
        assert!(older(&t));
        assert_eq!(top(&t), "$ cmd2");
    }

    #[test]
    fn resize_in_alternate_screen_drops_marks() {
        let t = terminal(20, 5, 100);
        session(&t, 3, 3);
        feed(&t, b"\x1b[?1049h");
        t.resize(TermSize::new(12, 5, 8, 16));
        feed(&t, b"\x1b[?1049l");
        assert!(!older(&t));
    }

    #[test]
    fn clear_screen_keeps_earlier_prompts_reachable() {
        let t = terminal(20, 5, 100);
        session(&t, 2, 1);
        feed(&t, b"\x1b[H\x1b[2J\x1b]133;A\x07$ ");
        // 清屏前光标所在的那个提示符也被推进回滚区
        assert!(older(&t));
        assert_eq!(top(&t), "$");
        assert!(older(&t));
        assert_eq!(top(&t), "$ cmd1");
    }

    #[test]
    fn prompt_split_across_feeds() {
        let t = terminal(20, 5, 100);
        session(&t, 2, 6);
        feed(&t, b"\x1b]13");
        feed(&t, b"3;A\x07$ last");
        assert!(older(&t));
        assert_eq!(top(&t), "$ cmd1");
    }
}
