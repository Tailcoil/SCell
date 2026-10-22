//! 设置页中的“主机”：左侧是可搜索的主机列表，右侧原位编辑选中的主机。

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Div, Entity, EventEmitter, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, WeakEntity, Window, div, prelude::FluentBuilder,
};
use rust_i18n::t;
use scell_config::hosts::{AuthPref, ForwardKind, ForwardRule, HostId, HostProfile};

use crate::config::{self, ConfigState};
use crate::ui::form::{field, fields, grow, icon_button, icon_segmented, select, select_field};
use crate::ui::metrics::{
    EDITOR_MAIN_MIN_WIDTH, EDITOR_SIDE_WIDTH, FORWARD_LISTEN_WIDTH, FORWARD_TARGET_BASIS,
    HOST_FIELD_BASIS, HOSTS_SPLIT_MIN, LIST_ROW_HEIGHT, LIST_ROW_TALL_HEIGHT, MASTER_LIST_WIDTH,
    PAGE_GUTTER, PORT_WIDTH, SHEET_WIDTH,
};
use crate::workspace::Workspace;
use gpui_kit::component::tab::{Tab, TabBar};

/// 编码下拉中的选项（WHATWG 标签）。
const ENCODINGS: &[&str] = &["utf-8", "gbk", "gb18030", "big5", "shift_jis", "euc-kr"];

/// 可选的图标颜色：设置文件中的名称与色值。中等明度，在明暗两种主题下都清楚。
const HOST_COLORS: &[(&str, u32)] = &[
    ("red", 0xef4444),
    ("orange", 0xf97316),
    ("amber", 0xf59e0b),
    ("yellow", 0xeab308),
    ("lime", 0x84cc16),
    ("green", 0x22c55e),
    ("teal", 0x14b8a6),
    ("cyan", 0x06b6d4),
    ("blue", 0x3b82f6),
    ("indigo", 0x6366f1),
    ("violet", 0x8b5cf6),
    ("pink", 0xec4899),
    ("slate", 0x64748b),
];

/// 主机图标的颜色；未设置时为 `None`（默认前景色）。
pub(crate) fn host_color(host: &HostProfile) -> Option<Hsla> {
    let name = host.color.as_deref()?;
    HOST_COLORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, hex)| gpui_kit::rgb(*hex).into())
}

/// 可选的主机图标：设置文件中的名称与对应图标，第一个为默认。
const HOST_ICONS: &[(&str, Lucide)] = &[
    ("server", Lucide::Server),
    ("database", Lucide::Database),
    ("cloud", Lucide::Cloud),
    ("globe", Lucide::Globe),
    ("container", Lucide::Container),
    ("router", Lucide::Router),
    ("cpu", Lucide::Cpu),
    ("hard-drive", Lucide::HardDrive),
    ("monitor", Lucide::Monitor),
    ("laptop", Lucide::Laptop),
    ("house", Lucide::House),
    ("shield", Lucide::Shield),
    ("flask-conical", Lucide::FlaskConical),
    ("rocket", Lucide::Rocket),
    ("bot", Lucide::Bot),
];

/// 主机的图标；未设置或名称不认识时为服务器图标。
pub(crate) fn host_icon(host: &HostProfile) -> Icon {
    let name = host.icon.as_deref().unwrap_or("server");
    let icon = HOST_ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(Lucide::Server, |(_, i)| *i);
    Icon::new(icon)
}

pub(crate) enum HostEntry {
    Group(String),
    /// `movable` 为假时不能拖动排序：收藏、最近使用里重复列出的行，以及搜索结果。
    Host {
        host: Box<HostProfile>,
        movable: bool,
    },
}

impl HostEntry {
    fn host(host: &HostProfile, movable: bool) -> Self {
        Self::Host {
            host: Box::new(host.clone()),
            movable,
        }
    }
}

/// 拖动中的主机。
#[derive(Clone)]
pub(crate) struct DraggedHost {
    id: HostId,
    name: SharedString,
    icon: Icon,
}

impl Render for DraggedHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .h(LIST_ROW_HEIGHT)
            .px_2()
            .gap_2()
            .rounded(theme.radius)
            .bg(theme.popover)
            .shadow_md()
            .text_sm()
            .text_color(theme.popover_foreground)
            .child(self.icon.clone().small())
            .child(self.name.clone())
    }
}

/// 让一行主机可以拖动，并接受其他主机拖到它上面：拖到哪台主机上就放到它的位置。
pub(crate) fn draggable(row: Stateful<Div>, host: &HostProfile) -> Stateful<Div> {
    let drag = DraggedHost {
        id: host.id.clone(),
        name: host.name.clone().into(),
        icon: host_icon(host),
    };
    let target = host.id.clone();
    row.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .drag_over::<DraggedHost>(|s, _, _, cx| s.bg(cx.theme().drop_target))
        .on_drop(move |dragged: &DraggedHost, _, cx| config::move_host(&dragged.id, &target, cx))
}

