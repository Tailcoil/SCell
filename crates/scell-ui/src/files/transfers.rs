//! 传输列表：汇总传输引擎的任务快照，提供侧栏角标、弹出列表与冲突询问。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt, h_flex, v_flex,
};
use gpui_kit::{
    Anchor, App, AppContext, Context, Entity, Global, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use rust_i18n::t;
use scell_session::Encoding;
use scell_vfs::transfer::{Conflict, ConflictAction, ConflictPolicy, TransferState};
use scell_vfs::{Entry, TransferEvent, TransferId, TransferInfo, TransferRequest, Vfs, VfsPath};

use super::format_size;
use crate::app::AppState;
use crate::overlays::show_notification;
use crate::ui::form;
use crate::ui::metrics::TRANSFER_LIST_WIDTH;

/// 发起任务时记下的显示信息：按各自编码解码的名称与目标目录，下载时还有本机目标路径。
#[derive(Clone, Default)]
struct Meta {
    name: SharedString,
    dest_dir: SharedString,
    local: Option<PathBuf>,
    /// 目标侧的文件名编码，冲突询问按它显示名称。
    dest_encoding: Encoding,
}

struct Item {
    info: TransferInfo,
    meta: Meta,
}

pub(crate) struct TransferList {
    items: Vec<Item>,
    /// 已发起、尚未收到首个快照的任务。
    pending: HashMap<TransferId, Meta>,
}

struct GlobalTransfers(Entity<TransferList>);

impl Global for GlobalTransfers {}

pub(crate) fn init(cx: &mut App) {
    let list = cx.new(|_| TransferList {
        items: Vec::new(),
        pending: HashMap::new(),
    });
    cx.set_global(GlobalTransfers(list.clone()));
    let events = cx.global::<AppState>().sessions.transfer_events();
    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv_async().await {
            cx.update(|cx| match event {
                TransferEvent::Updated(info) => list.update(cx, |l, cx| l.update(info, cx)),
                TransferEvent::Conflict(conflict) => ask_conflict(conflict, cx),
                _ => {}
            });
        }
    })
    .detach();
}

pub(crate) fn list(cx: &App) -> Entity<TransferList> {
    cx.global::<GlobalTransfers>().0.clone()
}

impl TransferList {
    fn update(&mut self, info: TransferInfo, cx: &mut Context<Self>) {
        match self.items.iter_mut().find(|i| i.info.id == info.id) {
            Some(item) => {
                let finished_now =
                    !item.info.state.is_finished() && info.state == TransferState::Done;
                item.info = info;
                if finished_now && let Some(path) = item.meta.local.clone() {
                    notify_downloaded(item.meta.name.clone(), path, cx);
                }
            }
            None => {
                let meta = self.pending.remove(&info.id).unwrap_or_else(|| Meta {
                    name: lossy_name(&info.source),
                    dest_dir: info
                        .dest
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                    local: None,
                    dest_encoding: Encoding::UTF_8,
                });
                self.items.push(Item { info, meta });
            }
        }
        cx.notify();
    }

