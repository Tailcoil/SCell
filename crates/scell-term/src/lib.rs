//! 终端状态机门面。
//!
//! 对外只暴露本 crate 的类型，VT 核心（当前为 alacritty_terminal）可整体替换。
//! 本 crate 纯同步、不依赖异步运行时；字节由会话层喂入，渲染层读取快照。

mod color;
mod engine;
mod event;
mod hint;
pub mod input;
mod osc;
mod prompt;
mod search;
mod selection;
mod size;
mod snapshot;

pub use color::{Palette, Rgb};
pub use engine::Terminal;
pub use event::{ClipboardKind, TermEvent};
pub use hint::Link;
pub use search::{SearchDirection, SearchQuery};
pub use selection::{SelectionKind, ViewportPoint};
pub use size::TermSize;
pub use snapshot::{CellAttrs, CursorShape, RenderSnapshot, SnapshotCell, SnapshotCursor};
