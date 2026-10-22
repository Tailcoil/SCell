use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, Bounds, Context, EntityInputHandler, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, ParentElement,
    Pixels, Render, Styled, UTF16Selection, WeakEntity, Window, actions, div,
};
use scell_term::input::{KeyEventKind, encode_key};
use scell_term::{RenderSnapshot, SearchDirection, TermSize, Terminal};

use crate::TerminalStyle;
use crate::clipboard;
use crate::element::TerminalElement;
use crate::glyphs::GlyphCache;
use crate::keys::{is_text_input, to_key_input};
use crate::metrics::CellMetrics;
use crate::mouse::MouseState;

actions!(
    terminal,
    [
        Copy,
        Paste,
        PreviousPrompt,
        NextPrompt,
        ScrollPageUp,
        ScrollPageDown,
        SendTab,
        SendBackTab
    ]
);

const KEY_CONTEXT: &str = "Terminal";
/// 进程已退出时的上下文，宿主可据此绑定重启等按键（如 `"Terminal && exited"`）。
const KEY_CONTEXT_EXITED: &str = "Terminal exited";
/// 程序切到备用屏（vim、less 等）时的上下文。翻页、跳转提示符应绑在 `"Terminal && !alt_screen"`，
/// 备用屏里这些按键照常交给程序。
const KEY_CONTEXT_ALT: &str = "Terminal alt_screen";

/// 绑定终端自身需要的按键。复制、粘贴等动作的快捷键由宿主绑定。
pub fn init(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        // 组件库的 Root 把 tab 绑定为焦点切换，按键绑定先于 key_down 处理，须在终端上下文里接管
        KeyBinding::new("tab", SendTab, ctx),
        KeyBinding::new("shift-tab", SendBackTab, ctx),
    ]);
}

/// 终端视图与外部世界的连接：写出编码后的输入、同步网格尺寸。
pub trait TerminalIo: 'static {
    fn write(&self, bytes: Vec<u8>);
    fn resize(&self, size: TermSize);
}

/// 粘贴前的确认，由宿主实现（如弹出对话框），确认后对该视图调用 [`TerminalView::paste`]。
pub type PasteGuard = Rc<dyn Fn(String, WeakEntity<TerminalView>, &mut Window, &mut App)>;

pub struct TerminalView {
    pub(crate) focus: FocusHandle,
    pub(crate) terminal: Arc<Terminal>,
    pub(crate) io: Box<dyn TerminalIo>,
    pub(crate) style: TerminalStyle,
    pub(crate) snapshot: RenderSnapshot,
    pub(crate) glyphs: GlyphCache,
    /// 按（样式, 缩放）缓存的单元格度量。
    pub(crate) metrics: Option<(CellMetrics, f32)>,
    /// 输入法预编辑文本，绘制在光标处，提交前不发送给程序。
    pub(crate) marked_text: Option<String>,
    /// 光标在窗口坐标系中的位置，供输入法定位候选窗。
    pub(crate) cursor_bounds: Option<Bounds<Pixels>>,
    pub(crate) mouse: MouseState,
    /// 选中后立即复制到剪贴板（Linux 的 PRIMARY 选区总会写入）。
    pub(crate) copy_on_select: bool,
    /// 有值时，可能被直接执行的多行粘贴先交给它确认。
    pub(crate) paste_guard: Option<PasteGuard>,
    pub(crate) exited: bool,
    pub(crate) blink: Blink,
}

/// 光标闪烁。只在获得焦点时闪烁，最近一次输入后持续一段时间，之后保持常亮，空闲时不再重绘。
#[derive(Default)]
pub(crate) struct Blink {
    /// 处于“灭”的半周期。
    pub(crate) off: bool,
    running: bool,
    /// 每次重置加一，旧的计时任务据此退出。
    epoch: u64,
    last_input: Option<Instant>,
}

const BLINK_INTERVAL: Duration = Duration::from_millis(530);
const BLINK_IDLE_STOP: Duration = Duration::from_secs(15);

