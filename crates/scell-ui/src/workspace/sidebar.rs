//! 左侧栏：顶部是面板开关与窗口按钮，其下切换“会话”（竖排标签）与“主机”（主机库）两个视图，
//! 底部是新建与设置。可折叠为图标栏。
//!
//! 标签行自绘而不用 `SidebarMenuItem`：后者不支持拖拽排序与行内重命名。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, AppContext, Context, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use rust_i18n::t;

use super::tab::{Launch, PaneStatus, TabId};
use super::{Workspace, chrome};
use crate::actions::{NewTab, OpenSettings, ToggleContextPanel, ToggleSidebar};
use crate::config::{self, ConfigState};
use crate::tabs::{HostEntry, draggable, group_heading, grouped, host_icon, open_host_sheet};
use crate::ui::metrics::{DOT, LIST_ROW_HEIGHT, SIDEBAR_COLLAPSED_WIDTH, SIDEBAR_WIDTH};
use crate::ui::motion;
use scell_config::HostId;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarView {
    Sessions,
    Hosts,
}

/// 拖动中的标签。
#[derive(Clone)]
pub(super) struct DraggedTab {
    id: TabId,
    title: SharedString,
    icon: Icon,
}

impl Render for DraggedTab {
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
            .child(self.title.clone())
    }
}

/// 侧栏顶部：面板开关、拖动区与窗口按钮。折叠为图标栏时只留两个面板开关。
fn header(collapsed: bool, window: &Window, cx: &mut Context<Workspace>) -> AnyElement {
    let toggle_sidebar = Button::new("toggle-sidebar")
        .icon(IconName::PanelLeft)
        .ghost()
        .small()
        .tooltip(t!("workspace.toggle_sidebar"))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx));
    let toggle_panel = Button::new("toggle-context-panel")
        .icon(IconName::PanelRight)
        .ghost()
        .small()
        .tooltip(t!("workspace.toggle_panel"))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleContextPanel), cx));
    if collapsed {
        v_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(toggle_sidebar)
            .child(toggle_panel)
            .into_any_element()
    } else {
        let controls = chrome::window_controls(window, cx);
        // 两个面板开关并排放在左侧；按钮放在拖动区之外，否则按下时会开始移动窗口
        h_flex()
            .w_full()
            .h(LIST_ROW_HEIGHT)
            .gap_1()
            .child(toggle_sidebar)
            .child(toggle_panel)
            .child(chrome::drag_area("sidebar-drag").flex_1().h_full())
            .children(controls)
            .into_any_element()
    }
}

