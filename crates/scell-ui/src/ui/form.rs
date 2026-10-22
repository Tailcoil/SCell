//! 设置页与主机编辑共用的版式：分节卡片、“标签—控件”行、下拉选择，以及与数据双向绑定的输入框。
//!
//! 窄宽度下的规则：文字不折行（放不下显示省略号）；控件的固定宽度只是上限，可随容器收缩；
//! 一行放不下时控件折到下一行，而不是溢出。

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext, Div, ElementId, Entity, InteractiveElement, IntoElement,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::FluentBuilder, px,
};

pub(crate) use super::metrics::CONTROL_WIDTH;
use super::metrics::FORM_ROW_HEIGHT;

/// 选项多于此数时下拉菜单可滚动。
const SCROLLABLE_OPTIONS: usize = 12;

/// 一节设置：可选的小标题，下面是一张圆角卡片，行与行之间是淡分隔线。
pub(crate) fn section(
    title: Option<SharedString>,
    rows: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let divider = theme.border.opacity(0.5);
    let mut card = v_flex().rounded(theme.radius_lg).bg(theme.tokens.group_box);
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            card = card.child(div().h(px(1.)).mx_4().bg(divider));
        }
        card = card.child(row);
    }
    v_flex()
        .gap_2()
        .children(title.map(|title| {
            div()
                .px_1()
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground)
                .child(title)
        }))
        .child(card)
        .into_any_element()
}

/// 左标签、右控件的一行。标签与控件放不下时控件折到下一行并靠右。
pub(crate) fn row(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    h_flex()
        .flex_wrap()
        .min_h(FORM_ROW_HEIGHT)
        .px_4()
        .py_2()
        .gap_x_4()
        .gap_y_2()
        .child(
            div()
                .flex_1()
                .text_sm()
                .whitespace_nowrap()
                .child(label.into()),
        )
        .child(
            h_flex()
                .ml_auto()
                .max_w_full()
                .min_w_0()
                .justify_end()
                .child(control),
        )
}

/// 下拉选择按钮：`options` 为（值, 显示名），按钮宽度随内容。
pub(crate) fn select<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    current: T,
    options: Vec<(T, SharedString)>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    dropdown(id, current, options, None, on_pick)
}

/// 与输入框等宽的下拉选择，用于表单。
pub(crate) fn select_field<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    current: T,
    options: Vec<(T, SharedString)>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    dropdown(id, current, options, Some(CONTROL_WIDTH), on_pick)
}

/// 分段选择，选项少（两到四个）且需要一眼看到全部时代替下拉。
pub(crate) fn segmented<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    current: T,
    options: Vec<(T, SharedString)>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let selected = options.iter().position(|(v, _)| *v == current).unwrap_or(0);
    let labels: Vec<SharedString> = options.iter().map(|(_, l)| l.clone()).collect();
    let values: Vec<T> = options.into_iter().map(|(v, _)| v).collect();
    TabBar::new(id)
        .segmented()
        .small()
        .selected_index(selected)
        .children(labels.into_iter().map(|l| Tab::new().label(l)))
        .on_click(move |ix: &usize, window, cx| {
            if let Some(value) = values.get(*ix) {
                on_pick(value.clone(), window, cx);
            }
        })
}

/// 图标在上、文字在下的分段选择，各段等宽；比横排的图标加文字窄，五个选项也能放进窄栏。
pub(crate) fn icon_segmented<T: Clone + PartialEq + 'static>(
    id: &'static str,
    current: T,
    options: Vec<(T, gpui_kit::component::Icon, SharedString)>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let on_pick = Rc::new(on_pick);
    let (selected_bg, muted, fg) = (
        theme.secondary_active,
        theme.muted_foreground,
        theme.foreground,
    );
    let radius = theme.radius;
    h_flex()
        .w_full()
        .p_0p5()
        .gap_0p5()
        .rounded(radius)
        .bg(theme.secondary)
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(i, (value, icon, label))| {
                    let selected = value == current;
                    let on_pick = on_pick.clone();
                    v_flex()
                        .id((id, i))
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_0p5()
                        .py_1()
                        .rounded(radius)
                        .text_xs()
                        .map(|d| {
                            if selected {
                                d.bg(selected_bg).text_color(fg)
                            } else {
                                d.text_color(muted).hover(move |s| s.text_color(fg))
                            }
                        })
                        .child(icon.small())
                        .child(
                            div()
                                .max_w_full()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_x_hidden()
                                .child(label),
                        )
                        .on_click(move |_, window, cx| on_pick(value.clone(), window, cx))
                }),
        )
}

/// 下拉中带说明的一项：值、名称与可选的一行说明。
pub(crate) struct Choice<T> {
    pub(crate) value: T,
    pub(crate) label: SharedString,
    pub(crate) detail: Option<SharedString>,
}

/// 下拉选择，菜单中每项名称下方有一行说明；按钮只显示名称，宽度随内容。
pub(crate) fn select_described<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    current: T,
    choices: Vec<Choice<T>>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = choices
        .iter()
        .find(|c| c.value == current)
        .map(|c| c.label.clone())
        .unwrap_or_default();
    let on_pick = Rc::new(on_pick);
    let scrollable = choices.len() > SCROLLABLE_OPTIONS;
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .dropdown_caret(true)
        .max_w_full()
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
            let menu = menu.scrollable(scrollable);
            choices.iter().fold(menu, |menu, choice| {
                let value = choice.value.clone();
                let on_pick = on_pick.clone();
                let (title, detail) = (choice.label.clone(), choice.detail.clone());
                menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        v_flex()
                            .py_0p5()
                            .child(div().text_sm().child(title.clone()))
                            .children(detail.clone().map(|d| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(d)
                            }))
                    })
                    .checked(choice.value == current)
                    .on_click(move |_, window, cx| on_pick(value.clone(), window, cx)),
                )
            })
        })
}

