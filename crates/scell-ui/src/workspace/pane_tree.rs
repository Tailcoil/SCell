//! 标签内的分屏树。只描述结构，不含视图；尺寸由 resizable 组件自行维护。

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PaneId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    /// 左右排列。
    Row,
    /// 上下排列。
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PaneTree {
    Leaf(PaneId),
    Split {
        /// 结构变化时换新 id，让 resizable 重新均分尺寸。
        id: u64,
        axis: Axis,
        children: Vec<PaneTree>,
    },
}

/// 分割的 id 全应用唯一：标签可以在窗口间移动，而 resizable 按 id 在窗口内保存尺寸。
fn fresh_split_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// 归一化坐标下的矩形，用于方向导航。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl PaneTree {
    /// 由子树组成分割；只有一棵时直接返回它，没有时为 `None`。
    pub(crate) fn join(axis: Axis, mut children: Vec<PaneTree>) -> Option<PaneTree> {
        match children.len() {
            0 => None,
            1 => children.pop(),
            _ => Some(PaneTree::Split {
                id: fresh_split_id(),
                axis,
                children,
            }),
        }
    }

    /// 在 `target` 之后插入 `new`。父节点方向相同时作为兄弟插入，否则把 `target` 替换为新的分割。
    pub(crate) fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            PaneTree::Leaf(id) if *id == target => {
                *self = PaneTree::Split {
                    id: fresh_split_id(),
                    axis,
                    children: vec![PaneTree::Leaf(target), PaneTree::Leaf(new)],
                };
                true
            }
            PaneTree::Leaf(_) => false,
            PaneTree::Split {
                id,
                axis: own,
                children,
            } => {
                if *own == axis
                    && let Some(i) = children.iter().position(|c| *c == PaneTree::Leaf(target))
                {
                    children.insert(i + 1, PaneTree::Leaf(new));
                    *id = fresh_split_id();
                    return true;
                }
                children.iter_mut().any(|c| c.split(target, new, axis))
            }
        }
    }

    /// 移除窗格；只剩一个子节点的分割会被折叠。返回是否找到。整棵树只剩被移除的叶子时返回 `false`，由调用方关闭标签。
    pub(crate) fn remove(&mut self, target: PaneId) -> bool {
        let PaneTree::Split { id, children, .. } = self else {
            return false;
        };
        if let Some(i) = children.iter().position(|c| *c == PaneTree::Leaf(target)) {
            children.remove(i);
            *id = fresh_split_id();
        } else if !children.iter_mut().any(|c| c.remove(target)) {
            return false;
        }
        if children.len() == 1 {
            let only = children.remove(0);
            *self = only;
        }
        true
    }

    pub(crate) fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            PaneTree::Leaf(id) => out.push(*id),
            PaneTree::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }

    /// 按均分尺寸估算布局，找 `from` 在某方向上相邻、重叠最多的窗格。
    pub(crate) fn neighbor(&self, from: PaneId, dir: Direction) -> Option<PaneId> {
        let mut rects = Vec::new();
        self.layout(
            Rect {
                x: 0.,
                y: 0.,
                w: 1.,
                h: 1.,
            },
            &mut rects,
        );
        let (_, src) = *rects.iter().find(|(id, _)| *id == from)?;
        const EPS: f32 = 1e-4;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.);
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, r)| {
                let (touch, shared) = match dir {
                    Direction::Left => (
                        (r.x + r.w - src.x).abs() < EPS,
                        overlap(src.y, src.y + src.h, r.y, r.y + r.h),
                    ),
                    Direction::Right => (
                        (src.x + src.w - r.x).abs() < EPS,
                        overlap(src.y, src.y + src.h, r.y, r.y + r.h),
                    ),
                    Direction::Up => (
                        (r.y + r.h - src.y).abs() < EPS,
                        overlap(src.x, src.x + src.w, r.x, r.x + r.w),
                    ),
                    Direction::Down => (
                        (src.y + src.h - r.y).abs() < EPS,
                        overlap(src.x, src.x + src.w, r.x, r.x + r.w),
                    ),
                };
                (touch && shared > EPS).then_some((*id, shared))
            })
            // 重叠相同时取顺序在前的窗格，结果稳定
            .fold(None, |best: Option<(PaneId, f32)>, c| match best {
                Some(b) if c.1 <= b.1 + EPS => Some(b),
                _ => Some(c),
            })
            .map(|(id, _)| id)
    }

    fn layout(&self, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            PaneTree::Leaf(id) => out.push((*id, rect)),
            PaneTree::Split { axis, children, .. } => {
                let n = children.len() as f32;
                for (i, child) in children.iter().enumerate() {
                    let i = i as f32;
                    let r = match axis {
                        Axis::Row => Rect {
                            x: rect.x + rect.w * i / n,
                            w: rect.w / n,
                            ..rect
                        },
                        Axis::Column => Rect {
                            y: rect.y + rect.h * i / n,
                            h: rect.h / n,
                            ..rect
                        },
                    };
                    child.layout(r, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PaneId = PaneId(1);
    const B: PaneId = PaneId(2);
    const C: PaneId = PaneId(3);
    const D: PaneId = PaneId(4);

    #[test]
    fn same_axis_split_inserts_sibling() {
        let mut t = PaneTree::Leaf(A);
        assert!(t.split(A, B, Axis::Row));
        assert!(t.split(A, C, Axis::Row));
        assert_eq!(t.leaves(), [A, C, B]);
        let PaneTree::Split { children, .. } = &t else {
            panic!("应为分割");
        };
        assert_eq!(children.len(), 3);
    }

    #[test]
    fn cross_axis_split_nests_and_remove_collapses() {
        let mut t = PaneTree::Leaf(A);
        t.split(A, B, Axis::Row);
        t.split(B, C, Axis::Column);
        assert_eq!(t.leaves(), [A, B, C]);
        let PaneTree::Split { id: outer, .. } = t else {
            panic!("应为分割");
        };
        assert!(t.remove(C));
        // 内层折叠后外层子节点数不变，id 保持，已有尺寸不受影响
        assert_eq!(
            t,
            PaneTree::Split {
                id: outer,
                axis: Axis::Row,
                children: vec![PaneTree::Leaf(A), PaneTree::Leaf(B)]
            }
        );
        assert!(t.remove(A));
        assert_eq!(t, PaneTree::Leaf(B));
        // 最后一个窗格由调用方处理
        assert!(!t.remove(B));
    }

    #[test]
    fn directional_neighbors() {
        // A | (B / C)，再把 C 左右拆成 C | D
        let mut t = PaneTree::Leaf(A);
        t.split(A, B, Axis::Row);
        t.split(B, C, Axis::Column);
        t.split(C, D, Axis::Row);
        assert_eq!(t.neighbor(A, Direction::Right), Some(B));
        // C、D 与 B 的重叠相同，取顺序在前的 C
        assert_eq!(t.neighbor(B, Direction::Down), Some(C));
        assert_eq!(t.neighbor(C, Direction::Left), Some(A));
        assert_eq!(t.neighbor(D, Direction::Left), Some(C));
        assert_eq!(t.neighbor(D, Direction::Up), Some(B));
        assert_eq!(t.neighbor(A, Direction::Left), None);
    }
}
