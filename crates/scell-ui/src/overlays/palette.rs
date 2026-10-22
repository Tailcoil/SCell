//! 命令面板（Ctrl+Shift+P）。命令项绑定 Action，快捷键提示由组件自动显示；主题、材质与配置项在确认回调里处理。

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, Icon, IconName};
use gpui_kit::{
    Action, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window, div,
};
use rust_i18n::t;
use scell_config::HostId;
use scell_config::settings::{ThemeChoice, WindowMaterial, spelling};

use crate::actions::{
    ClosePane, CloseTab, DecreaseFontSize, Find, IncreaseFontSize, MoveToNewWindow, NewTab,
    NewWindow, NextTab, OpenSettings, OpenSftp, PrevTab, RenameTab, ResetFontSize, SplitDown,
    SplitRight, ToggleContextPanel, ToggleSidebar, ToggleTransfers, ToggleZoom,
};
use crate::appearance::Appearance;
use crate::config::{self, ConfigState};

pub(crate) enum PaletteEvent {
    Close,
    /// 用第几个 shell 配置档新建标签。
    OpenProfile(usize),
    ConnectHost(HostId),
}

/// 只列主机的面板用于快速连接（Ctrl+Shift+O）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteMode {
    All,
    Hosts,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Commands,
    Hosts,
    Profiles,
    Themes,
    Material,
    Config,
}

pub(crate) struct Palette {
    state: Entity<CommandState>,
    mode: PaletteMode,
    /// 打开时的主题列表与主机顺序；主机行渲染时再去掉已删除的。
    themes: Vec<String>,
    hosts: Vec<HostId>,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub(crate) fn new(mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            state: cx.new(|cx| CommandState::new(window, cx)),
            mode,
            themes: Appearance::themes(cx).0,
            hosts: config::host_order(cx),
        }
    }

    /// 当前模式下各分组的顺序，即 `IndexPath::section` 的含义。
    fn sections(&self) -> &'static [Section] {
        match self.mode {
            PaletteMode::All => &[
                Section::Commands,
                Section::Hosts,
                Section::Profiles,
                Section::Themes,
                Section::Material,
                Section::Config,
            ],
            PaletteMode::Hosts => &[Section::Hosts],
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        state.update(cx, |state, cx| state.focus(window, cx));
    }
}