fn tab_row(
    ws: &Workspace,
    index: usize,
    collapsed: bool,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let tab = &ws.tabs[index];
    let id = tab.id;
    let theme = cx.theme();
    let active = index == ws.active;
    let title = tab.display_title();
    let icon = tab.icon(cx);
    let panes = tab
        .terminals()
        .map(|t| t.panes.as_slice())
        .unwrap_or_default();
    // 断开与退出比连接中更需要注意，连接中比响铃更需要注意
    let badge = if panes
        .iter()
        .any(|p| p.status.ended() && p.status != PaneStatus::Idle)
    {
        Some(theme.danger)
    } else if panes.iter().any(|p| p.status == PaneStatus::Connecting) {
        Some(theme.info)
    } else if panes.iter().any(|p| p.bell) {
        Some(theme.warning)
    } else {
        None
    };
    let group: SharedString = format!("tab-row-{}", id.0).into();
    let renaming = ws
        .renaming
        .as_ref()
        .filter(|r| r.tab == id)
        .map(|r| r.input.clone());

    // SSH 标签的图标按主机颜色着色
    let env = tab
        .terminals()
        .and_then(|t| t.focused_pane())
        .and_then(|p| match &p.launch {
            Launch::Ssh { host, .. } => crate::tabs::host_color(ConfigState::hosts(cx).get(host)?),
            Launch::Local(_) => None,
        });
    let icon_el = div()
        .relative()
        .flex_none()
        .child(icon.clone().small().when_some(env, |i, c| i.text_color(c)))
        .when_some(badge, |this, color| {
            this.child(
                div()
                    .absolute()
                    .top(px(-2.))
                    .right(px(-2.))
                    .size(DOT)
                    .rounded_full()
                    .bg(color),
            )
        });

    let close = Button::new(("close-tab", id.0 as usize))
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .tooltip(t!("workspace.close_tab"))
        .on_click(cx.listener(move |ws, _, window, cx| {
            cx.stop_propagation();
            ws.close_tab(id, window, cx);
        }));

    let drag = DraggedTab {
        id,
        title: title.clone(),
        icon,
    };
    let row = h_flex()
        .id(("tab", id.0 as usize))
        .group(group.clone())
        .w_full()
        .h(LIST_ROW_HEIGHT)
        .flex_none()
        .px_2()
        .gap_2()
        .rounded(theme.radius)
        .text_sm()
        .overflow_x_hidden()
        .when(collapsed, |this| this.justify_center())
        .map(|this| {
            if active {
                this.font_medium()
                    .bg(theme.tokens.sidebar_accent)
                    .text_color(theme.sidebar_accent_foreground)
            } else {
                let hover = theme.sidebar_accent.opacity(0.8);
                this.hover(move |s| s.bg(hover))
            }
        })
        .on_click(
            cx.listener(move |ws, e: &gpui_kit::ClickEvent, window, cx| {
                if e.click_count() >= 2 {
                    ws.start_rename(id, window, cx);
                } else if let Some(index) = ws.tab_index(id) {
                    ws.activate(index, window, cx);
                }
            }),
        )
        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .drag_over::<DraggedTab>(|s, _, _, cx| s.bg(cx.theme().drop_target))
        .on_drop(cx.listener(move |ws, dragged: &DraggedTab, window, cx| {
            ws.move_tab(dragged.id, id, window, cx)
        }))
        .child(icon_el)
        .when(collapsed, |this| {
            let title = title.clone();
            this.tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
        })
        .when(!collapsed, |this| match renaming {
            Some(input) => this.child(Input::new(&input).xsmall().appearance(false).flex_1()),
            None => this
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_x_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .when(active, |s| s.visible())
                        .child(close),
                ),
        });

    let workspace = cx.entity().downgrade();
    let ssh = tab
        .terminals()
        .and_then(|t| t.focused_pane())
        .is_some_and(|p| matches!(p.launch, Launch::Ssh { .. }));
    let window_handle = window.window_handle();
    let row = row.context_menu(move |menu, window, cx| {
        let others = crate::app::other_windows(window_handle, cx);
        let source = workspace.clone();
        let move_to = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let to_new = source.clone();
            let menu =
                menu.item(PopupMenuItem::new(t!("tab.new_window")).on_click(
                    move |_, window, cx| crate::app::move_tab(&to_new, id, None, window, cx),
                ))
                .when(!others.is_empty(), PopupMenu::separator);
            others.iter().fold(menu, |menu, (handle, ws, label)| {
                let (source, target) = (source.clone(), (*handle, ws.clone()));
                menu.item(
                    PopupMenuItem::new(label.clone()).on_click(move |_, window, cx| {
                        crate::app::move_tab(&source, id, Some(target.clone()), window, cx)
                    }),
                )
            })
        };
        let (sftp, rename, close) = (workspace.clone(), workspace.clone(), workspace.clone());
        menu.when(ssh, |menu| {
            menu.item(
                PopupMenuItem::new(t!("files.open_sftp"))
                    .icon(Icon::new(gpui_kit::assets::IconName::Folders))
                    .on_click(move |_, window, cx| {
                        let _ = sftp.update(cx, |ws, cx| ws.open_tab_sftp(id, window, cx));
                    }),
            )
            .separator()
        })
        .item(
            PopupMenuItem::new(t!("tab.rename")).on_click(move |_, window, cx| {
                let _ = rename.update(cx, |ws, cx| ws.start_rename(id, window, cx));
            }),
        )
        .submenu(t!("tab.move_to"), window, cx, move_to)
        .item(
            PopupMenuItem::new(t!("workspace.close_tab")).on_click(move |_, window, cx| {
                let _ = close.update(cx, |ws, cx| ws.close_tab(id, window, cx));
            }),
        )
    });
    motion::fade_in(
        div().w_full().child(row),
        ("tab-row-enter", id.0 as usize),
        cx,
    )
    .into_any_element()
}