/// 侧栏顶部“最近”分组的长度。
const RECENT_SHOWN: usize = 5;

/// 主机列表的内容。有查询时按模糊匹配程度平铺；否则按主机库的显示顺序（未分组的在前，其余按分组聚在一起），
/// 在组前插入组名。`pinned` 时在最前面加上“收藏”与“最近”两组（与下方的分组重复列出）。
pub(crate) fn grouped(query: &str, pinned: bool, cx: &App) -> Vec<HostEntry> {
    let book = ConfigState::hosts(cx);
    if !query.trim().is_empty() {
        return scell_config::search_hosts(&book.hosts, query)
            .into_iter()
            .map(|h| HostEntry::host(h, false))
            .collect();
    }
    let mut entries = Vec::new();
    if pinned {
        let favorites: Vec<&HostProfile> = book.hosts.iter().filter(|h| h.favorite).collect();
        if !favorites.is_empty() {
            entries.push(HostEntry::Group(label("favorites").into()));
            entries.extend(favorites.into_iter().map(|h| HostEntry::host(h, false)));
        }
        let recent: Vec<&HostProfile> = ConfigState::recent_hosts(cx)
            .iter()
            .filter_map(|id| book.get(id))
            .filter(|h| !h.favorite)
            .take(RECENT_SHOWN)
            .collect();
        if !recent.is_empty() {
            entries.push(HostEntry::Group(label("recent").into()));
            entries.extend(recent.into_iter().map(|h| HostEntry::host(h, false)));
        }
    }
    let hosts = book.display_order();
    // 上面已有分组时，未分组的主机也要有个标题，否则看起来像属于上一组
    if !entries.is_empty() && hosts.first().is_some_and(|h| h.group.is_none()) {
        entries.push(HostEntry::Group(label("ungrouped").into()));
    }
    let mut group: Option<&String> = None;
    for host in hosts {
        if let Some(g) = host.group.as_ref().filter(|g| Some(*g) != group) {
            group = Some(g);
            // 嵌套分组 `a/b` 显示为 `a / b`
            let path: Vec<&str> = g.split('/').map(str::trim).collect();
            entries.push(HostEntry::Group(path.join(" / ")));
        }
        entries.push(HostEntry::host(host, true));
    }
    entries
}

pub(crate) fn group_heading(name: String, cx: &App) -> AnyElement {
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .overflow_x_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(name)
        .into_any_element()
}

/// 在右侧 Sheet 中编辑主机（`None` 为新建），保存、还原或删除后关闭。
pub(crate) fn open_host_sheet(
    id: Option<HostId>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = cx.new(|cx| HostEditor::new(id, workspace, window, cx));
    let handle = window.window_handle();
    cx.subscribe(&editor, move |_, _: &HostEditorEvent, cx| {
        let _ = handle.update(cx, |_, window, cx| window.close_sheet(cx));
    })
    .detach();
    window.open_sheet(cx, move |sheet, _, cx| {
        crate::ui::layout::floating_sheet(sheet, cx)
            .size(SHEET_WIDTH)
            .child(div().size_full().pb_4().child(editor.clone()))
    });
}

fn label(key: &str) -> SharedString {
    t!(format!("hosts.{key}")).to_string().into()
}

pub(crate) struct HostsPane {
    workspace: WeakEntity<Workspace>,
    search: Entity<InputState>,
    editor: Option<Entity<HostEditor>>,
    /// 单栏版式下正在看编辑器（否则看列表）。
    detail: bool,
    _subscriptions: Vec<Subscription>,
}

