use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{self, ClipboardType, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Processor, StdSyncHandler};
use parking_lot::Mutex;

use crate::input::{InputMode, MouseEncoding, MouseMode};
use crate::osc::{OscScanner, Segment};
use crate::prompt::PromptMarks;
use crate::search::SearchState;
use crate::snapshot::RenderSnapshot;
use crate::{ClipboardKind, CursorShape, Palette, SearchDirection, TermEvent, TermSize};

/// 单个终端的状态机。可在线程间共享：会话层的 pump 调 [`Terminal::feed`]，
/// 渲染层在绘制前调 [`Terminal::snapshot`]。
pub struct Terminal {
    pub(crate) term: FairMutex<Term<Listener>>,
    parser: Mutex<Parsers>,
    pending: Arc<Mutex<Vec<Event>>>,
    /// 与 `term` 同时持有时必须先锁 `term`：`feed` 在持 `term` 期间转换事件要读它。
    state: Mutex<State>,
    /// 与 `term` 同时持有时必须先锁 `term`；不要在持有它时去锁 `state`。
    prompts: Mutex<PromptMarks>,
    pub(crate) search: Mutex<Option<SearchState>>,
    /// 首次识别链接时编译。
    pub(crate) url_regex: Mutex<Option<RegexSearch>>,
}

struct Parsers {
    vt: Processor<StdSyncHandler>,
    osc: OscScanner,
    /// 复用的分段缓冲。
    segments: Vec<Segment>,
}

struct State {
    size: TermSize,
    palette: Palette,
    /// alacritty 不提供读取配置的接口，修改选项时在副本上改完整体替换。
    config: term::Config,
}

/// alacritty 在持锁解析期间回调此监听器，这里只入队，出锁前统一转换。
#[derive(Clone)]
pub(crate) struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().push(event);
    }
}

impl Terminal {
    pub fn new(size: TermSize, palette: Palette, scrollback: usize) -> Self {
        let pending = Arc::new(Mutex::new(Vec::new()));
        let config = term::Config {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            ..Default::default()
        };
        Self {
            term: FairMutex::new(Term::new(config.clone(), &size, Listener(pending.clone()))),
            parser: Mutex::new(Parsers {
                vt: Processor::new(),
                osc: OscScanner::default(),
                segments: Vec::new(),
            }),
            pending,
            state: Mutex::new(State {
                size,
                palette,
                config,
            }),
            prompts: Mutex::new(PromptMarks::new(scrollback)),
            search: Mutex::new(None),
            url_regex: Mutex::new(None),
        }
    }

    /// 喂入一段已解码为 UTF-8 的输出，产生的事件追加到 `events`。
    pub fn feed(&self, bytes: &[u8], events: &mut Vec<TermEvent>) {
        let mut parsers = self.parser.lock();
        let Parsers { vt, osc, segments } = &mut *parsers;
        let size = self.state.lock().size;
        segments.clear();
        osc.scan(
            bytes,
            (usize::from(size.cols), usize::from(size.rows)),
            events,
            segments,
        );
        let mut term = self.term.lock_unfair();
        let mut prompts = self.prompts.lock();
        let mut start = 0;
        // 在每个提示符标记处停下，记下此刻光标所在的行
        for segment in segments.iter() {
            vt.advance(&mut *term, &bytes[start..segment.end]);
            // 同步更新期间字节还压在解析器里，光标位置不是标记处的
            let deferred = vt.sync_timeout().sync_timeout().is_some();
            prompts.track(&term, segment.max_scroll, deferred);
            if segment.prompt && !deferred {
                prompts.add(&term);
            }
            start = segment.end;
        }
        drop(prompts);
        self.drain_events(&term, events);
    }

