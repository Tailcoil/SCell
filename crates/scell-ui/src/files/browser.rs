//! 文件浏览器：一个目录的虚拟化列表，加上路径栏与基本文件操作。本地与远程共用，
//! 文件系统由 [`Vfs`] 提供，所有调用经 [`SessionService::run`] 放到 IO 运行时执行。
//!
//! 传输不在这里发起：双击文件、“发送”菜单与拖放都以 [`BrowserEvent`] 交给上层，由上层决定目标。

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, WindowExt, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext, ClickEvent, ClipboardItem, Context, Entity, EntityId,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, UniformListScrollHandle, Window,
    canvas, div, prelude::FluentBuilder, px, uniform_list,
};
use rust_i18n::t;
use scell_session::{Encoding, SessionService};
use scell_vfs::{Entry, FileType, Vfs, VfsPath};

use super::{format_size, format_time};
use crate::app::AppState;
use crate::ui::form;
use crate::ui::metrics::{FILE_ROW_HEIGHT, FILE_SIZE_WIDTH, FILE_TIME_MIN_WIDTH, FILE_TIME_WIDTH};

gpui_kit::actions!(
    file_browser,
    [
        SelectPrev,
        SelectNext,
        OpenSelected,
        GoUp,
        DeleteSelected,
        RenameSelected,
        Refresh,
        SelectAll
    ]
);

pub(crate) const CONTEXT: &str = "FileBrowser";

pub(crate) fn bind_keys(cx: &mut App) {
    use gpui_kit::KeyBinding;
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("enter", OpenSelected, ctx),
        KeyBinding::new("backspace", GoUp, ctx),
        KeyBinding::new("alt-up", GoUp, ctx),
        KeyBinding::new("delete", DeleteSelected, ctx),
        KeyBinding::new("f2", RenameSelected, ctx),
        KeyBinding::new("f5", Refresh, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
    ]);
}

/// 交给上层处理的动作。
pub(crate) enum BrowserEvent {
    /// 把这些条目发送到另一侧（下载、上传或双栏间复制）。
    Send(Vec<Entry>),
    /// 选择本机目录后发送（“下载到…”）。
    SendTo(Vec<Entry>),
    /// 在 SFTP 标签页中打开（侧面板中的浏览器）。
    Expand,
    /// 有东西拖放到 `target` 目录。
    Drop { target: VfsPath, source: Dropped },
}

pub(crate) enum Dropped {
    /// 从另一个文件浏览器拖来的条目。
    Files(DraggedFiles),
    /// 从系统文件管理器拖来的本机路径。
    Paths(Vec<PathBuf>),
}

/// 拖动中的文件，同时是拖动时跟随光标的预览。
#[derive(Clone)]
pub(crate) struct DraggedFiles {
    pub(crate) source: EntityId,
    pub(crate) vfs: Arc<dyn Vfs>,
    pub(crate) encoding: Encoding,
    pub(crate) entries: Vec<Entry>,
    label: SharedString,
}