    /// 未结束的任务数。
    pub(crate) fn active(&self) -> usize {
        self.items
            .iter()
            .filter(|i| !i.info.state.is_finished())
            .count()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 全部未结束任务的总进度（0–100）；总量未知时为 `None`。
    fn progress(&self) -> Option<f32> {
        let (done, total) = self
            .items
            .iter()
            .filter(|i| !i.info.state.is_finished())
            .fold((0u64, 0u64), |(d, t), i| {
                (d + i.info.bytes_done, t + i.info.bytes_total)
            });
        (total > 0).then(|| done as f32 / total as f32 * 100.)
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        let engine = cx.global::<AppState>().sessions.transfers().clone();
        self.items.retain(|i| !engine.remove(i.info.id));
        cx.notify();
    }

    fn retry(&mut self, id: TransferId, cx: &mut Context<Self>) {
        let engine = cx.global::<AppState>().sessions.transfers().clone();
        if let Some(new) = engine.retry(id)
            && let Some(i) = self.items.iter().position(|i| i.info.id == id)
        {
            let item = self.items.remove(i);
            self.pending.insert(new, item.meta);
            cx.notify();
        }
    }

    fn remove(&mut self, id: TransferId, cx: &mut Context<Self>) {
        let engine = cx.global::<AppState>().sessions.transfers().clone();
        if engine.remove(id) {
            self.items.retain(|i| i.info.id != id);
            cx.notify();
        }
    }
}

/// 把 `entries` 复制到 `to_dir` 下。名称按两侧的编码转换；两侧编码相同时原样保留字节。
pub(crate) fn send(
    entries: &[Entry],
    from: (&Arc<dyn Vfs>, Encoding),
    to: (&Arc<dyn Vfs>, Encoding, &VfsPath),
    cx: &mut App,
) {
    enqueue(entries, from, to, false, cx);
}

fn enqueue(
    entries: &[Entry],
    from: (&Arc<dyn Vfs>, Encoding),
    to: (&Arc<dyn Vfs>, Encoding, &VfsPath),
    local_dest: bool,
    cx: &mut App,
) {
    let engine = cx.global::<AppState>().sessions.transfers().clone();
    let (to_vfs, to_encoding, to_dir) = to;
    let dest_dir: SharedString = to_encoding.decode(to_dir.as_bytes()).into_owned().into();
    let list = list(cx);
    let mut unmappable = Vec::new();
    for entry in entries {
        let display = from.1.decode(&entry.name).into_owned();
        let Some(name) = rename_bytes(&entry.name, from.1, to_encoding) else {
            unmappable.push(display);
            continue;
        };
        // 列表已过滤；转码后的名字也不能把目标带出目标目录
        if !VfsPath::is_component(&name) {
            tracing::warn!(name = ?VfsPath::new(name), "跳过无效的文件名");
            continue;
        }
        let dest_path = to_dir.join(&name);
        let meta = Meta {
            name: display.into(),
            dest_dir: dest_dir.clone(),
            local: local_dest.then(|| scell_vfs::LocalFs::to_path(&dest_path)),
            dest_encoding: to_encoding,
        };
        let id = engine.enqueue(TransferRequest {
            source: from.0.clone(),
            source_path: entry.path.clone(),
            dest: to_vfs.clone(),
            dest_path,
            conflict: ConflictPolicy::Ask,
            preserve: true,
        });
        // 首个快照经事件流异步到达，此时还没处理，先登记显示信息
        list.update(cx, |l, _| l.pending.insert(id, meta));
    }
    if !unmappable.is_empty() {
        let text = t!(
            "transfers.unmappable",
            encoding = to_encoding.name(),
            names = unmappable.join(", ")
        );
        show_notification(Notification::error(text.to_string()), cx);
    }
}

/// 上传本机路径到 `to_dir` 下。
pub(crate) fn upload(paths: &[PathBuf], to: (&Arc<dyn Vfs>, Encoding, &VfsPath), cx: &mut App) {
    let local = cx.global::<AppState>().sessions.local_vfs();
    let entries: Vec<Entry> = paths
        .iter()
        .filter_map(|p| {
            let name = p.file_name()?;
            Some(Entry {
                name: name.as_encoded_bytes().to_vec(),
                path: scell_vfs::LocalFs::from_path(p),
                metadata: Default::default(),
            })
        })
        .collect();
    send(&entries, (&local, Encoding::UTF_8), to, cx);
}

/// 下载到本机目录 `dir`，完成时通知并可打开所在文件夹。
pub(crate) fn download(
    entries: &[Entry],
    from: (&Arc<dyn Vfs>, Encoding),
    dir: &Path,
    cx: &mut App,
) {
    let local = cx.global::<AppState>().sessions.local_vfs();
    let dir = scell_vfs::LocalFs::from_path(dir);
    enqueue(entries, from, (&local, Encoding::UTF_8, &dir), true, cx);
}

fn lossy_name(path: &VfsPath) -> SharedString {
    path.file_name()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default()
        .into()
}

fn notify_downloaded(name: SharedString, path: PathBuf, cx: &mut App) {
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let note = Notification::success(t!("transfers.downloaded", dir = dir).to_string())
        .title(name.to_string())
        .action(move |_, _, _| {
            let path = path.clone();
            Button::new("reveal-download")
                .label(t!("transfers.reveal").to_string())
                .small()
                .on_click(move |_, _, cx| cx.reveal_path(&path))
        });
    show_notification(note, cx);
}

/// 两侧编码不同时按文字转换；目标编码表示不了时为 `None`。
fn rename_bytes(name: &[u8], from: Encoding, to: Encoding) -> Option<Vec<u8>> {
    if from == to {
        return Some(name.to_vec());
    }
    to.encode(&from.decode(name))
        .map(std::borrow::Cow::into_owned)
}

/// 在当前窗口弹出冲突询问；没有窗口时取消该任务。
fn ask_conflict(conflict: Conflict, cx: &mut App) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        conflict.answer(ConflictAction::Cancel, false);
        return;
    };
    let list = list(cx).read(cx);
    let encoding = list
        .pending
        .get(&conflict.transfer)
        .or_else(|| {
            list.items
                .iter()
                .find(|i| i.info.id == conflict.transfer)
                .map(|i| &i.meta)
        })
        .map_or(Encoding::UTF_8, |m| m.dest_encoding);
    let name: SharedString = conflict
        .path
        .file_name()
        .map(|n| encoding.decode(n).into_owned())
        .unwrap_or_default()
        .into();
    let size = |m: &scell_vfs::Metadata| m.size.map(format_size).unwrap_or_default();
    let detail: SharedString = t!(
        "transfers.sizes",
        existing = size(&conflict.existing),
        incoming = size(&conflict.incoming)
    )
    .into_owned()
    .into();
    let resumable = matches!(
        (conflict.existing.size, conflict.incoming.size),
        (Some(have), Some(want)) if have < want
    );
    let apply_all = std::rc::Rc::new(std::cell::Cell::new(false));
    let _ = window.update(cx, |_, window, cx| {
        window.open_dialog(cx, move |dialog, _, cx| {
            let answer = |action: ConflictAction| {
                let conflict = conflict.clone();
                let apply_all = apply_all.clone();
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    conflict.answer(action, apply_all.get());
                    window.close_dialog(cx);
                }
            };
            let button = |id: &'static str, label: SharedString, action: ConflictAction| {
                Button::new(id)
                    .label(label)
                    .small()
                    .on_click(answer(action))
            };
            let all = apply_all.clone();
            let checked = apply_all.get();
            dialog
                .bg(crate::appearance::Appearance::surface(cx))
                .title(t!("transfers.conflict", name = name.to_string()).to_string())
                .w(px(420.))
                .close_button(false)
                .keyboard(false)
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail.clone()),
                        )
                        .child(
                            Checkbox::new("conflict-all")
                                .label(t!("transfers.apply_all").to_string())
                                .checked(checked)
                                .on_click(move |on, window, _| {
                                    all.set(*on);
                                    window.refresh();
                                }),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            button(
                                "conflict-cancel",
                                t!("transfers.cancel").into(),
                                ConflictAction::Cancel,
                            )
                            .ghost(),
                        )
                        .child(button(
                            "conflict-skip",
                            t!("transfers.skip").into(),
                            ConflictAction::Skip,
                        ))
                        .child(button(
                            "conflict-rename",
                            t!("transfers.keep_both").into(),
                            ConflictAction::Rename,
                        ))
                        .when(resumable, |el| {
                            el.child(button(
                                "conflict-resume",
                                t!("transfers.resume").into(),
                                ConflictAction::Resume,
                            ))
                        })
                        .child(
                            button(
                                "conflict-overwrite",
                                t!("transfers.overwrite").into(),
                                ConflictAction::Overwrite,
                            )
                            .primary(),
                        ),
                )
        });
    });
}

