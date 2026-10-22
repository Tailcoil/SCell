//! 本机 shell 配置档。启动时在后台探测一次，供“新建标签”菜单与命令面板使用。

use std::sync::Arc;

use gpui_kit::{App, Global};
use scell_session::{ShellProfile, detect_shells};

#[derive(Default)]
struct Shells(Arc<[ShellProfile]>);

impl Global for Shells {}

pub(crate) fn init(cx: &mut App) {
    cx.set_global(Shells::default());
    let task = cx.background_executor().spawn(async { detect_shells() });
    cx.spawn(async move |cx| {
        let profiles = task.await;
        tracing::debug!(count = profiles.len(), "shell 配置档探测完成");
        cx.update(|cx| {
            cx.set_global(Shells(profiles.into()));
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 已探测到的配置档，探测完成前为空。
pub(crate) fn profiles(cx: &App) -> Arc<[ShellProfile]> {
    cx.global::<Shells>().0.clone()
}