impl Render for DraggedFiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .h(FILE_ROW_HEIGHT)
            .px_2()
            .gap_2()
            .rounded(theme.radius)
            .bg(theme.popover)
            .shadow_md()
            .text_sm()
            .text_color(theme.popover_foreground)
            .child(Icon::new(IconName::File).small())
            .child(self.label.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortKey {
    Name,
    Size,
    Modified,
}

/// 列表中的一项：条目与解码后的显示名。
struct Row {
    entry: Entry,
    name: SharedString,
    /// 排序用的小写名。
    key: String,
    hidden: bool,
}

impl Row {
    fn is_dir(&self) -> bool {
        self.entry.metadata.is_dir()
    }
}

enum Load {
    Idle,
    Loading,
    Failed(SharedString),
}

/// 行内编辑：重命名或新建文件夹。
struct Edit {
    /// 重命名的原路径；`None` 为新建文件夹。
    target: Option<VfsPath>,
    input: Entity<InputState>,
    _events: Subscription,
}

pub(crate) struct FileBrowser {
    vfs: Arc<dyn Vfs>,
    encoding: Encoding,
    cwd: Option<VfsPath>,
    rows: Vec<Row>,
    /// 当前排序与过滤后显示的行，元素为 `rows` 的下标。
    visible: Vec<usize>,
    selected: HashSet<VfsPath>,
    /// 范围选择的起点与键盘光标，为 `visible` 的下标。
    cursor: Option<usize>,
    show_hidden: bool,
    sort: (SortKey, bool),
    /// 上一帧的宽度：窄时（侧面板、分屏的双栏）不显示修改时间，把位置留给名称。
    width: Rc<Cell<Pixels>>,
    /// “发送”菜单项的文案与图标；为空时不提供发送。
    send: Option<(SharedString, Icon)>,
    /// “发送到指定位置”菜单项的文案。
    send_to: Option<SharedString>,
    /// 提供“上传”菜单项（从本机选择文件）。
    upload: bool,
    /// 提供“在标签页中打开”菜单项。
    expandable: bool,
    load: Load,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    edit: Option<Edit>,
    _task: Option<Task<()>>,
}

impl EventEmitter<BrowserEvent> for FileBrowser {}

impl FileBrowser {
    /// 打开 `start`；为空时打开文件系统的初始目录。
    pub(crate) fn new(
        vfs: Arc<dyn Vfs>,
        encoding: Encoding,
        start: Option<VfsPath>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            vfs,
            encoding,
            cwd: None,
            rows: Vec::new(),
            visible: Vec::new(),
            selected: HashSet::new(),
            cursor: None,
            show_hidden: crate::config::ConfigState::settings(cx).files.show_hidden,
            sort: (SortKey::Name, false),
            width: Rc::new(Cell::new(px(0.))),
            send: None,
            send_to: None,
            upload: false,
            expandable: false,
            load: Load::Idle,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            edit: None,
            _task: None,
        };
        match start {
            Some(path) => this.navigate(path, cx),
            None => this.go_home(cx),
        }
        this
    }

    pub(crate) fn send_action(mut self, label: SharedString, icon: impl Into<Icon>) -> Self {
        self.send = Some((label, icon.into()));
        self
    }

    pub(crate) fn send_to_action(mut self, label: SharedString) -> Self {
        self.send_to = Some(label);
        self
    }

    pub(crate) fn uploadable(mut self, upload: bool) -> Self {
        self.upload = upload;
        self
    }

    pub(crate) fn expandable(mut self, expandable: bool) -> Self {
        self.expandable = expandable;
        self
    }

    pub(crate) fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    pub(crate) fn cwd(&self) -> Option<&VfsPath> {
        self.cwd.as_ref()
    }

    pub(crate) fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub(crate) fn display_path(&self, path: &VfsPath) -> String {
        self.encoding.decode(path.as_bytes()).into_owned()
    }

    fn sessions(cx: &App) -> SessionService {
        cx.global::<AppState>().sessions.clone()
    }

    fn go_home(&mut self, cx: &mut Context<Self>) {
        let vfs = self.vfs.clone();
        let io = Self::sessions(cx).run(async move { vfs.home().await });
        self.load = Load::Loading;
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = io.await;
            let _ = this.update(cx, |this, cx| match result {
                Some(Ok(home)) => this.navigate(home, cx),
                Some(Err(e)) => this.fail(e.to_string(), cx),
                None => this.fail(t!("files.closed").to_string(), cx),
            });
        }));
    }

    /// 进入目录。读取完成前保留原来的列表。
    pub(crate) fn navigate(&mut self, path: VfsPath, cx: &mut Context<Self>) {
        let vfs = self.vfs.clone();
        let dir = path.clone();
        let io = Self::sessions(cx).run(async move { vfs.list(&dir).await });
        self.load = Load::Loading;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = io.await;
            let _ = this.update(cx, |this, cx| match result {
                Some(Ok(entries)) => this.show(path, entries, cx),
                Some(Err(e)) => this.fail(e.to_string(), cx),
                None => this.fail(t!("files.closed").to_string(), cx),
            });
        }));
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.cwd.clone() {
            Some(cwd) => self.navigate(cwd, cx),
            None => self.go_home(cx),
        }
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.load = Load::Failed(message.into());
        cx.notify();
    }

    fn show(&mut self, path: VfsPath, entries: Vec<Entry>, cx: &mut Context<Self>) {
        let same_dir = self.cwd.as_ref() == Some(&path);
        self.rows = entries
            .into_iter()
            .map(|entry| {
                let name: SharedString = self.encoding.decode(&entry.name).into_owned().into();
                Row {
                    key: name.to_lowercase(),
                    hidden: entry.name.starts_with(b"."),
                    name,
                    entry,
                }
            })
            .collect();
        if same_dir {
            let paths: HashSet<&VfsPath> = self.rows.iter().map(|r| &r.entry.path).collect();
            self.selected.retain(|p| paths.contains(p));
        } else {
            self.selected.clear();
            self.cursor = None;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        self.cwd = Some(path);
        self.load = Load::Idle;
        self.resort();
        cx.notify();
    }

    fn resort(&mut self) {
        let rows = &self.rows;
        let mut visible: Vec<usize> = (0..rows.len())
            .filter(|&i| self.show_hidden || !rows[i].hidden)
            .collect();
        let (key, desc) = self.sort;
        visible.sort_by(|&a, &b| {
            let (a, b) = (&rows[a], &rows[b]);
            // 目录始终在前
            b.is_dir().cmp(&a.is_dir()).then_with(|| {
                let order = match key {
                    SortKey::Name => a.key.cmp(&b.key),
                    SortKey::Size => a.entry.metadata.size.cmp(&b.entry.metadata.size),
                    SortKey::Modified => a.entry.metadata.modified.cmp(&b.entry.metadata.modified),
                }
                .then_with(|| a.key.cmp(&b.key));
                if desc { order.reverse() } else { order }
            })
        });
        self.visible = visible;
        if self.cursor.is_some_and(|c| c >= self.visible.len()) {
            self.cursor = None;
        }
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            (k, desc) if k == key => (key, !desc),
            _ => (key, false),
        };
        self.resort();
        cx.notify();
    }

    fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let on = self.show_hidden;
        crate::config::update(
            "files",
            "show_hidden",
            Some(on.into()),
            |s| s.files.show_hidden = on,
            cx,
        );
        self.resort();
        cx.notify();
    }

    fn row(&self, visible: usize) -> Option<&Row> {
        self.visible.get(visible).map(|&i| &self.rows[i])
    }

    /// 选中的条目，按显示顺序。
    pub(crate) fn selection(&self) -> Vec<Entry> {
        self.visible
            .iter()
            .map(|&i| &self.rows[i].entry)
            .filter(|e| self.selected.contains(&e.path))
            .cloned()
            .collect()
    }

    /// 右键或拖动某一行时作用的条目：该行已选中时为全部选中项，否则只是该行。
    fn targets(&self, visible: usize) -> Vec<Entry> {
        let Some(row) = self.row(visible) else {
            return Vec::new();
        };
        if self.selected.contains(&row.entry.path) {
            self.selection()
        } else {
            vec![row.entry.clone()]
        }
    }

    fn click_row(
        &mut self,
        visible: usize,
        e: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some(path) = self.row(visible).map(|r| r.entry.path.clone()) else {
            return;
        };
        if e.click_count() >= 2 {
            self.open(visible, cx);
            return;
        }
        let modifiers = e.modifiers();
        if modifiers.shift
            && let Some(anchor) = self.cursor
        {
            let (a, b) = (anchor.min(visible), anchor.max(visible));
            self.selected = (a..=b)
                .filter_map(|i| self.row(i).map(|r| r.entry.path.clone()))
                .collect();
        } else if modifiers.secondary() {
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
            self.cursor = Some(visible);
        } else {
            self.selected = HashSet::from([path]);
            self.cursor = Some(visible);
        }
        cx.notify();
    }

    /// 右键时若该行未选中，改为只选中该行。
    fn select_for_menu(&mut self, visible: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.row(visible).map(|r| r.entry.path.clone())
            && !self.selected.contains(&path)
        {
            self.selected = HashSet::from([path]);
            self.cursor = Some(visible);
            cx.notify();
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        let next = match self.cursor {
            Some(c) => c.saturating_add_signed(delta).min(last),
            None if delta > 0 => 0,
            None => last,
        };
        self.cursor = Some(next);
        if let Some(path) = self.row(next).map(|r| r.entry.path.clone()) {
            self.selected = HashSet::from([path]);
        }
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// 目录进入；指向目录的符号链接先解析再进入；文件交给上层发送。
    fn open(&mut self, visible: usize, cx: &mut Context<Self>) {
        let Some(row) = self.row(visible) else {
            return;
        };
        let entry = row.entry.clone();
        match entry.metadata.file_type() {
            FileType::Dir => self.navigate(entry.path, cx),
            FileType::Symlink => {
                let vfs = self.vfs.clone();
                let path = entry.path.clone();
                let io = Self::sessions(cx).run(async move { vfs.stat(&path).await });
                cx.spawn(async move |this, cx| {
                    let dir = matches!(io.await, Some(Ok(m)) if m.is_dir());
                    let _ = this.update(cx, |this, cx| {
                        if dir {
                            this.navigate(entry.path, cx);
                        } else if this.send.is_some() {
                            cx.emit(BrowserEvent::Send(vec![entry]));
                        }
                    });
                })
                .detach();
            }
            _ if self.send.is_some() => cx.emit(BrowserEvent::Send(vec![entry])),
            _ => {}
        }
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.cwd.as_ref().and_then(VfsPath::parent) {
            self.navigate(parent, cx);
        }
    }

    fn start_edit(&mut self, target: Option<VfsPath>, window: &mut Window, cx: &mut Context<Self>) {
        let current = target
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| self.encoding.decode(n).into_owned())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current.clone()));
        let events = cx.subscribe_in(
            &input,
            window,
            |this, input, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let name = input.read(cx).value().trim().to_owned();
                    this.finish_edit(Some(name), window, cx);
                }
                InputEvent::Blur => this.finish_edit(None, window, cx),
                _ => {}
            },
        );
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            // 重命名时只选中主名，不含扩展名
            match current.rfind('.').filter(|&i| i > 0) {
                Some(dot) => input.set_selected_range(0..dot, cx),
                None => input.select_all(window, cx),
            }
        });
        self.edit = Some(Edit {
            target,
            input,
            _events: events,
        });
        cx.notify();
    }

    /// 结束行内编辑；`None` 为取消。
    fn finish_edit(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else {
            return;
        };
        window.focus(&self.focus, cx);
        cx.notify();
        let (Some(name), Some(cwd)) = (name.filter(|n| valid_name(n)), self.cwd.clone()) else {
            return;
        };
        let Some(bytes) = self
            .encoding
            .encode(&name)
            .map(std::borrow::Cow::into_owned)
        else {
            let text = t!(
                "transfers.unmappable",
                encoding = self.encoding.name(),
                names = name
            );
            self.notify_error(text.to_string(), window, cx);
            return;
        };
        let vfs = self.vfs.clone();
        let (from, to) = match edit.target {
            Some(from) => {
                let to = from.parent().unwrap_or(cwd).join(&bytes);
                if to == from {
                    return;
                }
                (Some(from), to)
            }
            None => (None, cwd.join(&bytes)),
        };
        let io = Self::sessions(cx).run(async move {
            match from {
                Some(from) => vfs.rename(&from, &to).await,
                None => vfs.mkdir(&to).await,
            }
            .map(|()| to)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = io.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Some(Ok(path)) => {
                        this.selected = HashSet::from([path]);
                    }
                    Some(Err(e)) => this.notify_error(e.to_string(), window, cx),
                    None => {}
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn rename(&mut self, visible: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let target = match visible {
            Some(v) => self.row(v).map(|r| r.entry.path.clone()),
            None => match self.selection().as_slice() {
                [one] => Some(one.path.clone()),
                _ => None,
            },
        };
        if let Some(target) = target {
            self.start_edit(Some(target), window, cx);
        }
    }

    fn confirm_delete(&mut self, entries: Vec<Entry>, window: &mut Window, cx: &mut Context<Self>) {
        if entries.is_empty() {
            return;
        }
        let message: SharedString = match entries.as_slice() {
            [one] => t!(
                "files.delete_one",
                name = self.encoding.decode(&one.name).into_owned()
            ),
            many => t!("files.delete_many", count = many.len()),
        }
        .into_owned()
        .into();
        let browser = cx.entity().downgrade();
        let entries = Arc::new(entries);
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let browser = browser.clone();
            let entries = entries.clone();
            alert
                .bg(crate::appearance::Appearance::surface(cx))
                .title(message.clone())
                .show_cancel(true)
                .ok_text(t!("files.delete").to_string())
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let entries = entries.clone();
                    let _ = browser.update(cx, |b, cx| b.delete(&entries, window, cx));
                    true
                })
        });
    }

    fn delete(&mut self, entries: &[Entry], window: &mut Window, cx: &mut Context<Self>) {
        let vfs = self.vfs.clone();
        let paths: Vec<VfsPath> = entries.iter().map(|e| e.path.clone()).collect();
        let io = Self::sessions(cx).run(async move {
            for path in &paths {
                scell_vfs::remove_all(vfs.as_ref(), path).await?;
            }
            Ok::<_, scell_vfs::VfsError>(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = io.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(Err(e)) = result {
                    this.notify_error(e.to_string(), window, cx);
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn notify_error(&self, message: String, window: &mut Window, cx: &mut App) {
        use gpui_kit::component::notification::Notification;
        window.push_notification(Notification::error(message), cx);
    }

    fn copy_path(&self, entries: &[Entry], cx: &mut App) {
        let text = entries
            .iter()
            .map(|e| self.display_path(&e.path))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn send(&mut self, entries: Vec<Entry>, cx: &mut Context<Self>) {
        if !entries.is_empty() {
            cx.emit(BrowserEvent::Send(entries));
        }
    }

    /// 从本机选择文件上传到当前目录。
    fn pick_upload(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else {
            return;
        };
        let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(t!("files.upload").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |this, cx| {
                    this.drop_on(cwd, Dropped::Paths(paths), cx);
                });
            }
        })
        .detach();
    }

    fn drop_on(&mut self, target: VfsPath, source: Dropped, cx: &mut Context<Self>) {
        if let Dropped::Files(files) = &source
            && files.source == cx.entity_id()
        {
            return;
        }
        cx.emit(BrowserEvent::Drop { target, source });
    }

    fn drag_label(&self, entries: &[Entry]) -> SharedString {
        match entries {
            [one] => self.encoding.decode(&one.name).into_owned().into(),
            many => t!("files.items", count = many.len()).into_owned().into(),
        }
    }
}

/// 文件名不能为空、不能含 `/`，也不能是 `.` 或 `..`。
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

impl Focusable for FileBrowser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- 渲染 ----

impl FileBrowser {
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let browser = cx.entity().downgrade();
        let more = Button::new("files-more")
            .icon(IconName::Ellipsis)
            .ghost()
            .small()
            .dropdown_menu(move |menu, _, cx| {
                let Some(this) = browser.upgrade() else {
                    return menu;
                };
                let hidden = this.read(cx).show_hidden;
                let upload = this.read(cx).upload;
                let expandable = this.read(cx).expandable;
                let expand = this.downgrade();
                let mkdir = this.downgrade();
                let toggle = this.downgrade();
                let pick = this.downgrade();
                menu.when(upload, |menu| {
                    menu.item(
                        PopupMenuItem::new(t!("files.upload_files"))
                            .icon(Icon::new(gpui_kit::assets::IconName::Upload))
                            .on_click(move |_, _, cx| {
                                let _ = pick.update(cx, |b, cx| b.pick_upload(cx));
                            }),
                    )
                })
                .item(
                    PopupMenuItem::new(t!("files.new_folder"))
                        .icon(Icon::new(gpui_kit::assets::IconName::FolderPlus))
                        .on_click(move |_, window, cx| {
                            let _ = mkdir.update(cx, |b, cx| b.start_edit(None, window, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new(t!("files.show_hidden"))
                        .checked(hidden)
                        .on_click(move |_, _, cx| {
                            let _ = toggle.update(cx, |b, cx| b.toggle_hidden(cx));
                        }),
                )
                .when(expandable, |menu| {
                    menu.separator().item(
                        PopupMenuItem::new(t!("files.open_tab"))
                            .icon(Icon::new(gpui_kit::assets::IconName::Folders))
                            .on_click(move |_, _, cx| {
                                let _ = expand.update(cx, |_, cx| cx.emit(BrowserEvent::Expand));
                            }),
                    )
                })
            });
        let loading = matches!(self.load, Load::Loading);
        h_flex()
            .w_full()
            .flex_none()
            .gap_1()
            .child(
                form::icon_button(
                    "files-up",
                    Icon::new(gpui_kit::assets::IconName::FolderUp),
                    t!("files.up"),
                )
                .disabled(self.cwd.as_ref().and_then(VfsPath::parent).is_none())
                .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
            )
            .child(self.breadcrumb(cx))
            .child(
                form::icon_button("files-refresh", IconName::RefreshCw, t!("files.refresh"))
                    .loading(loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(more)
    }

    /// 路径的各级目录，点击跳转。放不下时从左侧裁掉，末尾几级始终可见。
    fn breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut crumbs: Vec<(VfsPath, SharedString)> = Vec::new();
        let mut cur = self.cwd.clone();
        while let Some(path) = cur {
            let label = match path.file_name() {
                Some(name) => self.encoding.decode(name).into_owned(),
                None => self.display_path(&path),
            };
            cur = path.parent();
            crumbs.push((path, label.into()));
        }
        // crumbs 从末级到根排列，配合 row-reverse：溢出落在左侧被裁掉，路径短时整体仍靠左
        let muted = theme.muted_foreground;
        let hover = theme.secondary_hover;
        let radius = theme.radius;
        let count = crumbs.len();
        let mut children = Vec::with_capacity(count * 2);
        // 根目录（`/`、`C:/`）本身以分隔符结尾，紧跟其后的一级前不再加
        let parent_is_root: Vec<bool> = (0..count)
            .map(|i| crumbs.get(i + 1).is_some_and(|(_, l)| l.ends_with('/')))
            .collect();
        for (i, (path, label)) in crumbs.into_iter().enumerate() {
            let last = i == 0;
            children.push(
                div()
                    .id(("crumb", i))
                    .px_0p5()
                    .rounded(radius)
                    .map(|d| {
                        if last {
                            d.flex_none().font_medium()
                        } else {
                            d.flex_none().text_color(muted)
                        }
                    })
                    .hover(move |s| s.bg(hover))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(path.clone(), cx)))
                    .into_any_element(),
            );
            if i + 1 < count && !parent_is_root[i] {
                children.push(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child("/")
                        .into_any_element(),
                );
            }
        }
        h_flex().flex_1().min_w_0().child(
            h_flex()
                .flex_row_reverse()
                .max_w_full()
                .overflow_x_hidden()
                .text_sm()
                .children(children),
        )
    }

    fn show_time(&self) -> bool {
        self.width.get() >= FILE_TIME_MIN_WIDTH
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (key, desc) = self.sort;
        let column =
            |id: &'static str, label: SharedString, sort: SortKey, cx: &mut Context<Self>| {
                let arrow = (key == sort).then(|| {
                    Icon::new(if desc {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .xsmall()
                });
                h_flex()
                    .id(id)
                    .gap_0p5()
                    .cursor_pointer()
                    .child(label)
                    .children(arrow)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_sort(sort, cx)))
            };
        h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().child(column(
                "sort-name",
                t!("files.name").into(),
                SortKey::Name,
                cx,
            )))
            .child(div().w(FILE_SIZE_WIDTH).flex().justify_end().child(column(
                "sort-size",
                t!("files.size").into(),
                SortKey::Size,
                cx,
            )))
            .when(self.show_time(), |el| {
                el.child(div().w(FILE_TIME_WIDTH).child(column(
                    "sort-modified",
                    t!("files.modified").into(),
                    SortKey::Modified,
                    cx,
                )))
            })
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        range.map(|i| self.render_row(i, cx)).collect()
    }

    fn render_row(&self, visible: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.row(visible) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let selected = self.selected.contains(&row.entry.path);
        let is_dir = row.is_dir();
        let icon = match row.entry.metadata.file_type() {
            FileType::Dir => Icon::new(IconName::Folder),
            FileType::Symlink => Icon::new(gpui_kit::assets::IconName::FileSymlink),
            _ => Icon::new(IconName::File),
        };
        let editing = self
            .edit
            .as_ref()
            .filter(|e| e.target.as_ref() == Some(&row.entry.path))
            .map(|e| e.input.clone());
        let name: AnyElement = match editing {
            Some(input) => Input::new(&input).xsmall().into_any_element(),
            None => div()
                .truncate()
                .when(row.hidden, |d| d.text_color(theme.muted_foreground))
                .child(row.name.clone())
                .into_any_element(),
        };
        let size = (!is_dir)
            .then(|| row.entry.metadata.size.map(format_size))
            .flatten()
            .unwrap_or_default();
        let modified = row
            .entry
            .metadata
            .modified
            .map(format_time)
            .unwrap_or_default();
        let targets = self.targets(visible);
        let drag = DraggedFiles {
            source: cx.entity_id(),
            vfs: self.vfs.clone(),
            encoding: self.encoding,
            label: self.drag_label(&targets),
            entries: targets,
        };
        let path = row.entry.path.clone();
        let accent = theme.accent;
        let hover = theme.secondary_hover;
        let drop_bg = theme.drop_target;
        let browser = cx.entity().downgrade();
        let row_el = h_flex()
            .id(("file", visible))
            .w_full()
            .h(FILE_ROW_HEIGHT)
            .px_2()
            .gap_2()
            .rounded(theme.radius)
            .text_sm()
            .map(|el| {
                if selected {
                    el.bg(accent).text_color(theme.accent_foreground)
                } else {
                    el.hover(move |s| s.bg(hover))
                }
            })
            .child(icon.small().flex_none().text_color(if is_dir {
                theme.primary
            } else {
                theme.muted_foreground
            }))
            .child(div().flex_1().min_w_0().child(name))
            .child(
                div()
                    .w(FILE_SIZE_WIDTH)
                    .flex_none()
                    .flex()
                    .justify_end()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(size),
            )
            .when(self.show_time(), |el| {
                el.child(
                    div()
                        .w(FILE_TIME_WIDTH)
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(modified),
                )
            })
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                this.click_row(visible, e, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| this.select_for_menu(visible, cx)),
            )
            .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
            .when(is_dir, |el| {
                let files_target = path.clone();
                let paths_target = path.clone();
                el.drag_over::<DraggedFiles>(move |s, _, _, _| s.bg(drop_bg))
                    .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
                    .on_drop(cx.listener(move |this, d: &DraggedFiles, _, cx| {
                        cx.stop_propagation();
                        this.drop_on(files_target.clone(), Dropped::Files(d.clone()), cx);
                    }))
                    .on_drop(cx.listener(move |this, p: &ExternalPaths, _, cx| {
                        cx.stop_propagation();
                        this.drop_on(paths_target.clone(), Dropped::Paths(p.paths().to_vec()), cx);
                    }))
            });
        row_el
            .context_menu(move |menu, _, cx| row_menu(menu, browser.clone(), visible, cx))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Load::Failed(message) = &self.load
            && self.cwd.is_none()
        {
            return div()
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(message.clone())
                .into_any_element();
        }
        let theme = cx.theme();
        let drop_bg = theme.drop_target;
        let browser = cx.entity().downgrade();
        let new_folder = self.edit.as_ref().filter(|e| e.target.is_none()).map(|e| {
            h_flex()
                .h(FILE_ROW_HEIGHT)
                .px_2()
                .gap_2()
                .child(
                    Icon::new(IconName::Folder)
                        .small()
                        .text_color(theme.primary),
                )
                .child(div().flex_1().child(Input::new(&e.input).xsmall()))
        });
        let error = match &self.load {
            Load::Failed(message) => Some(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(message.clone()),
            ),
            _ => None,
        };
        v_flex()
            .id("files-list")
            .flex_1()
            .min_h_0()
            .children(error)
            .children(new_folder)
            .child(
                uniform_list("files", self.visible.len(), cx.processor(Self::render_rows))
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0(),
            )
            .drag_over::<DraggedFiles>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(|this, d: &DraggedFiles, _, cx| {
                if let Some(cwd) = this.cwd.clone() {
                    this.drop_on(cwd, Dropped::Files(d.clone()), cx);
                }
            }))
            .on_drop(cx.listener(|this, p: &ExternalPaths, _, cx| {
                if let Some(cwd) = this.cwd.clone() {
                    this.drop_on(cwd, Dropped::Paths(p.paths().to_vec()), cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
                if !this.selected.is_empty() {
                    this.selected.clear();
                    cx.notify();
                }
            }))
            .context_menu(move |menu, _, cx| blank_menu(menu, browser.clone(), cx))
            .into_any_element()
    }
}

fn row_menu(
    menu: PopupMenu,
    browser: gpui_kit::WeakEntity<FileBrowser>,
    visible: usize,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(this) = browser.upgrade() else {
        return menu;
    };
    let (send, send_to) = {
        let b = this.read(cx);
        (b.send.clone(), b.send_to.clone())
    };
    let targets = this.read(cx).targets(visible);
    let single_dir = matches!(targets.as_slice(), [one] if one.metadata.is_dir());
    let single = targets.len() == 1;
    let action =
        |f: fn(&mut FileBrowser, usize, Vec<Entry>, &mut Window, &mut Context<FileBrowser>)| {
            let browser = browser.clone();
            let targets = targets.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let _ = browser.update(cx, |b, cx| f(b, visible, targets.clone(), window, cx));
            }
        };
    menu.when(single_dir, |menu| {
        menu.item(
            PopupMenuItem::new(t!("files.open")).on_click(action(|b, v, _, _, cx| b.open(v, cx))),
        )
    })
    .when_some(send, |menu, (label, icon)| {
        menu.item(
            PopupMenuItem::new(label)
                .icon(icon)
                .on_click(action(|b, _, t, _, cx| b.send(t, cx))),
        )
    })
    .when_some(send_to, |menu, label| {
        menu.item(PopupMenuItem::new(label).on_click(action(|_, _, t, _, cx| {
            if !t.is_empty() {
                cx.emit(BrowserEvent::SendTo(t));
            }
        })))
    })
    .separator()
    .when(single, |menu| {
        menu.item(
            PopupMenuItem::new(t!("files.rename"))
                .icon(Icon::new(gpui_kit::assets::IconName::Pencil))
                .on_click(action(|b, v, _, w, cx| b.rename(Some(v), w, cx))),
        )
    })
    .item(
        PopupMenuItem::new(t!("files.copy_path"))
            .icon(Icon::new(IconName::Copy))
            .on_click(action(|b, _, t, _, cx| b.copy_path(&t, cx))),
    )
    .separator()
    .item(
        PopupMenuItem::new(t!("files.delete"))
            .icon(Icon::new(IconName::Delete))
            .on_click(action(|b, _, t, w, cx| b.confirm_delete(t, w, cx))),
    )
}

fn blank_menu(
    menu: PopupMenu,
    browser: gpui_kit::WeakEntity<FileBrowser>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(this) = browser.upgrade() else {
        return menu;
    };
    let hidden = this.read(cx).show_hidden;
    let upload = this.read(cx).upload;
    let mkdir = browser.clone();
    let refresh = browser.clone();
    let toggle = browser.clone();
    let pick = browser.clone();
    menu.when(upload, |menu| {
        menu.item(
            PopupMenuItem::new(t!("files.upload_files"))
                .icon(Icon::new(gpui_kit::assets::IconName::Upload))
                .on_click(move |_, _, cx| {
                    let _ = pick.update(cx, |b, cx| b.pick_upload(cx));
                }),
        )
    })
    .item(
        PopupMenuItem::new(t!("files.new_folder"))
            .icon(Icon::new(gpui_kit::assets::IconName::FolderPlus))
            .on_click(move |_, window, cx| {
                let _ = mkdir.update(cx, |b, cx| b.start_edit(None, window, cx));
            }),
    )
    .item(
        PopupMenuItem::new(t!("files.refresh"))
            .icon(Icon::new(IconName::RefreshCw))
            .on_click(move |_, _, cx| {
                let _ = refresh.update(cx, |b, cx| b.refresh(cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(t!("files.show_hidden"))
            .checked(hidden)
            .on_click(move |_, _, cx| {
                let _ = toggle.update(cx, |b, cx| b.toggle_hidden(cx));
            }),
    )
}

impl Render for FileBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .relative()
            .size_full()
            .gap_1()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                if let Some(c) = this.cursor {
                    this.open(c, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.go_up(cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(
                cx.listener(|this, _: &RenameSelected, window, cx| this.rename(None, window, cx)),
            )
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                let entries = this.selection();
                this.confirm_delete(entries, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.selected = this
                    .visible
                    .iter()
                    .map(|&i| this.rows[i].entry.path.clone())
                    .collect();
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                    this.finish_edit(None, window, cx)
                }),
            )
            .child(self.toolbar(cx))
            .child(self.header(cx))
            .child(self.render_list(cx))
            .child(self.width_probe(cx))
    }
}

impl FileBrowser {
    /// 记下宽度，跨过显示修改时间的阈值时重绘一次。
    fn width_probe(&self, cx: &Context<Self>) -> impl IntoElement {
        let measured = self.width.clone();
        let id = cx.entity_id();
        canvas(
            move |bounds, _, cx| {
                let wide = |w| w >= FILE_TIME_MIN_WIDTH;
                let before = measured.replace(bounds.size.width);
                if wide(before) != wide(bounds.size.width) {
                    cx.notify(id);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }
}