/// “会话 / 主机”切换，只在侧栏展开时显示。
fn view_switch(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let current = ws.sidebar_view;
    let views = [SidebarView::Sessions, SidebarView::Hosts];
    let title = |view| match view {
        SidebarView::Sessions => t!("workspace.sessions"),
        SidebarView::Hosts => t!("hosts.title"),
    };
    TabBar::new("sidebar-view")
        .segmented()
        .small()
        .w_full()
        .selected_index(views.iter().position(|v| *v == current).unwrap_or(0))
        .children(views.map(|view| Tab::new().label(title(view)).flex_1()))
        .on_click(cx.listener(move |ws, ix: &usize, _, cx| {
            ws.sidebar_view = views[*ix];
            cx.notify();
        }))
        .into_any_element()
}

/// 主机视图：单击连接，右键可编辑。已打开的主机名旁有一个点。
fn host_rows(ws: &Workspace, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let open: Vec<HostId> = ws
        .tabs
        .iter()
        .filter_map(|t| t.terminals())
        .flat_map(|t| &t.panes)
        .filter(|p| p.status == PaneStatus::Running)
        .filter_map(|p| match &p.launch {
            Launch::Ssh { host, .. } => Some(host.clone()),
            Launch::Local(_) => None,
        })
        .collect();
    let query = ws.host_search.read(cx).value();
    let theme = cx.theme().clone();
    let workspace = cx.entity().downgrade();
    let mut rows = vec![
        Input::new(&ws.host_search)
            .small()
            .prefix(Icon::new(IconName::Search).small())
            .cleanable(true)
            .into_any_element(),
    ];
    for entry in grouped(&query, true, cx) {
        let (host, movable) = match entry {
            HostEntry::Group(name) => {
                rows.push(group_heading(name, cx));
                continue;
            }
            HostEntry::Host { host, movable } => (host, movable),
        };
        let id = host.id.clone();
        let env = crate::tabs::host_color(&host);
        let connected = open.contains(&host.id);
        let icon = host_icon(&host)
            .small()
            .when_some(env, |i, c| i.text_color(c));
        let name: SharedString = host.name.clone().into();
        let row = h_flex()
            // 同一主机会在“收藏/最近”和所在分组各列一次，id 要区分开，否则两行共用点击状态
            .id(SharedString::from(format!(
                "sidebar-host-{}-{}",
                if movable { "list" } else { "pinned" },
                host.id.0
            )))
            .w_full()
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .px_2()
            .gap_2()
            .rounded(theme.radius)
            .text_sm()
            .hover(|s| s.bg(theme.sidebar_accent.opacity(0.8)))
            .on_click(cx.listener({
                let id = id.clone();
                move |ws, _, window, cx| ws.open_host(&id, window, cx)
            }))
            .when(movable, |row| draggable(row, &host))
            .child(div().flex_none().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(name),
            )
            .when(connected, |this| {
                this.child(div().flex_none().size(DOT).rounded_full().bg(theme.success))
            });
        let workspace = workspace.clone();
        let favorite = host.favorite;
        rows.push(
            row.context_menu(move |menu, _, _| {
                let connect = workspace.clone();
                let edit = workspace.clone();
                let connect_id = id.clone();
                let edit_id = id.clone();
                let favorite_id = id.clone();
                let sftp = workspace.clone();
                let sftp_id = id.clone();
                menu.item(
                    PopupMenuItem::new(t!("hosts.connect")).on_click(move |_, window, cx| {
                        let _ = connect.update(cx, |ws, cx| ws.open_host(&connect_id, window, cx));
                    }),
                )
                .item(
                    PopupMenuItem::new(t!("files.open_sftp")).on_click(move |_, window, cx| {
                        let _ =
                            sftp.update(cx, |ws, cx| ws.open_host_sftp(&sftp_id, None, window, cx));
                    }),
                )
                .item(
                    PopupMenuItem::new(t!("hosts.edit")).on_click(move |_, window, cx| {
                        open_host_sheet(Some(edit_id.clone()), edit.clone(), window, cx)
                    }),
                )
                .item(
                    PopupMenuItem::new(if favorite {
                        t!("hosts.unfavorite")
                    } else {
                        t!("hosts.favorite")
                    })
                    .on_click(move |_, _, cx| config::toggle_favorite(&favorite_id, cx)),
                )
            })
            .into_any_element(),
        );
    }
    rows
}

