//! SCell 的 GPUI 表现层：窗口、工作区、侧栏与覆盖层。
//!
//! 会话由 [`scell_session::SessionService`] 持有；本 crate 只持有句柄与视图，
//! 通过事件流感知会话变化。

rust_i18n::i18n!("locales", fallback = "en");

mod actions;
mod app;
mod appearance;
mod assets;
mod blur;
mod config;
mod files;
mod fonts;
mod overlays;
mod panels;
mod shells;
mod tabs;
mod ui;
mod updates;
mod workspace;

pub use app::{init, open_main_window};
pub use assets::Assets;
pub use fonts::BundledFont;
pub use updates::run_installer_hooks;
