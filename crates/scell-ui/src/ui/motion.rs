//! 动效。全部基于 GPUI 的弹簧：状态按元素 id 保存，目标中途改变时从当前位置与速度继续，
//! 不会跳变或重新开始。设置中关闭过渡动画时一律直接到位。

use gpui_kit::{
    AnimationExt, App, ElementId, IntoElement, Pixels, SpringAnimation, SpringAnimationElement,
    SpringConfig, SpringPlayback, SpringTarget, Styled,
};

use crate::config::ConfigState;

/// 界面过渡：临界阻尼，不回弹，约 200 ms 到位。
const TRANSITION: SpringConfig = SpringConfig::new(600., 49., 1.);

pub(crate) fn enabled(cx: &App) -> bool {
    ConfigState::settings(cx).appearance.animations
}

/// 朝 `target` 运动的弹簧；关闭动画时直接到位。
pub(crate) fn spring<T: SpringTarget>(target: T, cx: &App) -> SpringAnimation<T> {
    let animation = SpringAnimation::new(TRANSITION).to(target);
    if enabled(cx) {
        animation
    } else {
        animation.playback(SpringPlayback::Completed)
    }
}

/// 首次出现时淡入。`id` 变化即视为新元素，会再次淡入（如切换子页面时以页面作为 id 的一部分）。
pub(crate) fn fade_in<E>(
    element: E,
    id: impl Into<ElementId>,
    cx: &App,
) -> SpringAnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    element.with_spring(id, spring(true, cx).from(false), |el, phase| {
        el.opacity(phase.0.clamp(0., 1.))
    })
}

/// 首次出现时淡入并从下方 `rise` 处升到原位。`place` 按偏移量放置元素（如设置 `top` 或 `bottom`），
/// 只适用于绝对定位或相对定位的元素。
pub(crate) fn rise_in<E>(
    element: E,
    id: impl Into<ElementId>,
    rise: Pixels,
    place: impl FnOnce(E, Pixels) -> E + 'static,
    cx: &App,
) -> SpringAnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    element.with_spring(id, spring(true, cx).from(false), move |el, phase| {
        let p = phase.0.clamp(0., 1.);
        place(el.opacity(p), rise * (1. - p))
    })
}

/// 宽度随目标平滑变化（侧栏展开收起等）。
pub(crate) fn width<E>(
    element: E,
    id: impl Into<ElementId>,
    target: Pixels,
    cx: &App,
) -> SpringAnimationElement<E>
where
    E: IntoElement + Styled + 'static,
{
    element.with_spring(id, spring(target, cx), |el, w| el.w(w))
}
