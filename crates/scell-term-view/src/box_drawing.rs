//! 自绘方框绘制（U+2500–257F）、块元素（U+2580–259F）与 Powerline 分隔符。
//!
//! 字体里的这些字形按字体自己的行高设计，行高大于 1 时上下会断开；按单元格几何直接绘制可保证相邻格无缝衔接。
//! 未覆盖：斜线（U+2571–2573），以及虚线按实线绘制。双线在交汇处不做精确裁切。

use gpui_kit::{Bounds, Pixels, Point, Size, point, px, size};

#[derive(Debug, Clone)]
pub(crate) enum BoxPrimitive {
    /// 实心矩形，`alpha` 用于阴影块（░▒▓）。
    Rect(Bounds<Pixels>, f32),
    Polygon(Vec<Point<Pixels>>),
    /// 圆角：从 `from` 沿直线到圆弧起点，以 `radius` 圆弧转到终点，再到 `to`。
    Arc {
        from: Point<Pixels>,
        arc_start: Point<Pixels>,
        arc_end: Point<Pixels>,
        to: Point<Pixels>,
        radius: Pixels,
        width: Pixels,
        /// 屏幕坐标（y 向下）中是否顺时针转弯。
        clockwise: bool,
    },
}

/// 单元格几何与线宽，坐标已对齐到设备像素。
pub(crate) struct CellBox {
    pub origin: Point<Pixels>,
    pub size: Size<Pixels>,
    /// 细线宽度；粗线为两倍。
    pub light: Pixels,
    pub scale: f32,
}

impl CellBox {
    fn snap(&self, v: Pixels) -> Pixels {
        px((v.as_f32() * self.scale).round() / self.scale)
    }

    fn rect(&self, x0: Pixels, y0: Pixels, x1: Pixels, y1: Pixels) -> BoxPrimitive {
        let (x0, y0, x1, y1) = (self.snap(x0), self.snap(y0), self.snap(x1), self.snap(y1));
        BoxPrimitive::Rect(Bounds::new(point(x0, y0), size(x1 - x0, y1 - y0)), 1.)
    }
}

/// 线的粗细：无、细、粗；`D` 为两条细线。
#[derive(Clone, Copy, PartialEq, Eq)]
enum W {
    N,
    L,
    H,
    D,
}

/// 是否由本模块绘制。
pub(crate) fn handles(c: char) -> bool {
    matches!(c as u32,
        0x2500..=0x2570 | 0x2574..=0x259F | 0xE0B0..=0xE0B3 | 0xE0B4 | 0xE0B6)
}

pub(crate) fn draw(c: char, cell: &CellBox, out: &mut Vec<BoxPrimitive>) {
    let cp = c as u32;
    match cp {
        0x256D..=0x2570 => rounded_corner(cp, cell, out),
        0x2500..=0x257F => {
            if let Some([up, right, down, left]) = arms(cp) {
                lines(cell, up, right, down, left, out);
            }
        }
        0x2580..=0x259F => block(cp, cell, out),
        _ => powerline(cp, cell, out),
    }
}

fn lines(cell: &CellBox, up: W, right: W, down: W, left: W, out: &mut Vec<BoxPrimitive>) {
    let Point { x: x0, y: y0 } = cell.origin;
    let (x1, y1) = (x0 + cell.size.width, y0 + cell.size.height);
    let cx = cell.snap(x0 + cell.size.width / 2.);
    let cy = cell.snap(y0 + cell.size.height / 2.);
    let t = |w: W| match w {
        W::H => cell.light * 2.,
        _ => cell.light,
    };
    // 双线两条线中心到格中心的距离
    let d = cell.light;
    // 横向臂延伸到竖线的外缘，保证拐角闭合
    let v_reach = t(up.max_w(down)) / 2.
        + if up == W::D || down == W::D {
            d
        } else {
            px(0.)
        };
    let h_reach = t(left.max_w(right)) / 2.
        + if left == W::D || right == W::D {
            d
        } else {
            px(0.)
        };

    let mut h_arm = |w: W, from: Pixels, to: Pixels| match w {
        W::N => {}
        W::D => {
            for off in [-d, d] {
                let y = cy + off;
                out.push(cell.rect(from, y - cell.light / 2., to, y + cell.light / 2.));
            }
        }
        _ => out.push(cell.rect(from, cy - t(w) / 2., to, cy + t(w) / 2.)),
    };
    h_arm(left, x0, cx + v_reach);
    h_arm(right, cx - v_reach, x1);

    let mut v_arm = |w: W, from: Pixels, to: Pixels| match w {
        W::N => {}
        W::D => {
            for off in [-d, d] {
                let x = cx + off;
                out.push(cell.rect(x - cell.light / 2., from, x + cell.light / 2., to));
            }
        }
        _ => out.push(cell.rect(cx - t(w) / 2., from, cx + t(w) / 2., to)),
    };
    v_arm(up, y0, cy + h_reach);
    v_arm(down, cy - h_reach, y1);
}