impl HostsPane {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(label("search")));
        let _subscriptions = vec![cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify())];
        let mut pane = Self {
            workspace,
            search,
            editor: None,
            detail: false,
            _subscriptions,
        };
        let first = ConfigState::hosts(cx).hosts.first().map(|h| h.id.clone());
        if let Some(id) = first {
            pane.edit(Some(id), window, cx);
        }
        // 单栏版式下先看列表
        pane.detail = false;
        pane
    }

    /// 打开编辑器：`None` 为新建。
    fn edit(&mut self, id: Option<HostId>, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let editor = cx.new(|cx| HostEditor::new(id, workspace, window, cx));
        self._subscriptions.truncate(1);
        self._subscriptions.push(
            cx.subscribe_in(&editor, window, |pane, _, event, window, cx| match event {
                HostEditorEvent::Reopen(id) => pane.edit(Some(id.clone()), window, cx),
                HostEditorEvent::Deleted => {
                    pane.detail = false;
                    let first = ConfigState::hosts(cx).hosts.first().map(|h| h.id.clone());
                    match first {
                        Some(id) => pane.edit(Some(id), window, cx),
                        None => {
                            pane.editor = None;
                            cx.notify();
                        }
                    }
                }
            }),
        );
        self.editor = Some(editor);
        self.detail = true;
        cx.notify();
    }

    fn selected(&self, cx: &App) -> Option<HostId> {
        self.editor.as_ref().map(|e| e.read(cx).id.clone())
    }

    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.read(cx).value();
        let selected = self.selected(cx);
        let theme = cx.theme().clone();
        let mut items: Vec<AnyElement> = Vec::new();
        for entry in grouped(&query, false, cx) {
            let (host, movable) = match entry {
                HostEntry::Group(name) => {
                    items.push(group_heading(name, cx));
                    continue;
                }
                HostEntry::Host { host, movable } => (host, movable),
            };
            let active = selected.as_ref() == Some(&host.id);
            let dot = host_color(&host);
            let id = host.id.clone();
            let connect_id = host.id.clone();
            let group_name: SharedString = format!("host-row-{}", host.id.0).into();
            let workspace = self.workspace.clone();
            items.push(
                h_flex()
                    .id(SharedString::from(format!("host-{}", host.id.0)))
                    .group(group_name.clone())
                    .gap_2()
                    .px_2()
                    .h(LIST_ROW_TALL_HEIGHT)
                    .rounded(theme.radius)
                    .map(|this| {
                        if active {
                            this.bg(theme.secondary_active)
                        } else {
                            this.hover(|s| s.bg(theme.secondary_hover))
                        }
                    })
                    .on_click(
                        cx.listener(move |pane, e: &gpui_kit::ClickEvent, window, cx| {
                            if e.click_count() >= 2 {
                                let _ = pane
                                    .workspace
                                    .update(cx, |ws, cx| ws.open_host(&id, window, cx));
                            } else {
                                pane.edit(Some(id.clone()), window, cx);
                            }
                        }),
                    )
                    .when(movable, |row| draggable(row, &host))
                    .child(
                        div().flex_none().child(
                            host_icon(&host)
                                .small()
                                .text_color(dot.unwrap_or(theme.muted_foreground)),
                        ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .overflow_x_hidden()
                                    .child(host.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .overflow_x_hidden()
                                    .child(host.address()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover(group_name, |s| s.visible())
                            .child(
                                icon_button(
                                    SharedString::from(format!("connect-{}", host.id.0)),
                                    IconName::Play,
                                    label("connect"),
                                )
                                .xsmall()
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = workspace
                                        .update(cx, |ws, cx| ws.open_host(&connect_id, window, cx));
                                }),
                            ),
                    )
                    .into_any_element(),
            );
        }
        v_flex().gap_0p5().children(items).into_any_element()
    }
}

