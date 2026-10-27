//! 设置页。每项直接读写全局设置：修改立即生效并写回 `settings.toml`，文件被外部修改时页面随热重载刷新。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::input::{Input, NumberInput};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, img, prelude::FluentBuilder,
};
use rust_i18n::t;
use scell_config::Settings as Config;
use scell_config::settings::{
    BUNDLED_FONT, CursorStyle, FileSettings, TerminalSettings, ThemeChoice, Value, WindowMaterial,
};
use scell_config::settings::{limits, spelling};
use std::path::{Path, PathBuf};

use super::hosts::HostsPane;
use crate::appearance::Appearance;
use crate::config::{self, ConfigState};
use crate::fonts;
use crate::ui::form::{
    self, CONTROL_WIDTH, Choice, bound_input, bound_number, row, section, select,
};
use crate::ui::layout::{self, Body, column};
use crate::ui::metrics::{
    ABOUT_LOGO, NUMBER_WIDTH, PAGE_WIDE_WIDTH, PAGE_WIDTH, SETTINGS_TABS_MIN,
};
use crate::ui::motion;
use crate::updates::{self, UpdateState};
use crate::workspace::Workspace;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Terminal,
    Shell,
    Files,
    Hosts,
    About,
}

impl Page {
    const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Terminal,
        Self::Shell,
        Self::Files,
        Self::Hosts,
        Self::About,
    ];

    fn title(self) -> SharedString {
        match self {
            Self::Appearance => t!("settings.appearance"),
            Self::Terminal => t!("settings.terminal"),
            Self::Shell => t!("settings.shell"),
            Self::Files => t!("files.title"),
            Self::Hosts => t!("hosts.title"),
            Self::About => t!("settings.about"),
        }
        .to_string()
        .into()
    }
}

pub(crate) struct SettingsView {
    focus: FocusHandle,
    page: Page,
    hosts: Entity<HostsPane>,
    workspace: WeakEntity<Workspace>,
}

impl SettingsView {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            page: Page::Appearance,
            hosts: cx.new(|cx| HostsPane::new(workspace.clone(), window, cx)),
            workspace,
        }
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page;
        // 主机页是左右两栏，放宽内容列；标题栏居中，不随之移动
        let width = if page == Page::Hosts {
            PAGE_WIDE_WIDTH
        } else {
            PAGE_WIDTH
        };
        // 内容区窄到放不下全部分页时，标签栏可滚动，另给一个溢出菜单
        let narrow = self
            .workspace
            .upgrade()
            .is_some_and(|ws| ws.read(cx).content_width(window) < SETTINGS_TABS_MIN);
        let tabs = TabBar::new("settings-pages")
            .segmented()
            .small()
            .menu(narrow)
            .selected_index(Page::ALL.iter().position(|p| *p == page).unwrap_or(0))
            .children(Page::ALL.map(|p| Tab::new().label(p.title())))
            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                view.page = Page::ALL[*ix];
                cx.notify();
            }));
        let open_file = form::icon_button(
            "open-settings-file",
            IconName::ExternalLink,
            t!("settings.open_file"),
        )
        .on_click(|_, _, cx| config::open_settings_file(cx))
        .into_any_element();
        let body: AnyElement = match page {
            Page::Appearance => column(appearance(window, cx)),
            Page::Terminal => column(terminal(window, cx)),
            Page::Shell => column(shell(window, cx)),
            Page::Files => column(files(window, cx)),
            Page::About => column(about(cx)),
            Page::Hosts => self.hosts.clone().into_any_element(),
        };
        // 主机页是主从布局，列表与编辑器各自管理滚动；其余是整列滚动的表单
        let mode = if page == Page::Hosts {
            Body::Fill
        } else {
            Body::Scroll
        };
        // 以页面作为 id 的一部分，切换子页面时新内容淡入
        let body = motion::fade_in(
            div()
                .w_full()
                .when(mode == Body::Fill, |d| d.h_full())
                .child(body),
            ("settings-page", page as usize),
            cx,
        );
        div()
            .track_focus(&self.focus)
            .size_full()
            .child(layout::page(
                "settings-body",
                tabs,
                Some(open_file),
                width,
                mode,
                body,
            ))
    }
}

