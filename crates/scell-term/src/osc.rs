//! 旁路扫描 alacritty_terminal 不处理的 OSC 序列（7 工作目录、133 提示符标记）。
//!
//! 与主解析器并行消费同一字节流；vte 底层 Parser 自带跨 chunk 状态，切分位置不影响结果。
//! vte 的 OSC 缓冲上限约 1 KiB，超长的 OSC 7 路径会被截断。

use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::TermEvent;

#[derive(Default)]
pub(crate) struct OscScanner {
    parser: Parser,
}

/// 主解析器按提示符标记分段喂入，每段结束后记录提示符所在行。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// 段尾在本次输入中的字节偏移。
    pub(crate) end: usize,
    /// 段尾是一个提示符开始标记（OSC 133;A）。
    pub(crate) prompt: bool,
    /// 这段输出最多能让多少行滚入回滚区，偏大无妨。
    pub(crate) max_scroll: usize,
}

impl OscScanner {
    /// `screen` 为当前的（列数, 行数），用于估计滚动行数。`segments` 覆盖整个 `bytes`，最后一段不是提示符。
    pub(crate) fn scan(
        &mut self,
        bytes: &[u8],
        screen: (usize, usize),
        out: &mut Vec<TermEvent>,
        segments: &mut Vec<Segment>,
    ) {
        let mut sink = Sink {
            out,
            prompt: false,
            lines: 0,
            chars: 0,
            screen_lines: screen.1,
        };
        let columns = screen.0.max(1);
        let mut segment = |end: usize, prompt: bool, sink: &mut Sink<'_>| {
            // 自动折行：宽字符占两列，按两列估
            let wraps = (sink.chars * 2).div_ceil(columns);
            segments.push(Segment {
                end,
                prompt,
                max_scroll: sink.lines + wraps,
            });
            sink.lines = 0;
            sink.chars = 0;
        };
        // OSC 在终止字节（BEL、CAN、SUB、ESC）处分派。按这些字节切开逐片喂入，分派总发生在片尾，偏移因此精确
        let mut start = 0;
        while start < bytes.len() {
            let end = bytes[start..]
                .iter()
                .position(|b| matches!(b, 0x07 | 0x18 | 0x1a | 0x1b))
                .map_or(bytes.len(), |i| start + i + 1);
            self.parser.advance(&mut sink, &bytes[start..end]);
            if std::mem::take(&mut sink.prompt) {
                segment(end, true, &mut sink);
            }
            start = end;
        }
        segment(bytes.len(), false, &mut sink);
    }
}

struct Sink<'a> {
    out: &'a mut Vec<TermEvent>,
    prompt: bool,
    /// 换行与滚动类控制序列可能滚动的行数。
    lines: usize,
    chars: usize,
    screen_lines: usize,
}

impl Perform for Sink<'_> {
    fn print(&mut self, _: char) {
        self.chars += 1;
    }

    fn execute(&mut self, byte: u8) {
        // LF、VT、FF
        if matches!(byte, 0x0a..=0x0c) {
            self.lines += 1;
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        // IND、NEL
        if intermediates.is_empty() && matches!(byte, b'D' | b'E') {
            self.lines += 1;
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _: bool, action: char) {
        if !intermediates.is_empty() {
            return;
        }
        let first = params.iter().next().and_then(|p| p.first().copied());
        let n = match action {
            // SU、DL（光标在首行时删除的行进入回滚区），一次最多一屏
            'S' | 'M' => usize::from(first.unwrap_or(1).max(1)).min(self.screen_lines),
            // ED 2：清屏前把屏幕内容推入回滚区
            'J' if first == Some(2) => self.screen_lines,
            _ => 0,
        };
        self.lines += n;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params {
            [b"7", rest @ ..] if !rest.is_empty() => {
                let url = rest
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>()
                    .join(";");
                self.out.push(TermEvent::WorkingDirectory(url));
            }
            [b"133", kind, ..] if kind.first() == Some(&b'A') => self.prompt = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (usize, usize) = (80, 24);

    fn scan_chunks(chunks: &[&[u8]]) -> (Vec<TermEvent>, Vec<Vec<Segment>>) {
        let mut scanner = OscScanner::default();
        let mut out = Vec::new();
        let segments = chunks
            .iter()
            .map(|c| {
                let mut segments = Vec::new();
                scanner.scan(c, SCREEN, &mut out, &mut segments);
                segments
            })
            .collect();
        (out, segments)
    }

    #[test]
    fn osc7_survives_any_split() {
        let seq = b"ls\r\n\x1b]7;file://host/home/a;b\x07$ ";
        let expect = vec![TermEvent::WorkingDirectory("file://host/home/a;b".into())];
        for split in 0..=seq.len() {
            let (a, b) = seq.split_at(split);
            assert_eq!(scan_chunks(&[a, b]).0, expect, "split at {split}");
        }
    }

    #[test]
    fn prompt_segments_end_at_the_terminator() {
        let bel = b"out\r\n\x1b]133;A\x07$ \x1b]133;C\x07";
        let (_, segments) = scan_chunks(&[bel]);
        let ends: Vec<_> = segments[0].iter().map(|s| (s.end, s.prompt)).collect();
        assert_eq!(ends, vec![(13, true), (bel.len(), false)]);

        // ST 终止时在 ESC 处分派，段尾停在 `\` 之前
        let st = b"\x1b]133;A\x1b\\$ ";
        let (_, segments) = scan_chunks(&[st]);
        assert_eq!(segments[0][0].end, 8);
        assert!(segments[0][0].prompt);
    }

    #[test]
    fn prompt_split_across_chunks_ends_in_the_second() {
        let (_, segments) = scan_chunks(&[b"\x1b]13", b"3;A\x07$ "]);
        assert_eq!(segments[0].len(), 1);
        assert_eq!((segments[1][0].end, segments[1][0].prompt), (4, true));
    }

    #[test]
    fn scroll_bound_counts_newlines_wraps_and_scroll_sequences() {
        let (_, segments) = scan_chunks(&[b"a\nb\n\x1bD\x1b[3S\x1b[2J\x1b[J"]);
        assert_eq!(segments[0][0].max_scroll, 2 + 1 + 3 + 24 + 1);
        let long = vec![b'x'; 200];
        let (_, segments) = scan_chunks(&[&long]);
        assert_eq!(segments[0][0].max_scroll, 5);
    }
}