/// 侧栏底部的传输按钮：有任务时出现，未结束任务数作为角标，点开为任务列表。
pub(crate) fn button(cx: &App) -> Option<impl IntoElement> {
    let list = list(cx);
    let state = list.read(cx);
    if state.is_empty() {
        return None;
    }
    let active = state.active();
    let theme = cx.theme();
    let badge = (active > 0).then(|| {
        div()
            .absolute()
            .top(px(-2.))
            .right(px(-2.))
            .min_w(px(14.))
            .h(px(14.))
            .px_0p5()
            .rounded_full()
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .text_size(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .child(active.to_string())
    });
    let popover = Popover::new("transfers-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(form::icon_button(
            "transfers",
            Icon::new(gpui_kit::assets::IconName::ArrowDownUp),
            t!("transfers.title"),
        ))
        .content(move |_, _, _| {
            div()
                .w(TRANSFER_LIST_WIDTH)
                .max_h(px(420.))
                .flex()
                .flex_col()
                .child(list.clone())
        });
    Some(div().relative().child(popover).children(badge))
}

impl Render for TransferList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let finished = self.items.iter().any(|i| i.info.state.is_finished());
        let total = self.progress();
        let rows: Vec<_> = self.items.iter().rev().map(|item| row(item, cx)).collect();
        let foreground = cx.theme().popover_foreground;
        v_flex()
            .size_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_1()
                    .text_sm()
                    .font_medium()
                    .child(div().flex_1().child(t!("transfers.title").to_string()))
                    .when(finished, |el| {
                        el.child(
                            Button::new("transfers-clear")
                                .label(t!("transfers.clear").to_string())
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.clear_finished(cx))),
                        )
                    })
                    .child(
                        form::icon_button(
                            "transfers-dock",
                            IconName::PanelBottom,
                            t!("transfers.panel"),
                        )
                        .xsmall()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::actions::ToggleTransfers), cx)
                        }),
                    ),
            )
            .when_some(total, |el, total| {
                el.child(Progress::new("transfers-total").value(total).h(px(3.)))
            })
            .child(
                v_flex()
                    .id("transfer-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(rows),
            )
            .text_color(foreground)
    }
}

