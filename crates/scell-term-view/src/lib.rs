//! GPUI 终端视图。
//!
//! 只依赖 `scell-term`：输出读快照，输入通过注入的 [`TerminalIo`] 写出，不感知会话层。
//! 字形按单元格网格逐个放置，复用 GPUI 的字形图集与批处理，不自建渲染管线。

mod box_drawing;
mod clipboard;
mod element;
mod glyphs;
mod keys;
mod metrics;
mod mouse;
mod scrollbar;
mod style;
mod view;

pub use style::TerminalStyle;
pub use view::{
    Copy, NextPrompt, Paste, PasteGuard, PreviousPrompt, ScrollPageDown, ScrollPageUp, TerminalIo,
    TerminalView, init,
};