impl W {
    /// 用于计算交汇处延伸量的较粗一方。
    fn max_w(self, other: W) -> W {
        let rank = |w: W| match w {
            W::N => 0,
            W::L | W::D => 1,
            W::H => 2,
        };
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

/// 四个方向（上、右、下、左）的线型。
fn arms(cp: u32) -> Option<[W; 4]> {
    use W::*;
    Some(match cp {
        0x2500 | 0x2504 | 0x2508 | 0x254C => [N, L, N, L],
        0x2501 | 0x2505 | 0x2509 | 0x254D => [N, H, N, H],
        0x2502 | 0x2506 | 0x250A | 0x254E => [L, N, L, N],
        0x2503 | 0x2507 | 0x250B | 0x254F => [H, N, H, N],
        0x250C => [N, L, L, N],
        0x250D => [N, H, L, N],
        0x250E => [N, L, H, N],
        0x250F => [N, H, H, N],
        0x2510 => [N, N, L, L],
        0x2511 => [N, N, L, H],
        0x2512 => [N, N, H, L],
        0x2513 => [N, N, H, H],
        0x2514 => [L, L, N, N],
        0x2515 => [L, H, N, N],
        0x2516 => [H, L, N, N],
        0x2517 => [H, H, N, N],
        0x2518 => [L, N, N, L],
        0x2519 => [L, N, N, H],
        0x251A => [H, N, N, L],
        0x251B => [H, N, N, H],
        0x251C => [L, L, L, N],
        0x251D => [L, H, L, N],
        0x251E => [H, L, L, N],
        0x251F => [L, L, H, N],
        0x2520 => [H, L, H, N],
        0x2521 => [H, H, L, N],
        0x2522 => [L, H, H, N],
        0x2523 => [H, H, H, N],
        0x2524 => [L, N, L, L],
        0x2525 => [L, N, L, H],
        0x2526 => [H, N, L, L],
        0x2527 => [L, N, H, L],
        0x2528 => [H, N, H, L],
        0x2529 => [H, N, L, H],
        0x252A => [L, N, H, H],
        0x252B => [H, N, H, H],
        0x252C => [N, L, L, L],
        0x252D => [N, L, L, H],
        0x252E => [N, H, L, L],
        0x252F => [N, H, L, H],
        0x2530 => [N, L, H, L],
        0x2531 => [N, L, H, H],
        0x2532 => [N, H, H, L],
        0x2533 => [N, H, H, H],
        0x2534 => [L, L, N, L],
        0x2535 => [L, L, N, H],
        0x2536 => [L, H, N, L],
        0x2537 => [L, H, N, H],
        0x2538 => [H, L, N, L],
        0x2539 => [H, L, N, H],
        0x253A => [H, H, N, L],
        0x253B => [H, H, N, H],
        0x253C => [L, L, L, L],
        0x253D => [L, L, L, H],
        0x253E => [L, H, L, L],
        0x253F => [L, H, L, H],
        0x2540 => [H, L, L, L],
        0x2541 => [L, L, H, L],
        0x2542 => [H, L, H, L],
        0x2543 => [H, L, L, H],
        0x2544 => [H, H, L, L],
        0x2545 => [L, L, H, H],
        0x2546 => [L, H, H, L],
        0x2547 => [H, H, L, H],
        0x2548 => [L, H, H, H],
        0x2549 => [H, L, H, H],
        0x254A => [H, H, H, L],
        0x254B => [H, H, H, H],
        0x2550 => [N, D, N, D],
        0x2551 => [D, N, D, N],
        0x2552 => [N, D, L, N],
        0x2553 => [N, L, D, N],
        0x2554 => [N, D, D, N],
        0x2555 => [N, N, L, D],
        0x2556 => [N, N, D, L],
        0x2557 => [N, N, D, D],
        0x2558 => [L, D, N, N],
        0x2559 => [D, L, N, N],
        0x255A => [D, D, N, N],
        0x255B => [L, N, N, D],
        0x255C => [D, N, N, L],
        0x255D => [D, N, N, D],
        0x255E => [L, D, L, N],
        0x255F => [D, L, D, N],
        0x2560 => [D, D, D, N],
        0x2561 => [L, N, L, D],
        0x2562 => [D, N, D, L],
        0x2563 => [D, N, D, D],
        0x2564 => [N, D, L, D],
        0x2565 => [N, L, D, L],
        0x2566 => [N, D, D, D],
        0x2567 => [L, D, N, D],
        0x2568 => [D, L, N, L],
        0x2569 => [D, D, N, D],
        0x256A => [L, D, L, D],
        0x256B => [D, L, D, L],
        0x256C => [D, D, D, D],
        0x2574 => [N, N, N, L],
        0x2575 => [L, N, N, N],
        0x2576 => [N, L, N, N],
        0x2577 => [N, N, L, N],
        0x2578 => [N, N, N, H],
        0x2579 => [H, N, N, N],
        0x257A => [N, H, N, N],
        0x257B => [N, N, H, N],
        0x257C => [N, H, N, L],
        0x257D => [L, N, H, N],
        0x257E => [N, L, N, H],
        0x257F => [H, N, L, N],
        _ => return None,
    })
}

/// ╭ ╮ ╯ ╰：两条细线经四分之一圆弧相连。
fn rounded_corner(cp: u32, cell: &CellBox, out: &mut Vec<BoxPrimitive>) {
    let Point { x: x0, y: y0 } = cell.origin;
    let (x1, y1) = (x0 + cell.size.width, y0 + cell.size.height);
    let cx = cell.snap(x0 + cell.size.width / 2.);
    let cy = cell.snap(y0 + cell.size.height / 2.);
    let r = (cell.size.width / 2.).min(cell.size.height / 2.);
    // 竖向一端（上或下边缘）与横向一端（左或右边缘）
    let (vy, hx, sy, sx) = match cp {
        0x256D => (y1, x1, 1., 1.),
        0x256E => (y1, x0, 1., -1.),
        0x256F => (y0, x0, -1., -1.),
        _ => (y0, x1, -1., 1.),
    };
    out.push(BoxPrimitive::Arc {
        from: point(cx, vy),
        arc_start: point(cx, cy + r * sy),
        arc_end: point(cx + r * sx, cy),
        to: point(hx, cy),
        radius: r,
        width: cell.light,
        clockwise: sx * sy > 0.,
    });
}

fn block(cp: u32, cell: &CellBox, out: &mut Vec<BoxPrimitive>) {
    let Point { x: x0, y: y0 } = cell.origin;
    let (w, h) = (cell.size.width, cell.size.height);
    let (x1, y1) = (x0 + w, y0 + h);
    let (mx, my) = (x0 + w / 2., y0 + h / 2.);
    let eighth = |n: u32| n as f32 / 8.;
    match cp {
        0x2580 => out.push(cell.rect(x0, y0, x1, my)),
        0x2581..=0x2588 => out.push(cell.rect(x0, y1 - h * eighth(cp - 0x2580), x1, y1)),
        0x2589..=0x258F => out.push(cell.rect(x0, y0, x0 + w * eighth(0x2590 - cp), y1)),
        0x2590 => out.push(cell.rect(mx, y0, x1, y1)),
        0x2591..=0x2593 => {
            let alpha = (cp - 0x2590) as f32 * 0.25;
            if let BoxPrimitive::Rect(b, _) = cell.rect(x0, y0, x1, y1) {
                out.push(BoxPrimitive::Rect(b, alpha));
            }
        }
        0x2594 => out.push(cell.rect(x0, y0, x1, y0 + h * eighth(1))),
        0x2595 => out.push(cell.rect(x1 - w * eighth(1), y0, x1, y1)),
        _ => {
            // 象限：左上、右上、左下、右下
            let quads: [bool; 4] = match cp {
                0x2596 => [false, false, true, false],
                0x2597 => [false, false, false, true],
                0x2598 => [true, false, false, false],
                0x2599 => [true, false, true, true],
                0x259A => [true, false, false, true],
                0x259B => [true, true, true, false],
                0x259C => [true, true, false, true],
                0x259D => [false, true, false, false],
                0x259E => [false, true, true, false],
                _ => [false, true, true, true],
            };
            let rects = [
                (x0, y0, mx, my),
                (mx, y0, x1, my),
                (x0, my, mx, y1),
                (mx, my, x1, y1),
            ];
            for (on, (a, b, c, d)) in quads.into_iter().zip(rects) {
                if on {
                    out.push(cell.rect(a, b, c, d));
                }
            }
        }
    }
}

fn powerline(cp: u32, cell: &CellBox, out: &mut Vec<BoxPrimitive>) {
    let Point { x: x0, y: y0 } = cell.origin;
    let (x1, y1) = (x0 + cell.size.width, y0 + cell.size.height);
    let my = y0 + cell.size.height / 2.;
    match cp {
        0xE0B0 => out.push(BoxPrimitive::Polygon(vec![
            point(x0, y0),
            point(x1, my),
            point(x0, y1),
        ])),
        0xE0B2 => out.push(BoxPrimitive::Polygon(vec![
            point(x1, y0),
            point(x0, my),
            point(x1, y1),
        ])),
        // 细箭头：用两条细三角近似描边
        0xE0B1 | 0xE0B3 => {
            let (tip, base) = if cp == 0xE0B1 { (x1, x0) } else { (x0, x1) };
            let t = cell.light;
            let dir = if cp == 0xE0B1 { -1. } else { 1. };
            out.push(BoxPrimitive::Polygon(vec![
                point(base, y0),
                point(tip, my),
                point(base, y1),
                point(base, y1 - t * 1.5),
                point(tip + t * 1.5 * dir, my),
                point(base, y0 + t * 1.5),
            ]));
        }
        // 半圆：用多边形近似
        _ => {
            let right = cp == 0xE0B4;
            let r = cell.size.height / 2.;
            let steps = 16;
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let a =
                    std::f32::consts::PI * (i as f32 / steps as f32) - std::f32::consts::FRAC_PI_2;
                let dx = (cell.size.width.as_f32()).min(r.as_f32()) * a.cos();
                let x = if right { x0 + px(dx) } else { x1 - px(dx) };
                pts.push(point(x, my + r * a.sin()));
            }
            out.push(BoxPrimitive::Polygon(pts));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> CellBox {
        CellBox {
            origin: point(px(0.), px(0.)),
            size: size(px(8.), px(20.)),
            light: px(1.),
            scale: 1.,
        }
    }

    fn rects(c: char) -> Vec<Bounds<Pixels>> {
        let mut out = Vec::new();
        draw(c, &cell(), &mut out);
        out.into_iter()
            .filter_map(|p| match p {
                BoxPrimitive::Rect(b, _) => Some(b),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn vertical_line_spans_full_cell_height() {
        let r = rects('│');
        assert_eq!(r.len(), 2);
        let top = r.iter().map(|b| b.origin.y).fold(px(99.), Pixels::min);
        let bottom = r.iter().map(|b| b.bottom()).fold(px(0.), Pixels::max);
        assert_eq!((top, bottom), (px(0.), px(20.)));
    }

    #[test]
    fn corner_arms_meet() {
        // ┌：右臂与下臂在中心重叠，拐角没有缺口
        let r = rects('┌');
        let right = r[0];
        let down = r[1];
        assert!(right.origin.x <= down.origin.x);
        assert!(down.origin.y <= right.origin.y);
    }

    #[test]
    fn coverage() {
        assert!(handles('─') && handles('╭') && handles('█') && handles('\u{e0b0}'));
        assert!(!handles('╱') && !handles('a'));
        for cp in 0x2500..=0x259F_u32 {
            let c = char::from_u32(cp).unwrap();
            if handles(c) {
                let mut out = Vec::new();
                draw(c, &cell(), &mut out);
                assert!(!out.is_empty(), "U+{cp:04X} 没有绘制内容");
            }
        }
    }
}
