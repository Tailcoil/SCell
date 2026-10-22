use gpui_kit::{
    App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, FontId, GlobalElementId, GlyphId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, Rgba, ShapedLine, StrikethroughStyle, Style, TextAlign, TextRun,
    UnderlineStyle, Window, fill, outline, point, px, relative, size,
};
use scell_term::{CellAttrs, CursorShape, Rgb, SnapshotCell, TermSize};

use crate::box_drawing::{self, BoxPrimitive, CellBox};
use crate::glyphs::GlyphCache;
use crate::metrics::{CellMetrics, base_font};
use crate::{TerminalStyle, TerminalView};

/// 按单元格网格绘制终端：背景按行合并成色块，字形逐个落在格点上。
pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
    focus: FocusHandle,
}

impl TerminalElement {
    pub(crate) fn new(view: Entity<TerminalView>, focus: FocusHandle) -> Self {
        Self { view, focus }
    }
}

pub(crate) struct Frame {
    hitbox: Hitbox,
    /// 程序接管鼠标时显示箭头光标，否则显示文本光标。
    mouse_reporting: bool,
    /// 鼠标悬停在链接上（按住 Ctrl 时）。
    over_link: bool,
    over_scrollbar: bool,
    scrollbar: Option<(Bounds<Pixels>, Hsla)>,
    /// 默认背景（已乘以 `background_alpha`）；单元格背景与它相同时不再单独绘制。
    background: Hsla,
    rects: Vec<(Bounds<Pixels>, Hsla)>,
    /// 查找高亮，画在背景之后、字形之前。
    highlights: Vec<(Bounds<Pixels>, Hsla)>,
    /// 自绘的方框与块字符。
    boxes: Vec<(BoxPrimitive, Hsla)>,
    glyphs: Vec<PlacedGlyph>,
    decorations: Vec<Decoration>,
    cursor: Option<(Bounds<Pixels>, Hsla, bool)>,
    /// 块状光标下的字形改用背景色重绘，保证可读。
    cursor_glyphs: Vec<PlacedGlyph>,
    preedit: Option<(ShapedLine, Point<Pixels>, Hsla)>,
    line_height: Pixels,
}

#[derive(Clone, Copy)]
struct PlacedGlyph {
    origin: Point<Pixels>,
    font_id: FontId,
    glyph_id: GlyphId,
    font_size: Pixels,
    color: Hsla,
    is_emoji: bool,
}

struct Decoration {
    origin: Point<Pixels>,
    width: Pixels,
    color: Hsla,
    kind: DecorationKind,
}

enum DecorationKind {
    Underline { wavy: bool },
    Strikethrough,
}

fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        let focused = self.focus.is_focused(window);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.view.update(cx, |view, cx| {
            view.mouse.origin = bounds.origin;
            let frame = build_frame(view, bounds, focused, hitbox, window);
            let blinking = view.snapshot.cursor.is_some_and(|c| c.blinking);
            view.update_blink(focused, blinking, cx);
            frame
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        register_mouse(&self.view, &frame.hitbox, window);
        let cursor = if frame.over_link {
            CursorStyle::PointingHand
        } else if frame.mouse_reporting || frame.over_scrollbar {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, &frame.hitbox);
        if frame.background.a > 0. {
            window.paint_quad(fill(bounds, frame.background));
        }
        window.with_content_mask(Some(gpui_kit::ContentMask { bounds }), |window| {
            for (rect, color) in frame.rects.iter().chain(&frame.highlights) {
                window.paint_quad(fill(*rect, *color));
            }
            paint_boxes(&frame.boxes, window);
            paint_glyphs(&frame.glyphs, window);
            for d in &frame.decorations {
                match d.kind {
                    DecorationKind::Underline { wavy } => window.paint_underline(
                        d.origin,
                        d.width,
                        &UnderlineStyle {
                            color: Some(d.color),
                            thickness: px(1.),
                            wavy,
                        },
                    ),
                    DecorationKind::Strikethrough => window.paint_strikethrough(
                        d.origin,
                        d.width,
                        &StrikethroughStyle {
                            color: Some(d.color),
                            thickness: px(1.),
                        },
                    ),
                }
            }
            if let Some((rect, color, solid)) = frame.cursor {
                if solid {
                    window.paint_quad(fill(rect, color));
                    paint_glyphs(&frame.cursor_glyphs, window);
                } else {
                    window.paint_quad(outline(rect, color, gpui_kit::BorderStyle::Solid));
                }
            }
            if let Some((line, origin, bg)) = frame.preedit.take() {
                window.paint_quad(fill(
                    Bounds::new(origin, size(line.width, frame.line_height)),
                    bg,
                ));
                let _ = line.paint(origin, frame.line_height, TextAlign::Left, None, window, cx);
            }
            if let Some((thumb, color)) = frame.scrollbar {
                window.paint_quad(fill(thumb, color).corner_radii(thumb.size.width / 2.));
            }
        });
    }
}