impl TerminalView {
    pub fn new(
        terminal: Arc<Terminal>,
        io: impl TerminalIo,
        style: TerminalStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        terminal.set_palette(style.palette.clone());
        terminal.set_default_cursor(style.cursor_shape, style.cursor_blink);
        Self {
            focus: cx.focus_handle(),
            terminal,
            io: Box::new(io),
            style,
            snapshot: RenderSnapshot::default(),
            glyphs: GlyphCache::default(),
            metrics: None,
            marked_text: None,
            cursor_bounds: None,
            mouse: MouseState::default(),
            copy_on_select: false,
            paste_guard: None,
            exited: false,
            blink: Blink::default(),
        }
    }

    pub fn set_style(&mut self, style: TerminalStyle, cx: &mut Context<Self>) {
        self.terminal.set_palette(style.palette.clone());
        self.terminal
            .set_default_cursor(style.cursor_shape, style.cursor_blink);
        self.style = style;
        self.metrics = None;
        self.glyphs.invalidate();
        cx.notify();
    }

    pub fn terminal(&self) -> &Arc<Terminal> {
        &self.terminal
    }

    /// 标记进程已退出：保留屏幕内容，不再发送输入。
    pub fn set_exited(&mut self, exited: bool, cx: &mut Context<Self>) {
        self.exited = exited;
        cx.notify();
    }

    pub fn set_copy_on_select(&mut self, on: bool) {
        self.copy_on_select = on;
    }

    pub fn set_paste_guard(&mut self, guard: Option<PasteGuard>) {
        self.paste_guard = guard;
    }

