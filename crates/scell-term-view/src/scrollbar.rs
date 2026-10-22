//! 回滚滚动条：浮在内容右缘的细条，只在已向上滚动、鼠标靠近右缘或正在拖动时显示，可拖动或点击跳转。

use gpui_kit::{Bounds, Context, Pixels, Point, point, px, size};

use crate::TerminalView;

const WIDTH: Pixels = px(6.);
/// 右缘这么宽的区域内悬停即显示滚动条，也是可点击区域。
const HOT_ZONE: Pixels = px(14.);
const MIN_THUMB: Pixels = px(24.);

#[derive(Default)]
pub(crate) struct ScrollbarState {
    pub(crate) hovered: bool,
    /// 拖动中：按下点相对滑块顶部的距离。
    grab: Option<Pixels>,
    geometry: Option<Geometry>,
}

#[derive(Clone, Copy)]
struct Geometry {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    history: usize,
}

impl ScrollbarState {
    /// 由 prepaint 调用；返回需要绘制的滑块（不需要显示时为 `None`）。
    pub(crate) fn layout(
        &mut self,
        bounds: Bounds<Pixels>,
        rows: usize,
        history: usize,
        offset: usize,
    ) -> Option<Bounds<Pixels>> {
        self.geometry = None;
        if history == 0 || rows == 0 {
            self.grab = None;
            return None;
        }
        let track = Bounds::new(
            point(bounds.right() - WIDTH, bounds.top()),
            size(WIDTH, bounds.size.height),
        );
        let height = track.size.height;
        let thumb_height = (height * (rows as f32 / (rows + history) as f32))
            .max(MIN_THUMB)
            .min(height);
        let travel = height - thumb_height;
        let top = track.top() + travel * ((history - offset.min(history)) as f32 / history as f32);
        let thumb = Bounds::new(point(track.left(), top), size(WIDTH, thumb_height));
        self.geometry = Some(Geometry {
            track,
            thumb,
            history,
        });
        (offset > 0 || self.hovered || self.grab.is_some()).then_some(thumb)
    }

    pub(crate) fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    fn in_hot_zone(&self, p: Point<Pixels>) -> bool {
        self.geometry.is_some_and(|g| {
            p.x >= g.track.right() - HOT_ZONE
                && p.x <= g.track.right()
                && p.y >= g.track.top()
                && p.y <= g.track.bottom()
        })
    }

    /// 滑块顶部位于 `top` 时对应的滚动偏移。
    fn offset_for(&self, top: Pixels) -> Option<usize> {
        let g = self.geometry?;
        let travel = g.track.size.height - g.thumb.size.height;
        if travel <= px(0.) {
            return Some(0);
        }
        let frac = ((top - g.track.top()) / travel).clamp(0., 1.);
        Some(g.history - (frac * g.history as f32).round() as usize)
    }
}

impl TerminalView {
    /// 按下左键：落在右缘时开始拖动（点在滑块外则先跳到该处），返回是否已处理。
    pub(crate) fn scrollbar_down(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let bar = &mut self.mouse.scrollbar;
        let Some(g) = bar.geometry.filter(|_| bar.in_hot_zone(p)) else {
            return false;
        };
        let grab = if p.y >= g.thumb.top() && p.y <= g.thumb.bottom() {
            p.y - g.thumb.top()
        } else {
            g.thumb.size.height / 2.
        };
        bar.grab = Some(grab);
        self.scrollbar_drag(p, cx);
        true
    }

    /// 拖动中返回真；否则只更新悬停状态。
    pub(crate) fn scrollbar_move(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if self.mouse.scrollbar.dragging() {
            self.scrollbar_drag(p, cx);
            return true;
        }
        let hovered = self.mouse.scrollbar.in_hot_zone(p);
        if hovered != self.mouse.scrollbar.hovered {
            self.mouse.scrollbar.hovered = hovered;
            cx.notify();
        }
        false
    }

    pub(crate) fn scrollbar_up(&mut self) -> bool {
        self.mouse.scrollbar.grab.take().is_some()
    }

    fn scrollbar_drag(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let bar = &self.mouse.scrollbar;
        if let Some(offset) = bar.grab.and_then(|grab| bar.offset_for(p.y - grab)) {
            self.terminal.scroll_to(offset);
            cx.notify();
        }
    }
}