impl Render for HostsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let adding = self.editor.as_ref().is_some_and(|e| e.read(cx).is_new(cx));
        let header = h_flex()
            .gap_1()
            .child(
                Input::new(&self.search)
                    .small()
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .flex_1(),
            )
            .child(
                icon_button(
                    "import-ssh-config",
                    gpui_kit::assets::IconName::Import,
                    label("import"),
                )
                .on_click(|_, window, cx| import_ssh_config(window, cx)),
            )
            .child(
                icon_button("add-host", IconName::Plus, label("add"))
                    .disabled(adding)
                    .on_click(cx.listener(|pane, _, window, cx| pane.edit(None, window, cx))),
            );
        // 列表与编辑器各自滚动；宽度不够并排时改为单栏，列表与编辑器之间切换
        let content = self
            .workspace
            .upgrade()
            .map(|ws| ws.read(cx).content_width(window));
        let compact = content.is_some_and(|w| w < HOSTS_SPLIT_MIN);
        if let (Some(editor), Some(width)) = (&self.editor, content) {
            // 编辑器可用宽度：内容区减去页面留白，并排时再减去列表
            let mut room = width - PAGE_GUTTER * 2.;
            if !compact {
                room -= MASTER_LIST_WIDTH + PAGE_GUTTER;
            }
            let two = room >= EDITOR_SIDE_WIDTH + PAGE_GUTTER + EDITOR_MAIN_MIN_WIDTH;
            editor.update(cx, |e, _| e.two_columns = two);
        }
        let list = v_flex().h_full().gap_2().child(header).child(
            div()
                .id("hosts-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(self.list(cx)),
        );
        if !compact {
            return h_flex()
                .size_full()
                .items_start()
                .gap_6()
                .child(list.w(MASTER_LIST_WIDTH).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .children(self.editor.clone()),
                )
                .into_any_element();
        }
        match self.editor.clone().filter(|_| self.detail) {
            Some(editor) => v_flex()
                .size_full()
                .gap_2()
                .child(
                    h_flex().child(
                        Button::new("hosts-back")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label(label("title"))
                            .on_click(cx.listener(|pane, _, _, cx| {
                                pane.detail = false;
                                cx.notify();
                            })),
                    ),
                )
                .child(div().flex_1().min_h_0().child(editor))
                .into_any_element(),
            None => list.w_full().into_any_element(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthChoice {
    Auto,
    Agent,
    Key,
    Password,
    Interactive,
    /// 单选表示不了的认证列表（如从 ssh_config 导入的多种方式），保存时原样保留。
    Custom,
}

impl AuthChoice {
    const ALL: [Self; 5] = [
        Self::Auto,
        Self::Agent,
        Self::Key,
        Self::Password,
        Self::Interactive,
    ];

    fn label(self) -> SharedString {
        label(match self {
            Self::Auto => "auth_auto",
            Self::Agent => "auth_agent",
            Self::Key => "auth_key",
            Self::Password => "auth_password",
            Self::Interactive => "auth_interactive",
            Self::Custom => "auth_custom",
        })
    }

    fn of(auth: &[AuthPref]) -> Self {
        match auth {
            [] => Self::Auto,
            [AuthPref::Agent] => Self::Agent,
            [AuthPref::Key { .. }] => Self::Key,
            [AuthPref::Password] => Self::Password,
            [AuthPref::KeyboardInteractive] => Self::Interactive,
            _ => Self::Custom,
        }
    }
}

/// 编辑器右栏的分页。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    General,
    Forwards,
    Advanced,
}

impl EditorTab {
    const ALL: [Self; 3] = [Self::General, Self::Forwards, Self::Advanced];
}

pub(crate) enum HostEditorEvent {
    /// 以存储中的内容重新打开该主机（保存后、还原时）。
    Reopen(HostId),
    Deleted,
}

pub(crate) struct HostEditor {
    id: HostId,
    workspace: WeakEntity<Workspace>,
    name: Entity<InputState>,
    group: Entity<InputState>,
    tags: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key_path: Entity<InputState>,
    notes: Entity<TextareaState>,
    forwards: Vec<ForwardRow>,
    tab: EditorTab,
    /// 左右两栏并排；由所在容器按宽度设置，否则上下排列。
    two_columns: bool,
    auth: AuthChoice,
    /// 经跳板机连接；为假时忽略 `jump`，切回来时恢复原先选的跳板。
    via_jump: bool,
    /// 跳板链，最外层在前。下拉只能选单跳，从 ssh_config 导入的多跳链作为一个选项保留。
    jump: Vec<HostId>,
    color: Option<String>,
    icon: Option<String>,
    encoding: String,
    monitor: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HostEditorEvent> for HostEditor {}

impl HostEditor {
    fn new(
        id: Option<HostId>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let profile = id
            .as_ref()
            .and_then(|id| ConfigState::hosts(cx).get(id))
            .cloned()
            .unwrap_or_else(|| HostProfile::new("", ""));
        let mut input = |value: String, placeholder: String| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let key = profile.auth.iter().find_map(|a| match a {
            AuthPref::Key { path } => Some(path.display().to_string()),
            _ => None,
        });
        let auth = AuthChoice::of(&profile.auth);
        let name = input(profile.name.clone(), String::new());
        let group = input(profile.group.clone().unwrap_or_default(), String::new());
        let tags = input(profile.tags.join(", "), String::new());
        let host = input(profile.host.clone(), String::new());
        let port = input(profile.port.to_string(), String::new());
        let user = input(
            profile.user.clone().unwrap_or_default(),
            scell_config::local_user().unwrap_or_default(),
        );
        let key_path = input(key.unwrap_or_default(), String::new());
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .default_value(profile.notes.clone())
        });
        // 任何输入变化都要重新判断“保存”是否可用
        let mut _subscriptions: Vec<Subscription> =
            [&name, &group, &tags, &host, &port, &user, &key_path]
                .into_iter()
                .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
                .collect();
        _subscriptions.push(cx.subscribe(&notes, |_, _, _: &InputEvent, cx| cx.notify()));
        let forwards = profile
            .forwards
            .iter()
            .map(|rule| ForwardRow::new(Some(rule), window, cx))
            .collect();
        Self {
            id: profile.id.clone(),
            workspace,
            name,
            group,
            tags,
            host,
            port,
            user,
            key_path,
            notes,
            forwards,
            tab: EditorTab::General,
            two_columns: true,
            auth,
            via_jump: !profile.jump.is_empty(),
            jump: profile.jump.clone(),
            color: profile.color.clone(),
            icon: profile.icon.clone(),
            encoding: profile.encoding.clone().unwrap_or_else(|| "utf-8".into()),
            monitor: profile.monitor,
            _subscriptions,
        }
    }

    fn stored<'a>(&self, cx: &'a App) -> Option<&'a HostProfile> {
        ConfigState::hosts(cx).get(&self.id)
    }

    fn is_new(&self, cx: &App) -> bool {
        self.stored(cx).is_none()
    }

    /// 读取表单；必填项缺失或端口无效时返回 `None`。
    fn profile(&self, cx: &App) -> Option<HostProfile> {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        let host = text(&self.host);
        let port = text(&self.port);
        let port = if port.is_empty() {
            22
        } else {
            port.parse::<u16>().ok().filter(|p| *p > 0)?
        };
        if host.is_empty() {
            return None;
        }
        let mut profile = self.stored(cx).cloned().unwrap_or_else(|| {
            let mut p = HostProfile::new("", "");
            p.id = self.id.clone();
            p
        });
        let name = text(&self.name);
        profile.name = if name.is_empty() { host.clone() } else { name };
        profile.host = host;
        profile.port = port;
        let optional = |s: String| (!s.is_empty()).then_some(s);
        profile.group = optional(text(&self.group));
        // 逗号（含中文逗号）分隔，去掉空项与重复
        profile.tags = Vec::new();
        for tag in text(&self.tags).split([',', '，']).map(str::trim) {
            if !tag.is_empty() && !profile.tags.iter().any(|t| t == tag) {
                profile.tags.push(tag.to_owned());
            }
        }
        profile.user = optional(text(&self.user));
        profile.notes = self.notes.read(cx).value().trim().to_owned();
        // 空行忽略；有一条不完整就不能保存
        let mut forwards = Vec::new();
        for row in &self.forwards {
            let (listen, target, note) = (text(&row.listen), text(&row.target), text(&row.note));
            if listen.is_empty() && target.is_empty() && note.is_empty() {
                continue;
            }
            let rule = ForwardRule {
                kind: row.kind,
                listen,
                target: if row.kind == ForwardKind::Dynamic {
                    String::new()
                } else {
                    target
                },
                note,
            };
            if !rule.is_valid() {
                return None;
            }
            forwards.push(rule);
        }
        profile.forwards = forwards;
        profile.auth = match self.auth {
            AuthChoice::Auto => Vec::new(),
            AuthChoice::Agent => vec![AuthPref::Agent],
            AuthChoice::Password => vec![AuthPref::Password],
            AuthChoice::Interactive => vec![AuthPref::KeyboardInteractive],
            AuthChoice::Key => {
                let path = text(&self.key_path);
                if path.is_empty() {
                    return None;
                }
                vec![AuthPref::Key { path: path.into() }]
            }
            AuthChoice::Custom => profile.auth,
        };
        profile.jump = if self.via_jump {
            self.jump.clone()
        } else {
            Vec::new()
        };
        profile.color = self.color.clone();
        // 默认图标不写入文件
        profile.icon = self.icon.clone().filter(|i| i != HOST_ICONS[0].0);
        profile.encoding = Some(self.encoding.clone()).filter(|e| e != "utf-8");
        profile.monitor = self.monitor;
        Some(profile)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(profile) = self.profile(cx) else {
            return;
        };
        config::update_hosts(|book| book.upsert(profile), cx);
        cx.emit(HostEditorEvent::Reopen(self.id.clone()));
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let id = self.id.clone();
        config::update_hosts(|book| book.remove(&id), cx);
        // 凭据库调用会阻塞；没存过时不会触发解锁对话框
        let secret = id.clone();
        cx.background_executor()
            .spawn(async move {
                scell_config::secrets::delete(scell_config::secrets::SecretKey::Password(&secret))
            })
            .detach();
        cx.emit(HostEditorEvent::Deleted);
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let stored = self.stored(cx);
        let title: SharedString = match stored {
            Some(h) => h.name.clone().into(),
            None => label("add"),
        };
        let address = stored.map(HostProfile::address);
        let id = self.id.clone();
        let workspace = self.workspace.clone();
        h_flex()
            .gap_3()
            .px_1()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_lg()
                            .font_medium()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_x_hidden()
                            .child(title),
                    )
                    .children(address.map(|a| {
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_x_hidden()
                            .child(a)
                    })),
            )
            .when_some(stored.map(|h| h.favorite), |this, favorite| {
                let favorite_id = self.id.clone();
                this.child(
                    icon_button(
                        "favorite-host",
                        if favorite {
                            IconName::StarFill
                        } else {
                            IconName::Star
                        },
                        label(if favorite { "unfavorite" } else { "favorite" }),
                    )
                    .ghost()
                    .when(favorite, |b| b.text_color(theme.warning))
                    .on_click(move |_, _, cx| config::toggle_favorite(&favorite_id, cx)),
                )
                .child(
                    icon_button(
                        "delete-host",
                        gpui_kit::assets::IconName::Trash,
                        label("delete"),
                    )
                    .danger()
                    .ghost()
                    .on_click(cx.listener(|e, _, _, cx| e.delete(cx))),
                )
                .child(
                    Button::new("connect-host")
                        .primary()
                        .small()
                        .icon(IconName::Play)
                        .label(label("connect"))
                        .on_click(move |_, window, cx| {
                            let _ = workspace.update(cx, |ws, cx| ws.open_host(&id, window, cx));
                        }),
                )
            })
            .into_any_element()
    }

    /// 保存与还原，始终在同一位置：没有修改时不可用，避免出现又消失让版面跳动。
    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let profile = self.profile(cx);
        let stored = self.stored(cx);
        let dirty = stored.is_none() || profile.as_ref() != stored;
        let new = stored.is_none();
        let id = self.id.clone();
        h_flex()
            .justify_end()
            .gap_2()
            .when(!new, |this| {
                this.child(
                    Button::new("revert-host")
                        .ghost()
                        .small()
                        .label(label("revert"))
                        .disabled(!dirty)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(HostEditorEvent::Reopen(id.clone()))
                        })),
                )
            })
            .child(
                Button::new("save-host")
                    .primary()
                    .small()
                    .label(label("save"))
                    .disabled(!dirty || profile.is_none())
                    .on_click(cx.listener(|e, _, _, cx| e.save(cx))),
            )
            .into_any_element()
    }
}