    /// DEC 2026 同步更新的截止时间；到期后调用方应执行 [`Terminal::flush_sync`]。
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.lock().vt.sync_timeout().sync_timeout()
    }

    pub fn flush_sync(&self, events: &mut Vec<TermEvent>) {
        let mut parsers = self.parser.lock();
        let mut term = self.term.lock_unfair();
        parsers.vt.stop_sync(&mut *term);
        self.prompts.lock().track(&term, 0, false);
        self.drain_events(&term, events);
    }

    pub fn resize(&self, size: TermSize) {
        let mut term = self.term.lock();
        let mut state = self.state.lock();
        if state.size == size {
            return;
        }
        state.size = size;
        let mut prompts = self.prompts.lock();
        let saved = prompts.save(&term);
        term.resize(size);
        prompts.restore(&term, saved);
    }

    pub fn size(&self) -> TermSize {
        self.state.lock().size
    }

    pub fn set_palette(&self, palette: Palette) {
        self.state.lock().palette = palette;
    }

    /// 程序未通过 DECSCUSR 指定时使用的光标样式。
    pub fn set_default_cursor(&self, shape: CursorShape, blinking: bool) {
        let shape = match shape {
            CursorShape::Block | CursorShape::HollowBlock => ansi::CursorShape::Block,
            CursorShape::Underline => ansi::CursorShape::Underline,
            CursorShape::Beam => ansi::CursorShape::Beam,
        };
        let style = ansi::CursorStyle { shape, blinking };
        let mut term = self.term.lock();
        let mut state = self.state.lock();
        if state.config.default_cursor_style == style {
            return;
        }
        state.config.default_cursor_style = style;
        term.set_options(state.config.clone());
    }

    /// 按行滚动回滚区，正数向上（更早的内容）。
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    /// 滚动到距底部 `offset` 行处，超出回滚区时停在顶部。
    pub fn scroll_to(&self, offset: usize) {
        let mut term = self.term.lock();
        let current = term.grid().display_offset() as i64;
        let delta = offset as i64 - current;
        term.scroll_display(Scroll::Delta(
            delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32
        ));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// 把上一个（[`SearchDirection::Older`]）或下一个提示符滚到视口顶部，提示符来自 shell 发出的
    /// OSC 133 标记。下一个提示符已在最后一屏时滚到底部。没有可去的位置时返回 `false`。
    pub fn scroll_to_prompt(&self, direction: SearchDirection) -> bool {
        let mut term = self.term.lock();
        let prompts = self.prompts.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        let offset = term.grid().display_offset() as i32;
        let top = -offset;
        let target = {
            let mut lines = prompts.lines();
            match direction {
                SearchDirection::Older => lines.rev().find(|&l| l < top),
                SearchDirection::Newer => lines.find(|&l| l > top),
            }
        };
        let to = match (target, direction) {
            (Some(line), _) => (-line).max(0),
            (None, SearchDirection::Newer) => 0,
            (None, SearchDirection::Older) => return false,
        };
        if to == offset {
            return false;
        }
        drop(prompts);
        term.scroll_display(Scroll::Delta(to - offset));
        true
    }

    pub fn input_mode(&self) -> InputMode {
        InputMode::from_term_mode(*self.term.lock().mode())
    }

    /// 把可见区域写入复用缓冲，避免每帧分配。
    pub fn snapshot(&self, out: &mut RenderSnapshot) {
        let palette = self.state.lock().palette.clone();
        let term = self.term.lock();
        let mut search = self.search.lock();
        out.fill(&term, &palette, search.as_mut());
    }

    fn drain_events(&self, term: &Term<Listener>, events: &mut Vec<TermEvent>) {
        let mut pending = self.pending.lock();
        if pending.is_empty() {
            return;
        }
        let state = self.state.lock();
        for event in pending.drain(..) {
            let mapped = match event {
                Event::PtyWrite(s) => TermEvent::PtyWrite(s.into_bytes()),
                Event::Title(t) => TermEvent::Title(t),
                Event::ResetTitle => TermEvent::ResetTitle,
                Event::Bell => TermEvent::Bell,
                Event::ClipboardStore(kind, text) => {
                    TermEvent::ClipboardStore(clipboard_kind(kind), text)
                }
                Event::ColorRequest(index, format) => {
                    let rgb = term.colors()[index]
                        .map(Into::into)
                        .unwrap_or_else(|| state.palette.by_index(index));
                    TermEvent::PtyWrite(
                        format(ansi::Rgb {
                            r: rgb.r,
                            g: rgb.g,
                            b: rgb.b,
                        })
                        .into_bytes(),
                    )
                }
                Event::TextAreaSizeRequest(format) => {
                    TermEvent::PtyWrite(format(state.size.into()).into_bytes())
                }
                // OSC 52 读取默认关闭（Osc52::OnlyCopy），其余事件与渲染层无关
                _ => continue,
            };
            events.push(mapped);
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new(TermSize::default(), Palette::default(), 10_000)
    }
}

fn clipboard_kind(kind: ClipboardType) -> ClipboardKind {
    match kind {
        ClipboardType::Clipboard => ClipboardKind::Clipboard,
        ClipboardType::Selection => ClipboardKind::Selection,
    }
}

impl InputMode {
    fn from_term_mode(mode: TermMode) -> Self {
        Self {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            mouse: if mode.contains(TermMode::MOUSE_MOTION) {
                MouseMode::Motion
            } else if mode.contains(TermMode::MOUSE_DRAG) {
                MouseMode::Drag
            } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
                MouseMode::Click
            } else {
                MouseMode::Off
            },
            mouse_encoding: if mode.contains(TermMode::SGR_MOUSE) {
                MouseEncoding::Sgr
            } else if mode.contains(TermMode::UTF8_MOUSE) {
                MouseEncoding::Utf8
            } else {
                MouseEncoding::Default
            },
            // TermMode 从第 18 位起按协议顺序存放五个标志
            kitty_flags: (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits()
                >> TermMode::DISAMBIGUATE_ESC_CODES.bits().trailing_zeros(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &Terminal, bytes: &[u8]) -> Vec<TermEvent> {
        let mut ev = Vec::new();
        t.feed(bytes, &mut ev);
        ev
    }

    #[test]
    fn answers_device_status_report() {
        let t = Terminal::default();
        feed(&t, b"abc");
        let ev = feed(&t, b"\x1b[6n");
        assert_eq!(ev, vec![TermEvent::PtyWrite(b"\x1b[1;4R".to_vec())]);
    }

    #[test]
    fn title_and_cwd_events() {
        let t = Terminal::default();
        let ev = feed(&t, b"\x1b]2;hello\x07\x1b]7;file://h/tmp\x07");
        assert!(ev.contains(&TermEvent::Title("hello".into())));
        assert!(ev.contains(&TermEvent::WorkingDirectory("file://h/tmp".into())));
    }

    #[test]
    fn answers_color_query_from_palette() {
        let t = Terminal::default();
        let ev = feed(&t, b"\x1b]11;?\x07");
        let bg = Palette::default().background;
        let expect = format!(
            "\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
            bg.r, bg.g, bg.b
        );
        assert_eq!(ev, vec![TermEvent::PtyWrite(expect.into_bytes())]);
    }

    #[test]
    fn grid_size_follows_resize() {
        let t = Terminal::default();
        t.resize(TermSize::new(100, 30, 8, 16));
        let mut snap = RenderSnapshot::default();
        t.snapshot(&mut snap);
        assert_eq!((snap.cols, snap.rows), (100, 30));
        assert_eq!(
            alacritty_terminal::grid::Dimensions::columns(&*t.term.lock()),
            100
        );
    }

    #[test]
    fn resize_while_feeding_queries_does_not_deadlock() {
        let t = Arc::new(Terminal::default());
        let feeder = {
            let t = t.clone();
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    feed(&t, b"\x1b[6n\x1b]2;x\x07");
                }
            })
        };
        for i in 0..2000u16 {
            t.resize(TermSize::new(80 + i % 2, 24, 8, 16));
            t.set_default_cursor(
                if i % 2 == 0 {
                    CursorShape::Beam
                } else {
                    CursorShape::Block
                },
                false,
            );
        }
        feeder.join().unwrap();
    }
}