fn paint_boxes(boxes: &[(BoxPrimitive, Hsla)], window: &mut Window) {
    for (prim, color) in boxes {
        match prim {
            BoxPrimitive::Rect(rect, alpha) => {
                let mut c = *color;
                c.a *= alpha;
                window.paint_quad(fill(*rect, c));
            }
            BoxPrimitive::Polygon(points) => {
                let mut builder = PathBuilder::fill();
                builder.add_polygon(points, true);
                if let Ok(path) = builder.build() {
                    window.paint_path(path, *color);
                }
            }
            BoxPrimitive::Arc {
                from,
                arc_start,
                arc_end,
                to,
                radius,
                width,
                clockwise,
            } => {
                let mut builder = PathBuilder::stroke(*width);
                builder.move_to(*from);
                builder.line_to(*arc_start);
                builder.arc_to(point(*radius, *radius), px(0.), false, *clockwise, *arc_end);
                builder.line_to(*to);
                if let Ok(path) = builder.build() {
                    window.paint_path(path, *color);
                }
            }
        }
    }
}

/// 按下只在终端可见区域内生效；移动与释放在窗口范围内监听，拖动到区域外时选区仍能延伸。
fn register_mouse(view: &Entity<TerminalView>, hitbox: &Hitbox, window: &mut Window) {
    let (v, h) = (view.clone(), hitbox.clone());
    window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && h.is_hovered(window) {
            v.update(cx, |view, cx| view.mouse_down(e, window, cx));
        }
    });
    let v = view.clone();
    window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble {
            v.update(cx, |view, cx| view.mouse_move(e, window, cx));
        }
    });
    let v = view.clone();
    window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble {
            v.update(cx, |view, cx| view.mouse_up(e, window, cx));
        }
    });
}

fn paint_glyphs(glyphs: &[PlacedGlyph], window: &mut Window) {
    for g in glyphs {
        let result = if g.is_emoji {
            window.paint_emoji(g.origin, g.font_id, g.glyph_id, g.font_size)
        } else {
            window.paint_glyph(g.origin, g.font_id, g.glyph_id, g.font_size, g.color)
        };
        if let Err(err) = result {
            tracing::debug!(%err, "字形绘制失败");
        }
    }
}