fn settings(cx: &App) -> &Config {
    ConfigState::settings(cx)
}

fn label(key: &str) -> SharedString {
    t!(format!("settings.{key}")).to_string().into()
}

fn appearance(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let (names, _) = Appearance::themes(cx);
    let themes = std::iter::once((None, label("follow_system")))
        .chain(names.into_iter().map(|n| (Some(n.clone()), n.into())))
        .collect();
    let theme = match &settings(cx).appearance.theme {
        ThemeChoice::Fixed(name) => Some(name.clone()),
        ThemeChoice::System { .. } => None,
    };
    let materials = WindowMaterial::ALL
        .into_iter()
        .map(|m| {
            let label = t!(format!("palette.material.{}", spelling(&m))).to_string();
            (m, label.into())
        })
        .collect();
    let material = settings(cx).appearance.window;

    let mut rows = vec![
        row(
            label("theme"),
            select("theme", theme, themes, |choice, _, cx| {
                config::set_theme(
                    choice.map_or_else(ThemeChoice::default, ThemeChoice::Fixed),
                    cx,
                )
            }),
        )
        .into_any_element(),
        row(
            label("window"),
            form::segmented("window", material, materials, |m, _, cx| {
                config::set_material(m, cx)
            }),
        )
        .into_any_element(),
    ];
    // 不透明时透明度没有意义，直接不显示
    if material != WindowMaterial::Opaque {
        let opacity = bound_number(
            "opacity",
            wide(limits::OPACITY),
            0.05,
            |cx| f64::from(settings(cx).appearance.opacity),
            |v, cx| {
                config::update(
                    "appearance",
                    "opacity",
                    Some(v.into()),
                    |s| s.appearance.opacity = v as f32,
                    cx,
                )
            },
            window,
            cx,
        );
        rows.push(row(label("opacity"), number(&opacity)).into_any_element());
    }
    vec![section(None, rows, cx), interface(window, cx)]
}

/// 界面字体、字号与动画。字体未设置时用内置字体，字号未设置时跟随系统。
fn interface(window: &mut Window, cx: &mut App) -> AnyElement {
    let system = fonts::system(cx).clone();
    let families = std::iter::once(Choice {
        value: None,
        label: BUNDLED_FONT.into(),
        detail: None,
    })
    .chain(
        fonts::families(cx)
            .into_iter()
            .filter(|f| f != BUNDLED_FONT)
            .map(|f| Choice {
                value: Some(f.clone()),
                label: f.into(),
                detail: None,
            }),
    )
    .collect();
    let (current, animated) = {
        let a = &settings(cx).appearance;
        (a.ui_font.clone(), a.animations)
    };
    let ui_font = form::select_described(
        "ui-font",
        current,
        families,
        |family: Option<String>, _, cx| {
            config::update(
                "appearance",
                "ui_font",
                family.as_deref().map(Value::from),
                |s| s.appearance.ui_font = family.clone(),
                cx,
            )
        },
    );
    let system_size = system.size.unwrap_or(16.);
    let size = bound_number(
        "ui_font_size",
        wide(limits::UI_FONT_SIZE),
        1.,
        move |cx| f64::from(settings(cx).appearance.ui_font_size.unwrap_or(system_size)),
        |v, cx| {
            config::update(
                "appearance",
                "ui_font_size",
                Some(v.into()),
                |s| s.appearance.ui_font_size = Some(v as f32),
                cx,
            )
        },
        window,
        cx,
    );
    let animations = switch("appearance", "animations", animated, |s, v| {
        s.appearance.animations = v
    });
    let system_language = crate::app::LANGUAGES
        .iter()
        .find(|(id, _)| *id == crate::app::system_locale())
        .map(|(_, name)| SharedString::from(*name));
    let languages = std::iter::once(Choice {
        value: None,
        label: label("follow_system"),
        detail: system_language,
    })
    .chain(crate::app::LANGUAGES.iter().map(|(id, name)| Choice {
        value: Some((*id).to_owned()),
        label: (*name).into(),
        detail: None,
    }))
    .collect();
    let language = form::select_described(
        "ui-language",
        settings(cx).appearance.language.clone(),
        languages,
        |language: Option<String>, _, cx| {
            config::update(
                "appearance",
                "language",
                language.as_deref().map(Value::from),
                |s| s.appearance.language = language.clone(),
                cx,
            )
        },
    );
    section(
        Some(label("interface")),
        [
            row(label("language"), language).into_any_element(),
            row(label("ui_font"), ui_font).into_any_element(),
            row(label("ui_font_size"), number(&size)).into_any_element(),
            row(label("animations"), animations).into_any_element(),
        ],
        cx,
    )
}