fn footer(view: SidebarView, cx: &mut Context<Workspace>) -> AnyElement {
    let workspace = cx.entity().downgrade();
    let new_tab = match view {
        SidebarView::Sessions => Button::new("new-tab")
            .icon(IconName::Plus)
            .ghost()
            .small()
            .tooltip(t!("workspace.new_tab"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NewTab), cx)),
        SidebarView::Hosts => {
            let workspace = workspace.clone();
            Button::new("add-host")
                .icon(IconName::Plus)
                .ghost()
                .small()
                .tooltip(t!("hosts.add"))
                .on_click(move |_, window, cx| open_host_sheet(None, workspace.clone(), window, cx))
        }
    };
    let settings = Button::new("open-settings")
        .icon(IconName::Settings)
        .ghost()
        .small()
        .tooltip(t!("settings.title"))
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx));
    if view == SidebarView::Hosts {
        return h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_1()
            .child(new_tab)
            .child(chrome::drag_area("sidebar-footer-drag").flex_1().h_full())
            .children(crate::files::transfers_button(cx))
            .child(settings)
            .into_any_element();
    }
    let profiles = Button::new("new-tab-profiles")
        .icon(IconName::ChevronUp)
        .ghost()
        .xsmall()
        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, cx| {
            let hosts_ws = workspace.clone();
            crate::shells::profiles(cx)
                .iter()
                .enumerate()
                .fold(menu, |menu, (index, profile)| {
                    let workspace = workspace.clone();
                    menu.item(
                        PopupMenuItem::new(profile.name.clone())
                            .icon(crate::assets::prompt_icon())
                            .on_click(move |_, window, cx| {
                                let _ = workspace
                                    .update(cx, |ws, cx| ws.open_profile(index, window, cx));
                            }),
                    )
                })
                .separator()
                .map(|menu| {
                    // 主机直接列出，收藏与最近使用在前
                    let book = ConfigState::hosts(cx);
                    let hosts: Vec<_> = config::host_order(cx)
                        .into_iter()
                        .filter_map(|id| book.get(&id).map(|h| (id, h.name.clone(), host_icon(h))))
                        .collect();
                    let scrollable = hosts.len() > 12;
                    hosts
                        .into_iter()
                        .fold(menu, |menu, (id, name, icon)| {
                            let workspace = hosts_ws.clone();
                            menu.item(PopupMenuItem::new(name).icon(icon).on_click(
                                move |_, window, cx| {
                                    let _ = workspace
                                        .update(cx, |ws, cx| ws.open_host(&id, window, cx));
                                },
                            ))
                        })
                        .scrollable(scrollable)
                })
        });
    h_flex()
        .w_full()
        .child(h_flex().child(new_tab).child(profiles))
        .child(chrome::drag_area("sidebar-footer-drag").flex_1().h_full())
        .children(crate::files::transfers_button(cx))
        .child(settings)
        .into_any_element()
}

pub(super) fn render(ws: &Workspace, window: &Window, cx: &mut Context<Workspace>) -> AnyElement {
    let collapsed = ws.sidebar_collapsed;
    let header = header(collapsed, window, cx);
    // 折叠后只剩面板开关与会话列表
    let switch = (!collapsed).then(|| view_switch(ws, cx));
    let rows: Vec<AnyElement> = match ws.sidebar_view {
        _ if collapsed => (0..ws.tabs.len())
            .map(|index| tab_row(ws, index, true, window, cx))
            .collect(),
        SidebarView::Sessions => (0..ws.tabs.len())
            .map(|index| tab_row(ws, index, false, window, cx))
            .collect(),
        SidebarView::Hosts => host_rows(ws, cx),
    };
    let footer = (!collapsed).then(|| footer(ws.sidebar_view, cx));
    let width = if collapsed {
        SIDEBAR_COLLAPSED_WIDTH
    } else {
        SIDEBAR_WIDTH
    };
    let body = v_flex()
        .h_full()
        .flex_none()
        .overflow_hidden()
        .p_2()
        .gap_2()
        .child(header)
        .children(switch)
        .child(
            v_flex()
                .id("tab-list")
                .flex_1()
                .min_h_0()
                .gap_0p5()
                .overflow_y_scroll()
                .children(rows)
                // 列表下方的空白也能拖动窗口；列表满了也留出至少一行
                .child(
                    chrome::drag_area("sidebar-list-drag")
                        .flex_1()
                        .min_h(LIST_ROW_HEIGHT),
                ),
        )
        .children(footer);
    motion::width(body, "sidebar", width, cx).into_any_element()
}