/// 每条命令都带图标，否则有无图标的行文字对不齐。
fn command(label: impl Into<String>, icon: impl Into<Icon>, action: impl Action) -> CommandItem {
    CommandItem::new()
        .label(label.into())
        .icon(icon)
        .action(Box::new(action))
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, current_theme) = Appearance::themes(cx);
        let material = Appearance::material(cx);

        let commands = CommandGroup::new().label(t!("palette.commands")).items([
            command(t!("workspace.new_tab"), Lucide::Plus, NewTab),
            command(t!("palette.new_window"), Lucide::AppWindow, NewWindow),
            command(
                t!("palette.move_to_new_window"),
                Lucide::PictureInPicture2,
                MoveToNewWindow,
            ),
            command(t!("palette.next_tab"), Lucide::ArrowDown, NextTab),
            command(t!("palette.prev_tab"), Lucide::ArrowUp, PrevTab),
            command(t!("palette.rename_tab"), Lucide::Pencil, RenameTab),
            command(t!("files.open_sftp"), Lucide::Folders, OpenSftp),
            command(t!("palette.split_right"), Lucide::Columns2, SplitRight),
            command(t!("palette.split_down"), Lucide::Rows2, SplitDown),
            command(t!("palette.close_pane"), Lucide::SquareX, ClosePane),
            command(t!("palette.close_tab"), Lucide::Close, CloseTab),
            command(t!("palette.zoom"), Lucide::Maximize, ToggleZoom),
            command(t!("find.placeholder"), Lucide::Search, Find),
            command(
                t!("workspace.toggle_sidebar"),
                Lucide::PanelLeft,
                ToggleSidebar,
            ),
            command(
                t!("workspace.toggle_panel"),
                Lucide::PanelRight,
                ToggleContextPanel,
            ),
            command(t!("transfers.panel"), Lucide::PanelBottom, ToggleTransfers),
            command(t!("settings.title"), Lucide::Settings, OpenSettings),
            command(t!("palette.zoom_in"), Lucide::AArrowUp, IncreaseFontSize),
            command(t!("palette.zoom_out"), Lucide::AArrowDown, DecreaseFontSize),
            command(t!("palette.zoom_reset"), Lucide::ALargeSmall, ResetFontSize),
        ]);
        // 打开期间主机库可能变化：行与确认时取的 id 都来自这一份过滤后的列表
        let book = ConfigState::hosts(cx);
        let live_hosts: Vec<_> = self.hosts.iter().filter_map(|id| book.get(id)).collect();
        let hosts_snapshot: Vec<HostId> = live_hosts.iter().map(|h| h.id.clone()).collect();
        let hosts = CommandGroup::new()
            .label(t!("hosts.title"))
            .items(live_hosts.into_iter().map(|h| {
                CommandItem::new()
                    .label(h.name.clone())
                    .icon(crate::tabs::host_icon(h))
                    .keywords(
                        [
                            h.address(),
                            h.group.clone().unwrap_or_default(),
                            "ssh".into(),
                        ]
                        .into_iter()
                        .chain(h.tags.iter().cloned()),
                    )
            }));
        let profiles = CommandGroup::new().label(t!("workspace.new_tab")).items(
            crate::shells::profiles(cx).iter().map(|p| {
                CommandItem::new()
                    .label(p.name.clone())
                    .icon(crate::assets::prompt_icon())
                    .keywords(["shell", "new", "新建"])
            }),
        );
        let themes = CommandGroup::new()
            .label(t!("palette.theme"))
            .items(self.themes.iter().map(|name| {
                CommandItem::new()
                    .label(name.clone())
                    .keywords(["theme", "主题"])
                    .checked(*name == current_theme)
            }));
        let materials = CommandGroup::new().label(t!("palette.window")).items(
            WindowMaterial::ALL.into_iter().map(|m| {
                CommandItem::new()
                    .label(t!(format!("palette.material.{}", spelling(&m))).to_string())
                    .keywords(["window".to_owned(), "窗口".to_owned(), spelling(&m)])
                    .checked(m == material)
            }),
        );
        let configs = CommandGroup::new().label(t!("palette.config")).items([
            CommandItem::new()
                .label(t!("palette.open_settings").to_string())
                .icon(IconName::Settings),
            CommandItem::new()
                .label(t!("palette.open_themes").to_string())
                .icon(IconName::FolderOpen),
        ]);

        let this = cx.entity().downgrade();
        let sections = self.sections();
        let themes_snapshot = self.themes.clone();
        let mut groups = Vec::new();
        for section in sections {
            groups.push(match section {
                Section::Commands => commands.clone(),
                Section::Hosts => hosts.clone(),
                Section::Profiles => profiles.clone(),
                Section::Themes => themes.clone(),
                Section::Material => materials.clone(),
                Section::Config => configs.clone(),
            });
        }
        let placeholder = match self.mode {
            PaletteMode::All => t!("palette.placeholder"),
            PaletteMode::Hosts => t!("palette.quick_connect"),
        };
        let close = cx.entity().downgrade();
        div()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Close)))
            .w(crate::ui::metrics::PALETTE_WIDTH)
            // 背景、圆角与边框由组件画（与对话框、弹出菜单一致）；这里只给同样圆角的阴影
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(
                groups
                    .into_iter()
                    .fold(
                        Command::new(&self.state)
                            .placeholder(placeholder)
                            .bordered(true),
                        |command, group| command.group(group),
                    )
                    .on_confirm(move |ix, _, cx| {
                        match sections.get(ix.section) {
                            Some(Section::Hosts) => {
                                if let Some(id) = hosts_snapshot.get(ix.row).cloned() {
                                    let _ = this
                                        .update(cx, |_, cx| cx.emit(PaletteEvent::ConnectHost(id)));
                                }
                            }
                            Some(Section::Profiles) => {
                                let _ = this
                                    .update(cx, |_, cx| cx.emit(PaletteEvent::OpenProfile(ix.row)));
                            }
                            Some(Section::Themes) => {
                                if let Some(name) = themes_snapshot.get(ix.row) {
                                    config::set_theme(ThemeChoice::Fixed(name.clone()), cx);
                                }
                            }
                            Some(Section::Material) => {
                                if let Some(&m) = WindowMaterial::ALL.get(ix.row) {
                                    config::set_material(m, cx);
                                }
                            }
                            Some(Section::Config) if ix.row == 0 => config::open_settings_file(cx),
                            Some(Section::Config) => config::open_themes_dir(cx),
                            // 命令项的 Action 已由组件派发
                            _ => {}
                        }
                        let _ = this.update(cx, |_, cx| cx.emit(PaletteEvent::Close));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = close.update(cx, |_, cx| cx.emit(PaletteEvent::Close));
                    }),
            )
    }
}