fn terminal(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let current = settings(cx).terminal.font_family.clone();
    let mut families: Vec<String> = fonts::monospace(cx).map(|f| f.to_vec()).unwrap_or_default();
    if !families.contains(&current) {
        families.insert(0, current.clone());
    }
    let families = families
        .into_iter()
        .map(|f| (f.clone(), f.into()))
        .collect();
    let shapes = [
        CursorStyle::Block,
        CursorStyle::Beam,
        CursorStyle::Underline,
    ]
    .map(|shape| (shape, label(&format!("shape_{}", spelling(&shape)))))
    .to_vec();

    let font_size = terminal_number(
        "font_size",
        wide(limits::FONT_SIZE),
        0.5,
        |t| t.font_size,
        |t, v| t.font_size = v,
        window,
        cx,
    );
    let line_height = terminal_number(
        "line_height",
        wide(limits::LINE_HEIGHT),
        0.05,
        |t| t.line_height,
        |t, v| t.line_height = v,
        window,
        cx,
    );
    let fallbacks = bound_input(
        "font_fallbacks",
        |cx| settings(cx).terminal.font_fallbacks.join(", ").into(),
        |value, cx| {
            let list: Vec<String> = value
                .split(',')
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect();
            let array = list.iter().map(String::as_str).collect();
            config::update(
                "terminal",
                "font_fallbacks",
                Some(Value::Array(array)),
                |s| s.terminal.font_fallbacks = list,
                cx,
            );
        },
        |input| input,
        window,
        cx,
    );
    let scrollback = bound_number(
        "scrollback",
        0.0..=limits::MAX_SCROLLBACK as f64,
        1000.,
        |cx| settings(cx).terminal.scrollback as f64,
        |v, cx| {
            let lines = v as usize;
            config::update(
                "terminal",
                "scrollback",
                Some((lines as i64).into()),
                |s| s.terminal.scrollback = lines,
                cx,
            );
        },
        window,
        cx,
    );
    let terminal = &settings(cx).terminal;
    let font = vec![
        row(
            label("font_family"),
            select("font-family", current, families, |family: String, _, cx| {
                config::update(
                    "terminal",
                    "font_family",
                    Some(family.as_str().into()),
                    |s| s.terminal.font_family = family,
                    cx,
                );
            }),
        )
        .into_any_element(),
        row(label("font_size"), number(&font_size)).into_any_element(),
        row(label("line_height"), number(&line_height)).into_any_element(),
        row(label("fallbacks"), text(&fallbacks)).into_any_element(),
    ];
    let cursor = vec![
        row(
            label("cursor_shape"),
            select(
                "cursor-shape",
                terminal.cursor_shape,
                shapes,
                |shape, _, cx| {
                    config::update(
                        "terminal",
                        "cursor_shape",
                        Some(spelling(&shape).into()),
                        |s| s.terminal.cursor_shape = shape,
                        cx,
                    );
                },
            ),
        )
        .into_any_element(),
        row(
            label("cursor_blink"),
            switch("terminal", "cursor_blink", terminal.cursor_blink, |s, v| {
                s.terminal.cursor_blink = v
            }),
        )
        .into_any_element(),
    ];
    let behavior = vec![
        row(label("scrollback"), number(&scrollback)).into_any_element(),
        row(
            label("copy_on_select"),
            switch(
                "terminal",
                "copy_on_select",
                terminal.copy_on_select,
                |s, v| s.terminal.copy_on_select = v,
            ),
        )
        .into_any_element(),
        row(
            label("confirm_multiline_paste"),
            switch(
                "terminal",
                "confirm_multiline_paste",
                terminal.confirm_multiline_paste,
                |s, v| s.terminal.confirm_multiline_paste = v,
            ),
        )
        .into_any_element(),
    ];
    vec![
        section(Some(label("font")), font, cx),
        section(Some(label("cursor")), cursor, cx),
        section(Some(label("behavior")), behavior, cx),
    ]
}