    /// 直接粘贴，不经确认。
    pub fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        self.paste_text(text);
        cx.notify();
    }

    /// 来自用户操作的粘贴：程序未开启括号粘贴时，多行文本里的换行会被逐行执行，先交给确认。
    pub(crate) fn request_paste(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multiline = text.trim_end_matches(['\r', '\n']).contains(['\r', '\n']);
        match &self.paste_guard {
            Some(guard) if multiline && !self.terminal.input_mode().bracketed_paste => {
                guard.clone()(text, cx.entity().downgrade(), window, cx);
            }
            _ => self.paste(&text, cx),
        }
    }

    /// 发送键盘或粘贴输入：回到底部并清除选区。鼠标上报不走这里。
    pub(crate) fn send(&mut self, bytes: Vec<u8>) {
        self.terminal.scroll_to_bottom();
        self.terminal.clear_selection();
        self.blink.last_input = Some(Instant::now());
        self.blink.off = false;
        self.io.write(bytes);
    }

    /// 由绘制阶段调用：获得焦点且需要闪烁时启动计时，失焦时停止。
    pub(crate) fn update_blink(&mut self, focused: bool, blinking: bool, cx: &mut Context<Self>) {
        let wanted = focused && blinking && !self.exited;
        if !wanted {
            if self.blink.running {
                self.blink.running = false;
                self.blink.epoch += 1;
            }
            self.blink.off = false;
            // 重新获得焦点时从头闪烁
            self.blink.last_input = None;
            return;
        }
        if self.blink.running {
            return;
        }
        if self
            .blink
            .last_input
            .is_some_and(|t| t.elapsed() >= BLINK_IDLE_STOP)
        {
            return;
        }
        self.blink.running = true;
        self.blink.epoch += 1;
        self.blink.last_input.get_or_insert_with(Instant::now);
        let epoch = self.blink.epoch;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let keep = this.update(cx, |view, cx| {
                    if view.blink.epoch != epoch {
                        return false;
                    }
                    let idle = view
                        .blink
                        .last_input
                        .is_some_and(|t| t.elapsed() >= BLINK_IDLE_STOP);
                    view.blink.off = !view.blink.off && !idle;
                    cx.notify();
                    if idle {
                        view.blink.running = false;
                    }
                    !idle
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.exited {
            return;
        }
        let mode = self.terminal.input_mode();
        // 可打印文本交给输入法通道，否则会与 replace_text_in_range 重复发送；
        // 程序要求 kitty 协议“全部按键转义”时例外，此时文本键也要编码（预编辑中仍交给输入法）
        if is_text_input(&event.keystroke, event.prefer_character_input)
            && (!mode.all_keys_as_escapes() || self.marked_text.is_some())
        {
            return;
        }
        let kind = if event.is_held {
            KeyEventKind::Repeat
        } else {
            KeyEventKind::Press
        };
        if self.send_key(&event.keystroke, kind, cx) {
            cx.stop_propagation();
        }
    }

    /// 编码并发送一个按键；无法编码时返回 `false`。
    fn send_key(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        kind: KeyEventKind,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.terminal.input_mode();
        let Some(bytes) = to_key_input(keystroke, kind).and_then(|input| encode_key(&input, mode))
        else {
            return false;
        };
        self.send(bytes);
        cx.notify();
        true
    }

    fn send_bound_key(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.exited {
            return;
        }
        if let Ok(keystroke) = gpui_kit::Keystroke::parse(key) {
            self.send_key(&keystroke, KeyEventKind::Press, cx);
        }
    }

    /// 只在程序通过 kitty 协议要求上报事件类型时发送释放。
    fn on_key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let mode = self.terminal.input_mode();
        if self.exited || !mode.reports_key_events() || self.marked_text.is_some() {
            return;
        }
        if let Some(bytes) = to_key_input(&event.keystroke, KeyEventKind::Release)
            .and_then(|input| encode_key(&input, mode))
        {
            // 释放不算用户输入：不滚动到底部、不清除选区
            self.io.write(bytes);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.selection_text() {
            clipboard::write(text, cx);
        }
    }

    fn paste_clipboard(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = clipboard::read(cx) {
            self.request_paste(text, window, cx);
        }
    }

    fn jump_to_prompt(&mut self, direction: SearchDirection, cx: &mut Context<Self>) {
        if self.terminal.scroll_to_prompt(direction) {
            cx.notify();
        }
    }

    /// 按页滚动回滚区，正数向上。
    fn scroll_pages(&mut self, pages: i32, cx: &mut Context<Self>) {
        self.terminal
            .scroll(pages * i32::from(self.terminal.size().rows));
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context(if self.exited {
                KEY_CONTEXT_EXITED
            } else if self.terminal.input_mode().alt_screen {
                KEY_CONTEXT_ALT
            } else {
                KEY_CONTEXT
            })
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_modifiers_changed(cx.listener(|view, e: &ModifiersChangedEvent, _, cx| {
                view.update_link(None, e.modifiers.secondary(), cx)
            }))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_action(cx.listener(|view, _: &PreviousPrompt, _, cx| {
                view.jump_to_prompt(SearchDirection::Older, cx)
            }))
            .on_action(cx.listener(|view, _: &NextPrompt, _, cx| {
                view.jump_to_prompt(SearchDirection::Newer, cx)
            }))
            .on_action(cx.listener(|view, _: &ScrollPageUp, _, cx| view.scroll_pages(1, cx)))
            .on_action(cx.listener(|view, _: &ScrollPageDown, _, cx| view.scroll_pages(-1, cx)))
            .on_action(cx.listener(|view, _: &SendTab, _, cx| view.send_bound_key("tab", cx)))
            .on_action(
                cx.listener(|view, _: &SendBackTab, _, cx| view.send_bound_key("shift-tab", cx)),
            )
            .child(TerminalElement::new(cx.entity(), self.focus.clone()))
    }
}

/// 终端没有可编辑的文本缓冲区：只把预编辑文本作为“标记区间”，提交文本直接发送给程序。
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked_text.as_ref()?;
        let utf16: Vec<u16> = marked.encode_utf16().collect();
        let range = range.start.min(utf16.len())..range.end.min(utf16.len());
        *adjusted_range = Some(range.clone());
        String::from_utf16(&utf16[range]).ok()
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_ref()
            .map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() && !self.exited {
            self.send(text.as_bytes().to_vec());
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
