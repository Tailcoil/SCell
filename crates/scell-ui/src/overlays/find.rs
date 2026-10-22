//! 终端内查找条。随输入增量查找，Enter 跳到更早的匹配，Shift+Enter 跳到更新的匹配，Esc 关闭。

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Selectable, Sizable, h_flex};
use gpui_kit::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Subscription, Window,
};
use rust_i18n::t;
use scell_term::{SearchDirection, SearchQuery};
use scell_term_view::TerminalView;

pub(crate) enum FindEvent {
    Close,
}

pub(crate) struct FindBar {
    input: Entity<InputState>,
    target: Entity<TerminalView>,
    regex: bool,
    case_sensitive: bool,
    /// 有输入但没有匹配，或正则无效。
    no_match: bool,
    _input_events: Subscription,
}

impl EventEmitter<FindEvent> for FindBar {}

impl FindBar {
    pub(crate) fn new(
        target: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("find.placeholder")));
        let input_events = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Change => this.refresh(cx),
            InputEvent::PressEnter { shift, .. } => {
                let direction = if *shift {
                    SearchDirection::Newer
                } else {
                    SearchDirection::Older
                };
                this.step(direction, cx);
            }
            _ => {}
        });
        Self {
            input,
            target,
            regex: false,
            case_sensitive: false,
            no_match: false,
            _input_events: input_events,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 查找条件变化后重新查找，并定位到离底部最近的匹配。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = SearchQuery {
            pattern: self.input.read(cx).value().to_string(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
        };
        let terminal = self.target.read(cx).terminal().clone();
        self.no_match = match terminal.set_search(&query) {
            Ok(()) if query.pattern.is_empty() => false,
            // 只扫了最近一段且没找到时结果未知，先不标红，按 Enter 再全量查找
            Ok(()) => terminal.search_while_typing() == Some(false),
            Err(_) => true,
        };
        self.target.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn step(&mut self, direction: SearchDirection, cx: &mut Context<Self>) {
        let terminal = self.target.read(cx).terminal().clone();
        if !self.input.read(cx).value().is_empty() {
            self.no_match = !terminal.search_step(direction);
        }
        self.target.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.target.read(cx).terminal().clear_search();
        self.target.update(cx, |_, cx| cx.notify());
        cx.emit(FindEvent::Close);
    }
}

impl Render for FindBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let toggle = |id: &'static str, icon: Lucide, on: bool, tip: String| {
            Button::new(id)
                .icon(icon)
                .ghost()
                .xsmall()
                .selected(on)
                .tooltip(tip)
        };
        let mut input = Input::new(&self.input)
            .small()
            .w(crate::ui::metrics::FIND_INPUT_WIDTH);
        if self.no_match {
            input = input.text_color(theme.danger);
        }
        h_flex()
            .gap_0p5()
            .p_1()
            .rounded(theme.radius_lg)
            .bg(theme.popover)
            .shadow_md()
            .on_action(cx.listener(|this, _: &Escape, _, cx| this.close(cx)))
            .child(input)
            .child(
                toggle(
                    "find-case",
                    Lucide::CaseSensitive,
                    self.case_sensitive,
                    t!("find.case_sensitive").to_string(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.case_sensitive = !this.case_sensitive;
                    this.refresh(cx);
                })),
            )
            .child(
                toggle(
                    "find-regex",
                    Lucide::Regex,
                    self.regex,
                    t!("find.regex").to_string(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.regex = !this.regex;
                    this.refresh(cx);
                })),
            )
            .child(
                Button::new("find-older")
                    .icon(Lucide::ChevronUp)
                    .ghost()
                    .xsmall()
                    .tooltip(t!("find.older"))
                    .on_click(cx.listener(|this, _, _, cx| this.step(SearchDirection::Older, cx))),
            )
            .child(
                Button::new("find-newer")
                    .icon(Lucide::ChevronDown)
                    .ghost()
                    .xsmall()
                    .tooltip(t!("find.newer"))
                    .on_click(cx.listener(|this, _, _, cx| this.step(SearchDirection::Newer, cx))),
            )
            .child(
                Button::new("find-close")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .tooltip(t!("find.close"))
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
    }
}