fn shell(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let program = bound_input(
        "shell_program",
        |cx| {
            settings(cx)
                .shell
                .program
                .clone()
                .unwrap_or_default()
                .into()
        },
        |value, cx| {
            let value = value.trim().to_owned();
            let program = (!value.is_empty()).then_some(value);
            config::update(
                "shell",
                "program",
                program.as_deref().map(Value::from),
                |s| s.shell.program = program.clone(),
                cx,
            );
        },
        |input| input.placeholder(label("system_default")),
        window,
        cx,
    );
    let args = bound_input(
        "shell_args",
        |cx| {
            let args = &settings(cx).shell.args;
            shlex::try_join(args.iter().map(String::as_str))
                .unwrap_or_else(|_| args.join(" "))
                .into()
        },
        |value, cx| {
            // 引号未闭合等输入到一半的状态不写入
            let Some(args) = shlex::split(&value) else {
                return;
            };
            let array = args.iter().map(String::as_str).collect();
            config::update(
                "shell",
                "args",
                Some(Value::Array(array)),
                |s| s.shell.args = args,
                cx,
            );
        },
        |input| input,
        window,
        cx,
    );
    let default_known_hosts = config::tilde(&config::default_known_hosts());
    let known_hosts = path_input(
        ("ssh", "known_hosts"),
        |s| s.ssh.known_hosts.as_deref(),
        |s, p| s.ssh.known_hosts = p,
        Some(&default_known_hosts),
        window,
        cx,
    );
    let restore = switch(
        "shell",
        "restore_session",
        settings(cx).shell.restore_session,
        |s, v| s.shell.restore_session = v,
    );
    let ssh = &settings(cx).ssh;
    let auto_reconnect = switch("ssh", "auto_reconnect", ssh.auto_reconnect, |s, v| {
        s.ssh.auto_reconnect = v
    });
    let show_latency = switch("ssh", "show_latency", ssh.show_latency, |s, v| {
        s.ssh.show_latency = v
    });
    let keep_host_title = switch("ssh", "keep_host_title", ssh.keep_host_title, |s, v| {
        s.ssh.keep_host_title = v
    });
    vec![
        section(
            None,
            [
                row(label("program"), text(&program)).into_any_element(),
                row(label("args"), text(&args)).into_any_element(),
                row(label("restore_session"), restore).into_any_element(),
            ],
            cx,
        ),
        section(
            Some("SSH".into()),
            [
                row(label("known_hosts"), text(&known_hosts)).into_any_element(),
                row(label("auto_reconnect"), auto_reconnect).into_any_element(),
                row(label("show_latency"), show_latency).into_any_element(),
                row(label("keep_host_title"), keep_host_title).into_any_element(),
            ],
            cx,
        ),
    ]
}

