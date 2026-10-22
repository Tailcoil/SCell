//! 回滚区查找。查找状态挂在终端上；快照时标出可见区域内的全部匹配与当前匹配。

use std::ops::RangeInclusive;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};

use crate::Terminal;

/// 边输入边查找时最多扫描的行数，与 Alacritty 相同。
const TYPING_SEARCH_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: String,
    /// 为假时按字面量匹配。
    pub regex: bool,
    /// 为假时大小写不敏感；为真时始终区分。
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// 向上，即更早的输出。
    Older,
    Newer,
}

pub(crate) struct SearchState {
    pub(crate) regex: RegexSearch,
    pub(crate) focused: Option<RangeInclusive<Point>>,
}

impl Terminal {
    /// 设置查找条件；空模式清除查找。正则无效时返回错误信息，原有查找保持不变。
    pub fn set_search(&self, query: &SearchQuery) -> Result<(), String> {
        if query.pattern.is_empty() {
            self.clear_search();
            return Ok(());
        }
        let body = if query.regex {
            query.pattern.clone()
        } else {
            escape(&query.pattern)
        };
        let flags = if query.case_sensitive {
            "(?-i)"
        } else {
            "(?i)"
        };
        let regex = RegexSearch::new(&format!("{flags}{body}")).map_err(|e| e.to_string())?;
        *self.search.lock() = Some(SearchState {
            regex,
            focused: None,
        });
        Ok(())
    }

    pub fn clear_search(&self) {
        *self.search.lock() = None;
    }

    /// 跳到下一个匹配并滚动到可见处，到头后绕回。返回是否找到。
    pub fn search_step(&self, direction: SearchDirection) -> bool {
        self.step(direction, None).unwrap_or(false)
    }

    /// 查找词刚变化时的首次查找：从底部向上只扫描最近 1000 行，回滚区很大时逐字输入也不卡顿。
    /// 扫描没覆盖整个回滚区又没找到时返回 `None`，表示结果未知，之后用 [`Self::search_step`] 全量查找。
    pub fn search_while_typing(&self) -> Option<bool> {
        self.step(SearchDirection::Older, Some(TYPING_SEARCH_LINES))
    }

    fn step(&self, direction: SearchDirection, max_lines: Option<usize>) -> Option<bool> {
        // 加锁顺序与快照一致：先终端后查找
        let mut term = self.term.lock();
        let mut search = self.search.lock();
        let state = search.as_mut()?;
        let found = next_match(&term, state, direction, max_lines);
        let exhaustive = max_lines.is_none_or(|max| max + 1 >= term.total_lines());
        if let Some(m) = &found {
            term.scroll_to_point(*m.start());
        }
        state.focused = found;
        (state.focused.is_some() || exhaustive).then_some(state.focused.is_some())
    }
}

fn next_match<T>(
    term: &Term<T>,
    state: &mut SearchState,
    direction: SearchDirection,
    max_lines: Option<usize>,
) -> Option<RangeInclusive<Point>> {
    let offset = term.grid().display_offset() as i32;
    let (origin, dir, side) = match (&state.focused, direction) {
        (Some(m), SearchDirection::Older) => (*m.start(), Direction::Left, Side::Left),
        (Some(m), SearchDirection::Newer) => (*m.end(), Direction::Right, Side::Right),
        // 首次查找从可见区域底部向上、或从顶部向下
        (None, SearchDirection::Older) => (
            Point::new(
                Line(term.screen_lines() as i32 - 1 - offset),
                term.last_column(),
            ),
            Direction::Left,
            Side::Right,
        ),
        (None, SearchDirection::Newer) => (
            Point::new(Line(-offset), Column(0)),
            Direction::Right,
            Side::Left,
        ),
    };
    let origin = match (&state.focused, dir) {
        // 从当前匹配之外开始，否则会再次命中自己
        (Some(_), Direction::Left) => origin.sub(term, Boundary::None, 1),
        (Some(_), Direction::Right) => origin.add(term, Boundary::None, 1),
        (None, _) => origin,
    };
    term.search_next(&mut state.regex, origin, dir, side, max_lines)
}

/// 可见区域内的全部匹配（网格坐标）。
pub(crate) fn visible_matches<T>(
    term: &Term<T>,
    state: &mut SearchState,
) -> Vec<RangeInclusive<Point>> {
    let offset = term.grid().display_offset() as i32;
    let start = Point::new(Line(-offset), Column(0));
    let end = Point::new(
        Line(term.screen_lines() as i32 - 1 - offset),
        term.last_column(),
    );
    RegexIter::new(start, end, Direction::Right, term, &mut state.regex).collect()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellAttrs, RenderSnapshot, TermSize};

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery {
            pattern: pattern.into(),
            regex: false,
            case_sensitive: false,
        }
    }

    fn marked(t: &Terminal, row: usize, attr: CellAttrs) -> String {
        let mut snap = RenderSnapshot::default();
        t.snapshot(&mut snap);
        snap.row(row)
            .iter()
            .map(|c| if c.attrs.contains(attr) { c.ch } else { '.' })
            .collect::<String>()
            .trim_end_matches('.')
            .to_owned()
    }

    #[test]
    fn literal_search_marks_all_and_steps_upward() {
        let t = Terminal::new(TermSize::new(20, 3, 8, 16), Default::default(), 100);
        t.feed(b"a.c abc A.C\r\nx a.c", &mut Vec::new());
        t.set_search(&query("a.c")).unwrap();
        // 字面量匹配：abc 不算；默认不区分大小写
        assert_eq!(marked(&t, 0, CellAttrs::MATCH), "a.c.....A.C");
        assert!(t.search_step(SearchDirection::Older));
        assert_eq!(marked(&t, 1, CellAttrs::FOCUSED_MATCH), "..a.c");
        assert!(t.search_step(SearchDirection::Older));
        assert_eq!(marked(&t, 0, CellAttrs::FOCUSED_MATCH), "........A.C");
    }

    #[test]
    fn regex_and_case_sensitive() {
        let t = Terminal::new(TermSize::new(20, 2, 8, 16), Default::default(), 100);
        t.feed(b"Err err e1", &mut Vec::new());
        t.set_search(&SearchQuery {
            pattern: "E\\w+".into(),
            regex: true,
            case_sensitive: true,
        })
        .unwrap();
        assert_eq!(marked(&t, 0, CellAttrs::MATCH), "Err");
        assert!(
            t.set_search(&SearchQuery {
                pattern: "(".into(),
                regex: true,
                case_sensitive: false,
            })
            .is_err()
        );
    }

    #[test]
    fn empty_pattern_clears() {
        let t = Terminal::default();
        t.feed(b"abc", &mut Vec::new());
        t.set_search(&query("b")).unwrap();
        t.set_search(&query("")).unwrap();
        assert!(!t.search_step(SearchDirection::Older));
        assert_eq!(marked(&t, 0, CellAttrs::MATCH), "");
    }
}
