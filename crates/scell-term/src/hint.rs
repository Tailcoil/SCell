//! 链接识别：OSC 8 超链接优先，其次按正则识别输出中的 URL。

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};

use crate::{Terminal, ViewportPoint};

/// 与 Alacritty 默认提示规则相同的 URL 模式。
const URL_PATTERN: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`]+";

/// 允许交给系统打开的 OSC 8 链接协议；其余（如 `javascript:`、自定义协议）一律忽略。
const SAFE_SCHEMES: &[&str] = &["http://", "https://", "mailto:", "file://", "ftp://"];

/// 视口中的一段链接。`start`/`end` 为闭区间，可跨行（软换行的长 URL）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub start: ViewportPoint,
    pub end: ViewportPoint,
}

impl Link {
    /// 视口中 `(row, col)` 是否落在链接内。
    pub fn contains(&self, row: usize, col: usize) -> bool {
        (row, col) >= (self.start.row, self.start.col) && (row, col) <= (self.end.row, self.end.col)
    }
}

impl Terminal {
    /// 视口中某个单元格上的链接。
    pub fn link_at(&self, at: ViewportPoint) -> Option<Link> {
        // 加锁顺序：先终端后正则
        let term = self.term.lock();
        let offset = term.grid().display_offset();
        if at.row >= term.screen_lines() || at.col >= term.columns() {
            return None;
        }
        let point = Point::new(Line(at.row as i32 - offset as i32), Column(at.col));
        let (uri, start, end) = match hyperlink_at(&term, point) {
            Some(found) => found,
            None => {
                let mut regex = self.url_regex.lock();
                let regex = match regex.as_mut() {
                    Some(r) => r,
                    None => regex.insert(RegexSearch::new(URL_PATTERN).ok()?),
                };
                url_at(&term, regex, point)?
            }
        };
        let to_viewport = |p: Point| ViewportPoint {
            row: (p.line.0 + offset as i32).clamp(0, term.screen_lines() as i32 - 1) as usize,
            col: p.column.0,
            right_half: false,
        };
        Some(Link {
            uri,
            start: to_viewport(start),
            end: to_viewport(end),
        })
    }
}

fn hyperlink_at<T>(term: &Term<T>, point: Point) -> Option<(String, Point, Point)> {
    let grid = term.grid();
    let link = grid[point].hyperlink()?;
    let uri = link.uri();
    if !SAFE_SCHEMES.iter().any(|s| uri.starts_with(s)) {
        return None;
    }
    // 只在可见区域内扩展，同一链接的相邻单元格属于同一段
    let top = Point::new(Line(-(grid.display_offset() as i32)), Column(0));
    let bottom = Point::new(
        Line(term.screen_lines() as i32 - 1 - grid.display_offset() as i32),
        term.last_column(),
    );
    let same = |p: Point| grid[p].hyperlink().as_ref() == Some(&link);
    let mut start = point;
    while start > top {
        let prev = start.sub(term, Boundary::Grid, 1);
        if !same(prev) {
            break;
        }
        start = prev;
    }
    let mut end = point;
    while end < bottom {
        let next = end.add(term, Boundary::Grid, 1);
        if !same(next) {
            break;
        }
        end = next;
    }
    Some((uri.to_owned(), start, end))
}

fn url_at<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    point: Point,
) -> Option<(String, Point, Point)> {
    let start = term.line_search_left(point);
    let end = term.line_search_right(point);
    let found =
        RegexIter::new(start, end, Direction::Right, term, regex).find(|m| m.contains(&point))?;
    let mut uri = term.bounds_to_string(*found.start(), *found.end());
    // 句末标点与成对括号的右半边通常不属于 URL
    let mut end = *found.end();
    while let Some(last) = uri.chars().last() {
        let trailing = matches!(last, '.' | ',' | ':' | ';' | '!' | '?' | '\'')
            || (last == ')' && uri.matches('(').count() < uri.matches(')').count());
        if !trailing || end == point {
            break;
        }
        uri.pop();
        end = end.sub(term, Boundary::Grid, 1);
    }
    Some((uri, *found.start(), end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Palette, TermSize};

    fn term(text: &str) -> Terminal {
        let t = Terminal::new(
            TermSize {
                cols: 40,
                rows: 4,
                ..TermSize::default()
            },
            Palette::default(),
            100,
        );
        t.feed(text.as_bytes(), &mut Vec::new());
        t
    }

    fn at(row: usize, col: usize) -> ViewportPoint {
        ViewportPoint {
            row,
            col,
            right_half: false,
        }
    }

    #[test]
    fn finds_plain_url_and_trims_punctuation() {
        let t = term("see https://example.com/a_(b). ok");
        let link = t.link_at(at(0, 10)).unwrap();
        assert_eq!(link.uri, "https://example.com/a_(b)");
        assert_eq!((link.start.col, link.end.col), (4, 28));
        assert!(t.link_at(at(0, 1)).is_none());
    }

    #[test]
    fn follows_soft_wrap() {
        // 40 列，URL 从第 30 列开始，折到第二行
        let t = term(&format!(
            "{}https://example.com/very/long/path",
            " ".repeat(30)
        ));
        let link = t.link_at(at(1, 2)).unwrap();
        assert_eq!(link.uri, "https://example.com/very/long/path");
        assert_eq!((link.start.row, link.start.col), (0, 30));
        assert_eq!(link.end.row, 1);
    }

    #[test]
    fn osc8_hyperlink_wins_and_unsafe_scheme_is_ignored() {
        let t = term(
            "\x1b]8;;https://scell.dev\x1b\\docs\x1b]8;;\x1b\\ \x1b]8;;javascript:x\x1b\\bad\x1b]8;;\x1b\\",
        );
        let link = t.link_at(at(0, 1)).unwrap();
        assert_eq!(link.uri, "https://scell.dev");
        assert_eq!((link.start.col, link.end.col), (0, 3));
        assert!(t.link_at(at(0, 6)).is_none());
    }
}
