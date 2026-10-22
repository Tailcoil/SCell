//! 页面骨架。标题栏与内容列解耦：标题栏的控件始终在同一位置，切换子页面、内容列变宽变窄都不会让它移动。
//! 标题栏分三栏，左右两栏等宽，中间的控件居中且不会与右栏重叠。

use gpui_kit::component::sheet::Sheet;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::appearance::Appearance;
use crate::ui::metrics::PANE_GAP;

/// 内容列的纵向行为。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    /// 内容超出时整列滚动（表单页）。
    Scroll,
    /// 内容铺满剩余高度、自行安排滚动区域（主从布局等），避免一处滚动带动整页。
    Fill,
}

/// 整页：顶部一行标题栏（`center` 居中、`trailing` 贴右），下面是内容列，最大宽度为 `width`、水平居中。
pub(crate) fn page(
    id: &'static str,
    center: impl IntoElement,
    trailing: Option<AnyElement>,
    width: Pixels,
    mode: Body,
    body: impl IntoElement,
) -> impl IntoElement {
    // 两侧至少留一个小图标按钮的宽度，窄时中间的标签栏收缩而不压到右侧按钮
    let side = || h_flex().flex_1().flex_basis(px(0.)).min_w_6();
    let header = h_flex()
        .w_full()
        .gap_2()
        .px_6()
        .pt_5()
        .pb_4()
        .child(side())
        // 放不下时中间收缩，由其中的标签栏自行滚动或收进溢出菜单
        .child(div().flex_shrink(1.).min_w_0().max_w_full().child(center))
        .child(side().justify_end().children(trailing));
    let column = div().w_full().max_w(width).child(body);
    let content = match mode {
        Body::Scroll => div().id(id).flex_1().min_h_0().overflow_y_scroll().child(
            div()
                .w_full()
                .flex()
                .justify_center()
                .px_6()
                .pb_6()
                .child(column),
        ),
        Body::Fill => div()
            .id(id)
            .flex_1()
            .min_h_0()
            .flex()
            .justify_center()
            .px_6()
            .pb_6()
            .child(column.h_full()),
    };
    v_flex().size_full().child(header).child(content)
}

/// 纵向排列的若干分节。
pub(crate) fn column(sections: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    v_flex()
        .w_full()
        .gap_6()
        .children(sections)
        .into_any_element()
}

/// 侧弹窗与窗格一样做成浮起的圆角卡片：离窗口边缘留出窗格间距，不贴着圆角窗口的边缘露出直角。
/// 主题里已把侧弹窗的顶部偏移设为 0（见 `appearance::apply`），上边距也由这里给。
pub(crate) fn floating_sheet(sheet: Sheet, cx: &App) -> Sheet {
    let theme = cx.theme();
    sheet
        .mt(PANE_GAP)
        .mr(PANE_GAP)
        .mb(PANE_GAP)
        .rounded(theme.radius_lg)
        .overflow_hidden()
        // 组件按停靠方向只在一侧画边框；补齐四边，与对话框、弹出菜单一致，暗色下阴影看不出时靠它分隔
        .border_1()
        .border_color(theme.border)
        .bg(Appearance::surface(cx))
}