fn files(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let default_dir = FileSettings::default().download_dir();
    let download_dir = path_input(
        ("files", "download_dir"),
        |s| s.files.download_dir.as_deref(),
        |s, p| s.files.download_dir = p,
        Some(&config::tilde(&default_dir)),
        window,
        cx,
    );
    let browse = form::icon_button("browse-download-dir", IconName::FolderOpen, label("browse"))
        .on_click(|_, _, cx| {
            let picked = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: None,
            });
            cx.spawn(async move |cx| {
                if let Ok(Ok(Some(paths))) = picked.await
                    && let Some(dir) = paths.into_iter().next()
                {
                    cx.update(|cx| {
                        set_path(
                            ("files", "download_dir"),
                            Some(dir),
                            |s, p| s.files.download_dir = p,
                            cx,
                        )
                    });
                }
            })
            .detach();
        });
    let show_hidden = switch(
        "files",
        "show_hidden",
        settings(cx).files.show_hidden,
        |s, v| s.files.show_hidden = v,
    );
    vec![section(
        None,
        [
            row(
                label("download_dir"),
                h_flex().gap_1().child(text(&download_dir)).child(browse),
            )
            .into_any_element(),
            row(t!("files.show_hidden").to_string(), show_hidden).into_any_element(),
        ],
        cx,
    )]
}

fn about(cx: &mut App) -> Vec<AnyElement> {
    let muted = cx.theme().muted_foreground;
    let header = v_flex()
        .items_center()
        .gap_1()
        .py_4()
        .child(img(crate::assets::app_logo()).size(ABOUT_LOGO))
        .child(div().mt_2().text_lg().font_semibold().child("SCell"))
        .child(
            div()
                .text_sm()
                .text_color(muted)
                .child(env!("CARGO_PKG_VERSION")),
        )
        .child(update_button(cx))
        .into_any_element();
    let repo = env!("CARGO_PKG_REPOSITORY");
    let link = |id: &'static str, text: SharedString, url: String| {
        Button::new(id)
            .label(text)
            .ghost()
            .small()
            .on_click(move |_, _, cx| cx.open_url(&url))
    };
    let project = [
        row(
            label("homepage"),
            link(
                "about-homepage",
                repo.trim_start_matches("https://").to_owned().into(),
                repo.to_owned(),
            ),
        )
        .into_any_element(),
        row(
            label("license"),
            link(
                "about-license",
                env!("CARGO_PKG_LICENSE").into(),
                format!("{repo}/blob/main/LICENSE"),
            ),
        )
        .into_any_element(),
        row(
            label("third_party"),
            Button::new("about-third-party")
                .label(t!("settings.components", count = super::licenses::count()).to_string())
                .ghost()
                .small()
                .on_click(|_, window, cx| super::licenses::open(window, cx)),
        )
        .into_any_element(),
    ];
    let mut page = vec![header, section(None, project, cx)];
    if let Some(paths) = cx.global::<ConfigState>().paths.clone() {
        let rows = [
            ("config_dir", paths.root().to_owned()),
            ("logs_dir", paths.logs_dir()),
        ]
        .map(|(key, dir)| row(label(key), folder(key, dir, muted)).into_any_element());
        page.push(section(None, rows, cx));
    }
    page
}

/// 一个按钮走完检查、下载、重启安装；不能在程序内安装时改为打开 Release 页面。
fn update_button(cx: &App) -> Button {
    let state = updates::state(cx);
    let text: SharedString = match &state {
        UpdateState::Idle => label("check_updates"),
        UpdateState::Checking => label("checking_updates"),
        UpdateState::UpToDate => label("up_to_date"),
        UpdateState::Available(update) => {
            let key = if update.in_app() {
                "settings.update_to"
            } else {
                "settings.download_version"
            };
            t!(key, version = update.version).to_string().into()
        }
        UpdateState::Downloading(percent) => t!("settings.downloading_update", percent = percent)
            .to_string()
            .into(),
        UpdateState::Ready(_) => label("restart_to_update"),
    };
    Button::new("about-update")
        .label(text)
        .ghost()
        .small()
        .loading(matches!(
            state,
            UpdateState::Checking | UpdateState::Downloading(_)
        ))
        .on_click(|_, _, cx| updates::advance(cx))
}