fn row(item: &Item, cx: &mut Context<TransferList>) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let info = &item.info;
    let id = info.id;
    let ix = id.0 as usize;
    let percent = if info.bytes_total > 0 {
        info.bytes_done as f32 / info.bytes_total as f32 * 100.
    } else {
        0.
    };
    let status: SharedString = match &info.state {
        TransferState::Queued => t!("transfers.queued").into(),
        TransferState::Paused => t!("transfers.paused").into(),
        TransferState::Running => {
            let mut s = format!(
                "{} / {}",
                format_size(info.bytes_done),
                format_size(info.bytes_total)
            );
            if info.rate > 1. {
                s.push_str(&format!(" · {}/s", format_size(info.rate as u64)));
            }
            if info.files_total > 1 {
                s.push_str(&format!(" · {}/{}", info.files_done, info.files_total));
            }
            s.into()
        }
        TransferState::Done => {
            format!("{} · {}", format_size(info.bytes_total), item.meta.dest_dir).into()
        }
        TransferState::Skipped => t!("transfers.skipped").into(),
        TransferState::Cancelled => t!("transfers.cancelled").into(),
        TransferState::Failed(e) => e.clone().into(),
    };
    let engine = cx.global::<AppState>().sessions.transfers().clone();
    let actions = match &info.state {
        TransferState::Queued | TransferState::Running => vec![
            {
                let engine = engine.clone();
                form::icon_button(("t-pause", ix), IconName::Pause, t!("transfers.pause"))
                    .xsmall()
                    .on_click(move |_, _, _| engine.pause(id))
            },
            {
                let engine = engine.clone();
                form::icon_button(("t-cancel", ix), IconName::Close, t!("transfers.cancel"))
                    .xsmall()
                    .on_click(move |_, _, _| engine.cancel(id))
            },
        ],
        TransferState::Paused => vec![
            {
                let engine = engine.clone();
                form::icon_button(("t-resume", ix), IconName::Play, t!("transfers.continue"))
                    .xsmall()
                    .on_click(move |_, _, _| engine.resume(id))
            },
            {
                let engine = engine.clone();
                form::icon_button(("t-cancel", ix), IconName::Close, t!("transfers.cancel"))
                    .xsmall()
                    .on_click(move |_, _, _| engine.cancel(id))
            },
        ],
        TransferState::Failed(_) | TransferState::Cancelled => vec![
            form::icon_button(("t-retry", ix), IconName::RotateCw, t!("transfers.retry"))
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.retry(id, cx))),
            form::icon_button(("t-remove", ix), IconName::Close, t!("transfers.remove"))
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.remove(id, cx))),
        ],
        TransferState::Done | TransferState::Skipped => item
            .meta
            .local
            .clone()
            .filter(|_| info.state == TransferState::Done)
            .map(|path| {
                form::icon_button(
                    ("t-reveal", ix),
                    IconName::FolderOpen,
                    t!("transfers.reveal"),
                )
                .xsmall()
                .on_click(move |_, _, cx| cx.reveal_path(&path))
            })
            .into_iter()
            .chain([
                form::icon_button(("t-remove", ix), IconName::Close, t!("transfers.remove"))
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(id, cx))),
            ])
            .collect(),
    };
    let failed = matches!(info.state, TransferState::Failed(_));
    let running = !info.state.is_finished();
    let icon = if matches!(info.state, TransferState::Done) {
        Icon::new(IconName::CircleCheck).text_color(theme.success)
    } else if failed {
        Icon::new(IconName::CircleX).text_color(theme.danger)
    } else {
        Icon::new(IconName::File).text_color(theme.muted_foreground)
    };
    v_flex()
        .w_full()
        .gap_1()
        .py_1()
        .child(
            h_flex()
                .gap_2()
                .child(icon.small().flex_none())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(item.meta.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(if failed {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                })
                                .child(status),
                        ),
                )
                .child(h_flex().flex_none().children(actions)),
        )
        .when(running, |el| {
            el.child(
                Progress::new(("t-progress", ix))
                    .value(percent)
                    .loading(info.bytes_total == 0 && info.state == TransferState::Running)
                    .h(px(3.)),
            )
        })
        .into_any_element()
}