impl AuthChoice {
    fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName as I;
        match self {
            Self::Auto => I::Sparkles,
            Self::Agent => I::UserKey,
            Self::Key => I::KeyRound,
            Self::Password => I::LockKeyhole,
            Self::Interactive => I::Keyboard,
            Self::Custom => I::ListOrdered,
        }
    }
}

impl Render for HostEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 下拉与分段选择改的是编辑器自身的字段
        let entity = cx.entity().downgrade();
        let update = move |f: Box<dyn FnOnce(&mut Self)>, cx: &mut App| {
            let _ = entity.update(cx, |e, cx| {
                f(e);
                cx.notify();
            });
        };
        let update: Update = std::rc::Rc::new(update);
        let input = |state: &Entity<InputState>| Input::new(state).small();

        let route = {
            let update = update.clone();
            select(
                "host-route",
                self.via_jump,
                vec![(false, label("direct")), (true, label("jump"))],
                move |v, _, cx| update(Box::new(move |e| e.via_jump = v), cx),
            )
        };
        let mut general = vec![
            fields([
                field(label("connection"), route).into_any_element(),
                grow(field(label("address"), input(&self.host)), HOST_FIELD_BASIS),
                field(label("port"), input(&self.port))
                    .w(PORT_WIDTH)
                    .into_any_element(),
            ])
            .into_any_element(),
        ];
        if self.via_jump {
            // 以本机为跳板（直接或间接）的主机不能再作本机的跳板，否则成环
            let book = ConfigState::hosts(cx);
            let others = book
                .hosts
                .iter()
                .filter(|h| {
                    h.id != self.id
                        && book
                            .jump_chain(h)
                            .is_ok_and(|chain| chain.iter().all(|j| j.id != self.id))
                })
                .map(|h| (vec![h.id.clone()], SharedString::from(h.name.clone())));
            let stored_chain = self
                .stored(cx)
                .map(|h| h.jump.clone())
                .filter(|chain| chain.len() > 1)
                .map(|chain| {
                    let names = chain
                        .iter()
                        .map(|id| book.get(id).map_or("?", |h| h.name.as_str()))
                        .collect::<Vec<_>>()
                        .join(" → ");
                    (chain, SharedString::from(names))
                });
            let others = stored_chain.into_iter().chain(others).collect();
            let update = update.clone();
            let jump = select_field("host-jump", self.jump.clone(), others, move |v, _, cx| {
                update(Box::new(move |e| e.jump = v), cx)
            });
            general.push(field(label("jump"), jump).into_any_element());
        }
        general.push(field(label("user"), input(&self.user)).into_any_element());

        let auth = {
            let update = update.clone();
            let stored_custom = self
                .stored(cx)
                .is_some_and(|h| AuthChoice::of(&h.auth) == AuthChoice::Custom);
            icon_segmented(
                "host-auth",
                self.auth,
                AuthChoice::ALL
                    .into_iter()
                    .chain(stored_custom.then_some(AuthChoice::Custom))
                    .map(|a| (a, Icon::new(a.icon()), a.label()))
                    .collect(),
                move |choice, _, cx| update(Box::new(move |e| e.auth = choice), cx),
                cx,
            )
        };
        general.push(field(label("auth"), auth).into_any_element());
        if self.auth == AuthChoice::Key {
            general.push(field(label("key_path"), input(&self.key_path)).into_any_element());
        }
        if self.auth == AuthChoice::Custom
            && let Some(stored) = self.stored(cx)
        {
            let order = stored
                .auth
                .iter()
                .map(|a| match a {
                    AuthPref::Agent => AuthChoice::Agent.label().to_string(),
                    AuthPref::Key { path } => path.display().to_string(),
                    AuthPref::Password => AuthChoice::Password.label().to_string(),
                    AuthPref::KeyboardInteractive => AuthChoice::Interactive.label().to_string(),
                })
                .collect::<Vec<_>>()
                .join(" → ");
            general.push(
                field(
                    label("auth_order"),
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(order),
                )
                .into_any_element(),
            );
        }

        let encodings = ENCODINGS
            .iter()
            .map(|e| ((*e).to_owned(), SharedString::from(*e)))
            .collect();
        let encoding = {
            let update = update.clone();
            select_field(
                "host-encoding",
                self.encoding.clone(),
                encodings,
                move |v, _, cx| update(Box::new(move |e| e.encoding = v), cx),
            )
        };
        let content = match self.tab {
            EditorTab::General => general,
            EditorTab::Forwards => self.forward_rows(update.clone(), cx),
            EditorTab::Advanced => vec![
                field(label("encoding"), encoding).into_any_element(),
                {
                    let update = update.clone();
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(div().text_sm().child(label("monitor")))
                        .child(
                            gpui_kit::component::switch::Switch::new("host-monitor")
                                .checked(self.monitor)
                                .on_click(move |v: &bool, _, cx| {
                                    let v = *v;
                                    update(Box::new(move |e| e.monitor = v), cx)
                                }),
                        )
                        .into_any_element()
                },
                field(label("notes"), Textarea::new(&self.notes).small()).into_any_element(),
            ],
        };

        // 转发分页的标签旁用徽标标出规则数，不切过去也知道配了没有
        let theme = cx.theme().clone();
        let count = self.forwards.len();
        let tab = |t: EditorTab| {
            let title = match t {
                EditorTab::General => label("general"),
                EditorTab::Forwards => label("forwards"),
                EditorTab::Advanced => label("advanced"),
            };
            Tab::new()
                .label(title)
                .mr_4()
                .when(t == EditorTab::Forwards && count > 0, |tab| {
                    tab.suffix(
                        div()
                            .ml_1()
                            .px_1p5()
                            .rounded_full()
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    )
                })
        };
        let tabs = TabBar::new("host-editor-tabs")
            .underline()
            .small()
            .selected_index(
                EditorTab::ALL
                    .iter()
                    .position(|t| *t == self.tab)
                    .unwrap_or(0),
            )
            .children(EditorTab::ALL.map(tab))
            .on_click(cx.listener(|e, ix: &usize, _, cx| {
                e.tab = EditorTab::ALL[*ix];
                cx.notify();
            }));

        let side = v_flex()
            .flex_none()
            .map(|d| {
                if self.two_columns {
                    d.w(EDITOR_SIDE_WIDTH)
                } else {
                    d.w_full()
                }
            })
            .gap_4()
            .child(field(label("name"), input(&self.name)))
            .child(field(label("group"), input(&self.group)))
            .child(field(label("tags"), input(&self.tags)))
            .child(field(label("icon"), self.icons(cx)))
            .child(field(label("color"), self.colors(cx)));
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(tabs)
            .children(content);

        // 正常宽度下一屏放得下；只有窄到左右两栏折成上下时，内容区才在自身范围内滚动
        v_flex()
            .size_full()
            .gap_4()
            .child(self.header(cx))
            .child(
                div()
                    .id("host-editor-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(if self.two_columns {
                        h_flex()
                            .w_full()
                            .items_start()
                            .gap_6()
                            .child(side)
                            .child(main)
                    } else {
                        v_flex().w_full().gap_6().child(side).child(main)
                    }),
            )
            .child(self.footer(cx))
    }
}