fn build_frame(
    view: &mut TerminalView,
    bounds: Bounds<Pixels>,
    focused: bool,
    hitbox: Hitbox,
    window: &mut Window,
) -> Frame {
    let scale = window.scale_factor();
    let metrics = match view.metrics {
        Some((m, s)) if s == scale => m,
        _ => {
            let m = CellMetrics::measure(&view.style, window);
            view.metrics = Some((m, scale));
            view.glyphs.invalidate();
            m
        }
    };

    let cols = (bounds.size.width / metrics.width).floor() as u16;
    let rows = (bounds.size.height / metrics.height).floor() as u16;
    let term_size = TermSize::new(
        cols,
        rows,
        metrics.width.as_f32().round() as u16,
        metrics.height.as_f32().round() as u16,
    );
    if view.terminal.size() != term_size {
        view.io.resize(term_size);
    }
    view.terminal.snapshot(&mut view.snapshot);

    let snap = &view.snapshot;
    let origin = bounds.origin;
    let cell_origin = |row: usize, col: usize| {
        point(
            origin.x + metrics.width * col as f32,
            origin.y + metrics.height * row as f32,
        )
    };
    let background = hsla(snap.background);
    let mut frame_background = background;
    frame_background.a = view.style.background_alpha;

    let mut frame = Frame {
        hitbox,
        mouse_reporting: view.terminal.input_mode().mouse_reporting(),
        over_link: view.mouse.link.is_some(),
        over_scrollbar: view.mouse.scrollbar.hovered || view.mouse.scrollbar.dragging(),
        scrollbar: None,
        background: frame_background,
        rects: Vec::new(),
        highlights: Vec::new(),
        boxes: Vec::new(),
        glyphs: Vec::new(),
        decorations: Vec::new(),
        cursor: None,
        cursor_glyphs: Vec::new(),
        preedit: None,
        line_height: metrics.height,
    };

    let mut box_scratch = Vec::new();
    for row in 0..snap.rows {
        let cells = snap.row(row);

        // 同色背景按行合并，减少图元数量
        let mut col = 0;
        while col < cells.len() {
            let bg = cells[col].bg;
            let start = col;
            while col < cells.len() && cells[col].bg == bg {
                col += 1;
            }
            if bg != snap.background {
                frame.rects.push((
                    Bounds::new(
                        cell_origin(row, start),
                        size(metrics.width * (col - start) as f32, metrics.height),
                    ),
                    hsla(bg),
                ));
            }
        }

        let mut col = 0;
        while col < cells.len() {
            let highlight = if cells[col].attrs.contains(CellAttrs::FOCUSED_MATCH) {
                Some(view.style.focused_match_background)
            } else if cells[col].attrs.contains(CellAttrs::MATCH) {
                Some(view.style.match_background)
            } else {
                None
            };
            let start = col;
            while col < cells.len() && same_highlight(&cells[start], &cells[col]) {
                col += 1;
            }
            if let Some(color) = highlight {
                frame.highlights.push((
                    Bounds::new(
                        cell_origin(row, start),
                        size(metrics.width * (col - start) as f32, metrics.height),
                    ),
                    color,
                ));
            }
        }

        for (col, cell) in cells.iter().enumerate() {
            let top_left = cell_origin(row, col);
            let span = if cell.attrs.contains(CellAttrs::WIDE) {
                2.
            } else {
                1.
            };
            if cell.has_glyph() && cell.combining.is_none() && box_drawing::handles(cell.ch) {
                let cell_box = CellBox {
                    origin: top_left,
                    size: size(metrics.width, metrics.height),
                    light: metrics.stroke.max(metrics.font_size / 14.),
                    scale,
                };
                let start = box_scratch.len();
                box_drawing::draw(cell.ch, &cell_box, &mut box_scratch);
                let color = hsla(cell.fg);
                frame
                    .boxes
                    .extend(box_scratch.drain(start..).map(|p| (p, color)));
            } else if cell.has_glyph() {
                push_cell_glyphs(
                    &mut view.glyphs,
                    &view.style,
                    window,
                    &snap_text(snap, cell),
                    cell,
                    top_left,
                    span,
                    &metrics,
                    &mut frame.glyphs,
                    None,
                );
            }
            push_decorations(cell, top_left, span, &metrics, &mut frame.decorations);
            if !cell.attrs.contains(CellAttrs::UNDERLINE)
                && view
                    .mouse
                    .link
                    .as_ref()
                    .is_some_and(|l| l.contains(row, col))
            {
                frame.decorations.push(Decoration {
                    origin: point(top_left.x, top_left.y + metrics.underline_offset),
                    width: metrics.width * span,
                    color: hsla(cell.fg),
                    kind: DecorationKind::Underline { wavy: false },
                });
            }
        }
    }

    if let Some(cursor) = snap.cursor {
        let top_left = cell_origin(cursor.row, cursor.col);
        let width = metrics.width * if cursor.wide { 2. } else { 1. };
        let color = hsla(view.style.palette.cursor);
        let rect = match cursor.shape {
            CursorShape::Block | CursorShape::HollowBlock => {
                Bounds::new(top_left, size(width, metrics.height))
            }
            CursorShape::Beam => Bounds::new(top_left, size(metrics.stroke * 2., metrics.height)),
            CursorShape::Underline => Bounds::new(
                point(
                    top_left.x,
                    top_left.y + metrics.height - metrics.stroke * 2.,
                ),
                size(width, metrics.stroke * 2.),
            ),
        };
        let solid = focused && cursor.shape != CursorShape::HollowBlock;
        let block = matches!(cursor.shape, CursorShape::Block | CursorShape::HollowBlock);
        // 失焦时块状光标显示为空心框
        frame.cursor = Some((rect, color, solid || !block));
        if solid && block {
            let cell = snap.row(cursor.row)[cursor.col];
            if cell.has_glyph() {
                let span = if cursor.wide { 2. } else { 1. };
                push_cell_glyphs(
                    &mut view.glyphs,
                    &view.style,
                    window,
                    &snap_text(snap, &cell),
                    &cell,
                    top_left,
                    span,
                    &metrics,
                    &mut frame.cursor_glyphs,
                    Some(hsla(snap.background)),
                );
            }
        }
        view.cursor_bounds = Some(Bounds::new(top_left, size(metrics.width, metrics.height)));

        if let Some(text) = view.marked_text.clone() {
            let fg = hsla(view.style.palette.foreground);
            let run = TextRun {
                len: text.len(),
                font: base_font(&view.style),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle {
                    color: Some(fg),
                    thickness: px(1.),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let line =
                window
                    .text_system()
                    .shape_line(text.into(), metrics.font_size, &[run], None);
            frame.preedit = Some((line, top_left, background));
            frame.cursor = None;
        }
    } else {
        view.cursor_bounds = None;
    }
    // 闪烁的“灭”半周期只隐藏光标本身，候选窗位置与预编辑文本照常
    if view.exited || (focused && view.blink.off) {
        frame.cursor = None;
        frame.cursor_glyphs.clear();
    }

    let hovered = view.mouse.scrollbar.hovered || view.mouse.scrollbar.dragging();
    let (rows, history, offset) = (snap.rows, snap.history, snap.display_offset);
    frame.scrollbar = view
        .mouse
        .scrollbar
        .layout(bounds, rows, history, offset)
        .map(|thumb| {
            let color =
                hsla(view.style.palette.foreground).opacity(if hovered { 0.5 } else { 0.25 });
            (thumb, color)
        });
    frame
}

fn same_highlight(a: &SnapshotCell, b: &SnapshotCell) -> bool {
    let mask = CellAttrs::MATCH.union(CellAttrs::FOCUSED_MATCH);
    a.attrs.intersection(mask) == b.attrs.intersection(mask)
}

fn snap_text(snap: &scell_term::RenderSnapshot, cell: &SnapshotCell) -> String {
    let mut text = String::from(cell.ch);
    if let Some(extra) = snap.combining_of(cell) {
        text.push_str(extra);
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn push_cell_glyphs(
    cache: &mut GlyphCache,
    style: &TerminalStyle,
    window: &Window,
    text: &str,
    cell: &SnapshotCell,
    top_left: Point<Pixels>,
    span: f32,
    metrics: &CellMetrics,
    out: &mut Vec<PlacedGlyph>,
    color_override: Option<Hsla>,
) {
    let shaped = cache.get(
        text,
        cell.attrs.contains(CellAttrs::BOLD),
        cell.attrs.contains(CellAttrs::ITALIC),
        style,
        window,
    );
    // 字形比格子窄时（回退字体、宽字符）居中放置；比格子宽时左对齐，由内容遮罩裁剪
    let span_width = metrics.width * span;
    let offset = if shaped.width < span_width {
        (span_width - shaped.width) / 2.
    } else {
        px(0.)
    };
    let color = color_override.unwrap_or_else(|| hsla(cell.fg));
    for g in &shaped.glyphs {
        out.push(PlacedGlyph {
            origin: point(top_left.x + offset + g.x, top_left.y + metrics.baseline),
            font_id: g.font_id,
            glyph_id: g.glyph_id,
            font_size: metrics.font_size,
            color,
            is_emoji: g.is_emoji,
        });
    }
}

fn push_decorations(
    cell: &SnapshotCell,
    top_left: Point<Pixels>,
    span: f32,
    metrics: &CellMetrics,
    out: &mut Vec<Decoration>,
) {
    let width = metrics.width * span;
    let color = hsla(cell.fg);
    let a = cell.attrs;
    if a.intersects(
        CellAttrs::UNDERLINE
            .union(CellAttrs::DOUBLE_UNDERLINE)
            .union(CellAttrs::UNDERCURL),
    ) {
        out.push(Decoration {
            origin: point(top_left.x, top_left.y + metrics.underline_offset),
            width,
            color,
            kind: DecorationKind::Underline {
                wavy: a.contains(CellAttrs::UNDERCURL),
            },
        });
        if a.contains(CellAttrs::DOUBLE_UNDERLINE) {
            out.push(Decoration {
                origin: point(
                    top_left.x,
                    top_left.y + metrics.underline_offset + metrics.stroke * 2.,
                ),
                width,
                color,
                kind: DecorationKind::Underline { wavy: false },
            });
        }
    }
    if a.contains(CellAttrs::STRIKEOUT) {
        out.push(Decoration {
            origin: point(
                top_left.x,
                top_left.y + metrics.baseline - metrics.font_size * 0.3,
            ),
            width,
            color,
            kind: DecorationKind::Strikethrough,
        });
    }
}