/// 目录路径与“在文件管理器中打开”按钮；目录还不存在时先创建。
fn folder(key: &'static str, dir: PathBuf, muted: gpui_kit::Hsla) -> impl IntoElement {
    h_flex()
        .gap_1()
        .min_w_0()
        .child({
            let full: SharedString = dir.display().to_string().into();
            div()
                .id(SharedString::from(format!("folder-path-{key}")))
                .min_w_0()
                .max_w(CONTROL_WIDTH)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .text_color(muted)
                .child(config::tilde(&dir))
                // 路径常被截断，悬停看完整路径
                .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
        })
        .child(
            form::icon_button(key, IconName::FolderOpen, label("open_folder")).on_click(
                move |_, _, cx| match std::fs::create_dir_all(&dir) {
                    Ok(()) => cx.open_with_system(&dir),
                    Err(e) => config::report_errors(vec![format!("{}: {e}", dir.display())], cx),
                },
            ),
        )
}

/// 写入可选路径项；空值删除该键，回到默认。
fn set_path(
    (table, key): (&'static str, &'static str),
    path: Option<PathBuf>,
    set: fn(&mut Config, Option<PathBuf>),
    cx: &mut App,
) {
    let value = path.as_ref().map(|p| Value::from(p.display().to_string()));
    config::update(table, key, value, |s| set(s, path), cx);
}

/// 可选路径的输入框，`placeholder` 为未设置时实际使用的路径。
fn path_input(
    item: (&'static str, &'static str),
    get: fn(&Config) -> Option<&Path>,
    set: fn(&mut Config, Option<PathBuf>),
    placeholder: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let placeholder = placeholder.map(str::to_owned);
    bound_input(
        format!("{}_{}", item.0, item.1),
        move |cx| {
            get(settings(cx))
                .map(|p| p.display().to_string())
                .unwrap_or_default()
                .into()
        },
        move |value, cx| {
            let value = value.trim();
            set_path(item, (!value.is_empty()).then(|| value.into()), set, cx);
        },
        move |input| match placeholder {
            Some(p) => input.placeholder(p),
            None => input,
        },
        window,
        cx,
    )
}

fn wide(range: std::ops::RangeInclusive<f32>) -> std::ops::RangeInclusive<f64> {
    f64::from(*range.start())..=f64::from(*range.end())
}

/// `[terminal]` 下的浮点项。
fn terminal_number(
    key: &'static str,
    range: std::ops::RangeInclusive<f64>,
    step: f64,
    get: fn(&TerminalSettings) -> f32,
    set: fn(&mut TerminalSettings, f32),
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    bound_number(
        key,
        range,
        step,
        move |cx| f64::from(get(&settings(cx).terminal)),
        move |v, cx| {
            config::update(
                "terminal",
                key,
                Some(v.into()),
                |s| set(&mut s.terminal, v as f32),
                cx,
            )
        },
        window,
        cx,
    )
}

/// `[table]` 下的开关项。
fn switch(
    table: &'static str,
    key: &'static str,
    checked: bool,
    set: fn(&mut Config, bool),
) -> Switch {
    Switch::new(key)
        .checked(checked)
        .on_click(move |value: &bool, _, cx: &mut App| {
            let value = *value;
            config::update(table, key, Some(value.into()), |s| set(s, value), cx)
        })
}

fn number(input: &Entity<InputState>) -> impl IntoElement {
    NumberInput::new(input).small().w(NUMBER_WIDTH).max_w_full()
}

fn text(input: &Entity<InputState>) -> impl IntoElement {
    Input::new(input).small().w(CONTROL_WIDTH).max_w_full()
}