/// 标签在上、控件在下的字段，用于横向并排多个输入的紧凑表单。
pub(crate) fn field(label: impl Into<SharedString>, control: impl IntoElement) -> Div {
    v_flex()
        .gap_1p5()
        .min_w_0()
        .child(
            div()
                .text_xs()
                .whitespace_nowrap()
                .text_ellipsis()
                .overflow_x_hidden()
                .child(label.into()),
        )
        .child(control)
}

/// 可伸缩的字段：按 `basis` 分配宽度，放不下时整个字段折到下一行。
pub(crate) fn grow(field: Div, basis: Pixels) -> AnyElement {
    field.flex_1().flex_basis(basis).into_any_element()
}

/// 并排的一行字段，放不下时折行。各字段的宽度由调用方以 [`grow`] 或固定宽度指定。
pub(crate) fn fields(items: impl IntoIterator<Item = AnyElement>) -> Div {
    h_flex()
        .w_full()
        .flex_wrap()
        .items_end()
        .gap_3()
        .children(items)
}

/// 当前值不在选项中时按钮上为空。
fn dropdown<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    current: T,
    options: Vec<(T, SharedString)>,
    width: Option<Pixels>,
    on_pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = options
        .iter()
        .find(|(v, _)| *v == current)
        .map(|(_, l)| l.clone())
        .unwrap_or_default();
    let on_pick = Rc::new(on_pick);
    let scrollable = options.len() > SCROLLABLE_OPTIONS;
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .dropdown_caret(true)
        .when_some(width, |button, width| button.w(width).max_w_full())
        // 表单字段里的弹层容器会被拉伸到整列宽，按右上角对齐时菜单会离开按钮，故左对齐；
        // 行尾的下拉贴着右缘，按右上角对齐才不会伸出窗口
        .dropdown_menu_with_anchor(
            if width.is_some() {
                Anchor::TopLeft
            } else {
                Anchor::TopRight
            },
            move |menu, _, _| {
                options
                    .iter()
                    .fold(menu, |menu, (value, label)| {
                        let value = value.clone();
                        let checked = value == current;
                        let on_pick = on_pick.clone();
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(checked)
                                .on_click(move |_, window, cx| on_pick(value.clone(), window, cx)),
                        )
                    })
                    .scrollable(scrollable)
            },
        )
}

struct Bound {
    input: Entity<InputState>,
    /// 最近一次与数据同步时的值。数据变化而输入框未参与时（热重载、外部修改）才覆盖输入框，
    /// 否则输入到一半的内容会被规整后的值替换。
    seen: SharedString,
    _subscription: Subscription,
}

/// 与数据双向绑定的输入框，状态按 `key` 保存在窗口中。
/// `set` 收到每次编辑后的文本，自行决定是否写入（如未闭合的引号可以忽略）。
pub(crate) fn bound_input(
    key: impl Into<SharedString>,
    get: impl Fn(&App) -> SharedString + 'static,
    set: impl Fn(SharedString, &mut App) + 'static,
    setup: impl FnOnce(InputState) -> InputState,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let get = Rc::new(get);
    let current = get(cx);
    let state = window.use_keyed_state(key.into(), cx, |window, cx| {
        let initial = current.clone();
        let input = cx.new(|cx| setup(InputState::new(window, cx)).default_value(initial));
        let get = get.clone();
        let _subscription = cx.subscribe(&input, move |bound: &mut Bound, input, event, cx| {
            if let InputEvent::Change = event {
                let value = input.read(cx).value();
                if value != bound.seen {
                    set(value, cx);
                    bound.seen = get(cx);
                }
            }
        });
        Bound {
            input,
            seen: current.clone(),
            _subscription,
        }
    });
    state.update(cx, |bound, cx| {
        if bound.seen != current {
            bound.seen = current.clone();
            bound
                .input
                .update(cx, |input, cx| input.set_value(current, window, cx));
        }
    });
    state.read(cx).input.clone()
}

/// 数值输入框：显示与写入都按步长的小数位取整，避免 f32 误差露出来。
pub(crate) fn bound_number(
    key: impl Into<SharedString>,
    range: std::ops::RangeInclusive<f64>,
    step: f64,
    get: impl Fn(&App) -> f64 + 'static,
    set: impl Fn(f64, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    // 按步长保留小数位：步长 1 取整，0.1 级保留一位，更小保留两位
    let scale = if step >= 1. {
        1.
    } else if step >= 0.1 {
        10.
    } else {
        100.
    };
    let round = move |v: f64| (v * scale).round() / scale;
    let (min, max) = (*range.start(), *range.end());
    bound_input(
        key,
        move |cx| round(get(cx)).to_string().into(),
        move |text, cx| {
            // 输入到一半（空、"-"、"1."）或越界时先不写，失焦时输入框会自行钳制
            if let Ok(value) = text.parse::<f64>()
                && (min..=max).contains(&value)
            {
                set(round(value), cx);
            }
        },
        |input| input.step(step).min(min).max(max),
        window,
        cx,
    )
}

/// 带图标的小号幽灵按钮，用于行尾或标题旁。
pub(crate) fn icon_button(
    id: impl Into<ElementId>,
    icon: impl Into<gpui_kit::component::Icon>,
    tooltip: impl Into<SharedString>,
) -> Button {
    Button::new(id)
        .icon(icon)
        .ghost()
        .small()
        .tooltip(tooltip.into())
}