/// 编辑中的一条端口转发。
struct ForwardRow {
    kind: ForwardKind,
    listen: Entity<InputState>,
    target: Entity<InputState>,
    note: Entity<InputState>,
    _subscriptions: [Subscription; 3],
}

impl ForwardRow {
    fn new(rule: Option<&ForwardRule>, window: &mut Window, cx: &mut Context<HostEditor>) -> Self {
        let mut input = |value: &str, placeholder: &str| {
            let (value, placeholder) = (value.to_owned(), placeholder.to_owned());
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let listen = input(rule.map_or("", |r| &r.listen), "");
        let target = input(rule.map_or("", |r| &r.target), "");
        let note = input(rule.map_or("", |r| &r.note), &label("notes"));
        let notify = |cx: &mut Context<HostEditor>, input: &Entity<InputState>| {
            cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify())
        };
        Self {
            kind: rule.map_or(ForwardKind::Local, |r| r.kind),
            _subscriptions: [notify(cx, &listen), notify(cx, &target), notify(cx, &note)],
            listen,
            target,
            note,
        }
    }
}

type Update = std::rc::Rc<dyn Fn(Box<dyn FnOnce(&mut HostEditor)>, &mut App)>;

impl HostEditor {
    /// 端口转发：每条一行（类型、监听、目标、删除），最后是“添加”。
    fn forward_rows(&self, update: Update, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let kinds = [
            (ForwardKind::Local, "forward_local"),
            (ForwardKind::Remote, "forward_remote"),
            (ForwardKind::Dynamic, "forward_dynamic"),
        ]
        .map(|(k, key)| (k, label(key)))
        .to_vec();
        let mut rows: Vec<AnyElement> = self
            .forwards
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let update = update.clone();
                let kind = select(
                    ("forward-kind", i),
                    row.kind,
                    kinds.clone(),
                    move |k, _, cx| {
                        update(
                            Box::new(move |e| {
                                if let Some(row) = e.forwards.get_mut(i) {
                                    row.kind = k;
                                }
                            }),
                            cx,
                        )
                    },
                );
                let dynamic = row.kind == ForwardKind::Dynamic;
                // 第一行：类型、监听 → 目标；第二行：备注与删除。窄栏里也不折行
                let mut route = vec![
                    kind.into_any_element(),
                    // 监听多为端口号，定宽；空间留给目标地址
                    div()
                        .flex_none()
                        .w(FORWARD_LISTEN_WIDTH)
                        .child(Input::new(&row.listen).small())
                        .into_any_element(),
                ];
                if !dynamic {
                    route.push(
                        Icon::new(IconName::ArrowRight)
                            .small()
                            .text_color(cx.theme().muted_foreground)
                            .into_any_element(),
                    );
                    route.push(grow(
                        div().child(Input::new(&row.target).small()),
                        FORWARD_TARGET_BASIS,
                    ));
                }
                let remove = icon_button(
                    ("forward-remove", i),
                    gpui_kit::assets::IconName::Trash,
                    label("delete"),
                )
                .on_click(cx.listener(move |e, _, _, cx| {
                    if i < e.forwards.len() {
                        e.forwards.remove(i);
                    }
                    cx.notify();
                }));
                v_flex()
                    .gap_1p5()
                    .child(fields(route).items_center().gap_2())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&row.note).small()))
                            .child(remove),
                    )
                    .into_any_element()
            })
            .collect();
        rows.push(
            h_flex()
                .child(
                    Button::new("forward-add")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label(label("forward_add"))
                        .on_click(cx.listener(|e, _, window, cx| {
                            let row = ForwardRow::new(None, window, cx);
                            e.forwards.push(row);
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        rows
    }

    /// 颜色选择：无，以及一组没有特定含义的颜色。
    fn colors(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current = self.color.clone();
        let swatch = |name: Option<&'static str>, cx: &mut Context<Self>| {
            let selected = current.as_deref() == name;
            let fill = name.and_then(|n| HOST_COLORS.iter().find(|(c, _)| *c == n));
            div()
                .id(SharedString::from(format!(
                    "host-color-{}",
                    name.unwrap_or("none")
                )))
                .size_5()
                .flex_none()
                .rounded_full()
                .border_2()
                .border_color(if selected {
                    theme.foreground
                } else {
                    theme.transparent
                })
                .p_0p5()
                .child(div().size_full().rounded_full().map(|d| match fill {
                    Some((_, hex)) => d.bg(gpui_kit::rgb(*hex)),
                    None => d.border_1().border_color(theme.muted_foreground),
                }))
                .on_click(cx.listener(move |e, _, _, cx| {
                    e.color = name.map(str::to_owned);
                    cx.notify();
                }))
                .into_any_element()
        };
        let mut items = vec![swatch(None, cx)];
        for (name, _) in HOST_COLORS {
            items.push(swatch(Some(name), cx));
        }
        h_flex()
            .flex_wrap()
            .gap_1()
            .children(items)
            .into_any_element()
    }

    /// 图标选择：一排可折行的图标按钮，选中的高亮。
    fn icons(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.icon.as_deref().unwrap_or(HOST_ICONS[0].0);
        h_flex()
            .flex_wrap()
            .gap_1()
            .children(HOST_ICONS.iter().map(|(name, icon)| {
                let name = *name;
                Button::new(SharedString::from(format!("host-icon-{name}")))
                    .icon(Icon::new(*icon))
                    .ghost()
                    .small()
                    .selected(name == current)
                    .on_click(cx.listener(move |e, _, _, cx| {
                        e.icon = Some(name.to_owned());
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }
}

/// 从 `~/.ssh/config` 合并主机，结果以通知告知。
fn import_ssh_config(window: &mut Window, cx: &mut App) {
    let Some(path) = scell_config::default_ssh_config() else {
        return;
    };
    let mut book = ConfigState::hosts(cx).clone();
    let notification = match scell_config::import_ssh_config(&mut book, &path) {
        Ok(0) => Notification::info(label("import_none")),
        Ok(count) => {
            config::update_hosts(|hosts| *hosts = book, cx);
            Notification::success(t!("hosts.imported", count = count).to_string())
        }
        Err(e) => Notification::error(e.to_string()),
    };
    window.push_notification(notification, cx);
}
