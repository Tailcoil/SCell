//! 外观：主题目录、明暗跟随、窗口材质，以及由主题推导出的终端样式。
//!
//! 布局分两层表面：外框（标题栏、侧栏）由窗口根节点的 `background` 一次绘制，
//! 终端区域是叠在外框上的圆角卡片。两层之间不画分隔线，靠色差区分。
//! 毛玻璃下每个像素只叠这两层，所以外框的组件背景都要设为透明，否则透明度会层层累加。

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme, Theme, ThemeConfig, ThemeSet};
use gpui_kit::{App, Global, Hsla, Rgba, WindowBackgroundAppearance, px, transparent_black};
use scell_config::settings::{BUNDLED_FONT, CursorStyle, TerminalSettings, WindowMaterial, limits};
use scell_config::theme::{HexColor, TerminalColors, parse_theme_set};
use scell_term::{CursorShape, Palette, Rgb};
use scell_term_view::TerminalStyle;

use crate::app::AppState;
use crate::config::ConfigState;

const BUILTIN_THEMES: &[&str] = &[
    include_str!("../assets/themes/scell.json"),
    include_str!("../assets/themes/catppuccin.json"),
];
const FALLBACK_DARK: &str = "SCell Dark";
const FALLBACK_LIGHT: &str = "SCell Light";

const MIN_FONT_SIZE: f32 = *limits::FONT_SIZE.start();
const MAX_FONT_SIZE: f32 = *limits::FONT_SIZE.end();

/// 毛玻璃下终端卡片自身的不透明度，叠在外框之上。
const GLASS_CARD_ALPHA: f32 = 0.45;

#[derive(Clone)]
struct ThemeEntry {
    ui: Rc<ThemeConfig>,
    terminal: Option<TerminalColors>,
}

pub(crate) struct Appearance {
    themes: HashMap<String, ThemeEntry>,
    system_dark: bool,
    terminal: TerminalStyle,
    card: Hsla,
    /// 浮层（Sheet、对话框）的底色，始终不透明：窗口内无法模糊其下的内容，半透明会让底下的文字透出来。
    surface: Hsla,
    material: WindowMaterial,
    /// 当前生效的主题名。
    current: String,
    /// 快捷键缩放叠加在设置字号上的增量，只在本次运行有效。
    zoom: f32,
}

impl Global for Appearance {}

impl Appearance {
    pub(crate) fn terminal_style(cx: &App) -> TerminalStyle {
        cx.global::<Self>().terminal.clone()
    }

    /// 终端卡片的背景色（已含材质对应的透明度）。
    pub(crate) fn card(cx: &App) -> Hsla {
        cx.global::<Self>().card
    }

    pub(crate) fn surface(cx: &App) -> Hsla {
        cx.global::<Self>().surface
    }

    pub(crate) fn window_background(cx: &App) -> WindowBackgroundAppearance {
        background_appearance(cx.global::<Self>().material, cx)
    }

    /// 全部主题名，按名称排序；以及当前生效的主题名。
    pub(crate) fn themes(cx: &App) -> (Vec<String>, String) {
        let state = cx.global::<Self>();
        let mut names: Vec<String> = state.themes.keys().cloned().collect();
        names.sort();
        (names, state.current.clone())
    }

    pub(crate) fn material(cx: &App) -> WindowMaterial {
        cx.global::<Self>().material
    }
}

pub(crate) fn init(cx: &mut App) {
    let mut errors = Vec::new();
    let themes = load_themes(cx, &mut errors);
    let system_dark = is_dark(cx.window_appearance());
    // 占位，随后的 apply 按主题换上配色
    let terminal = &ConfigState::settings(cx).terminal;
    let terminal = terminal_style(terminal, terminal.font_size, Palette::default(), cx.theme());
    cx.set_global(Appearance {
        themes,
        system_dark,
        terminal,
        card: transparent_black(),
        surface: transparent_black(),
        material: WindowMaterial::Opaque,
        current: String::new(),
        zoom: 0.,
    });
    apply(cx);
    crate::config::report_errors(errors, cx);
}

/// 重新读取用户主题目录，返回解析错误。
pub(crate) fn reload_themes(cx: &mut App) -> Vec<String> {
    let mut errors = Vec::new();
    let themes = load_themes(cx, &mut errors);
    cx.global_mut::<Appearance>().themes = themes;
    errors
}

/// 系统明暗变化时由窗口回调。
pub(crate) fn set_system_dark(dark: bool, cx: &mut App) {
    if cx.global::<Appearance>().system_dark != dark {
        cx.global_mut::<Appearance>().system_dark = dark;
        apply(cx);
    }
}

pub(crate) fn is_dark(appearance: gpui_kit::WindowAppearance) -> bool {
    matches!(
        appearance,
        gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark
    )
}

/// 调整字号缩放；`None` 还原为设置中的字号。
pub(crate) fn zoom(delta: Option<f32>, cx: &mut App) {
    let base = ConfigState::settings(cx).terminal.font_size;
    let state = cx.global_mut::<Appearance>();
    state.zoom = match delta {
        Some(d) => (base + state.zoom + d).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE) - base,
        None => 0.,
    };
    apply(cx);
}

/// 按当前设置与系统明暗应用主题、窗口材质与终端样式。
pub(crate) fn apply(cx: &mut App) {
    let settings = ConfigState::settings(cx).clone();
    if crate::app::apply_locale(settings.appearance.language.as_deref()) {
        cx.refresh_windows();
    }
    let appearance = cx.global::<Appearance>();
    let dark = appearance.system_dark;
    let name = settings.appearance.theme.resolve(dark);
    let fallback = if dark { FALLBACK_DARK } else { FALLBACK_LIGHT };
    let entry = appearance.themes.get(name).or_else(|| {
        tracing::warn!(theme = name, "主题不存在，使用内置主题");
        appearance.themes.get(fallback)
    });
    let Some(entry) = entry.cloned() else {
        return;
    };
    tracing::debug!(theme = %entry.ui.name, material = ?settings.appearance.window, "应用外观");

    // 分两次更新：先装主题，再在其上调整。合在一次里时，切换明暗会重新加载主题覆盖掉调整
    Theme::update(cx, |theme| theme.apply_config(&entry.ui));

    let theme = cx.theme();
    let palette = terminal_palette(&entry, theme);
    let chrome = theme.sidebar;
    let card = hsla(palette.background);
    let material = settings.appearance.window;
    let (chrome_alpha, card_alpha) = match material {
        WindowMaterial::Opaque => (1., 1.),
        _ => (settings.appearance.opacity, GLASS_CARD_ALPHA),
    };

    // 界面字体：设置优先，其次内置字体；字号设置优先，其次系统。字号决定 rem，界面按 rem 计的间距随之缩放
    let ui_font = settings
        .appearance
        .ui_font
        .clone()
        .unwrap_or_else(|| BUNDLED_FONT.into());
    let ui_size = settings
        .appearance
        .ui_font_size
        .or(crate::fonts::system(cx).size);
    Theme::update(cx, |theme| {
        theme.font_family = ui_font.into();
        if let Some(size) = ui_size {
            theme.font_size = px(size);
        }
        theme.mono_font_family = settings.terminal.font_family.clone().into();
        theme.background = chrome.opacity(chrome_alpha);
        theme.sidebar = transparent_black();
        theme.sidebar_border = transparent_black();
        theme.title_bar = transparent_black();
        theme.title_bar_border = transparent_black();
        // 侧弹窗默认让出一条标题栏的高度；这里没有横贯窗口的标题栏，改由弹窗自己留边距
        theme.sheet.margin_top = px(0.);
    });

    let font_size = (settings.terminal.font_size + cx.global::<Appearance>().zoom)
        .clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    let terminal = terminal_style(&settings.terminal, font_size, palette, cx.theme());
    let state = cx.global_mut::<Appearance>();
    let material_changed = state.material != material;
    state.terminal = terminal.clone();
    state.card = card.opacity(card_alpha);
    state.surface = chrome.opacity(1.);
    state.material = material;
    state.current = entry.ui.name.to_string();

    if cx.has_global::<AppState>() {
        let copy_on_select = settings.terminal.copy_on_select;
        let paste_guard = crate::overlays::paste::guard(cx);
        for view in cx.global::<AppState>().views() {
            view.update(cx, |view, cx| {
                view.set_copy_on_select(copy_on_select);
                view.set_paste_guard(paste_guard.clone());
                view.set_style(terminal.clone(), cx);
            });
        }
        let sessions = &cx.global::<AppState>().sessions;
        sessions.set_scrollback(settings.terminal.scrollback);
        sessions.set_auto_reconnect(settings.ssh.auto_reconnect);
    }
    let background = background_appearance(material, cx);
    for window in cx.windows().into_iter().filter(|_| material_changed) {
        let _ = window.update(cx, |_, window, _| {
            window.set_background_appearance(background)
        });
    }
    cx.refresh_windows();
}

fn load_themes(cx: &App, errors: &mut Vec<String>) -> HashMap<String, ThemeEntry> {
    let mut themes = HashMap::new();
    for json in BUILTIN_THEMES {
        if let Err(e) = add_theme_set(&mut themes, json) {
            // 内置主题随二进制发布，解析失败属于构建错误
            tracing::error!(%e, "内置主题无效");
        }
    }
    let Some(dir) = ConfigState::themes_dir(cx) else {
        return themes;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return themes;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        // 用户主题与内置同名时覆盖内置
        let result = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|json| add_theme_set(&mut themes, &json));
        if let Err(e) = result {
            errors.push(format!("{}: {e}", path.display()));
        }
    }
    themes
}

fn add_theme_set(themes: &mut HashMap<String, ThemeEntry>, json: &str) -> Result<(), String> {
    let set: ThemeSet = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let terminals = parse_theme_set(json)?;
    for (ui, term) in set.themes.into_iter().zip(terminals) {
        themes.insert(
            ui.name.to_string(),
            ThemeEntry {
                ui: Rc::new(ui),
                terminal: term.terminal,
            },
        );
    }
    Ok(())
}

/// 主题未给出的终端颜色：前景、背景、光标取 UI 配色，ANSI 色取内置调色板。
fn terminal_palette(entry: &ThemeEntry, theme: &Theme) -> Palette {
    let base = if theme.is_dark() {
        Palette::dark()
    } else {
        Palette::light()
    };
    let t = entry.terminal.clone().unwrap_or_default();
    let mut ansi = base.ansi;
    for (slot, color) in ansi.iter_mut().zip(t.ansi()) {
        if let Some(c) = color {
            *slot = rgb(c);
        }
    }
    let foreground = t.foreground.map_or_else(|| ui_rgb(theme.foreground), rgb);
    Palette {
        ansi,
        foreground,
        background: t.background.map_or_else(|| ui_rgb(theme.background), rgb),
        cursor: t.cursor.map_or(foreground, rgb),
        selection: t.selection.map_or(base.selection, rgb),
    }
}

fn terminal_style(
    settings: &TerminalSettings,
    font_size: f32,
    palette: Palette,
    theme: &Theme,
) -> TerminalStyle {
    TerminalStyle {
        font_family: settings.font_family.clone().into(),
        fallbacks: settings.font_fallbacks.clone(),
        font_size: px(font_size),
        line_height: settings.line_height,
        palette,
        // 背景由卡片绘制
        background_alpha: 0.,
        match_background: theme.warning.opacity(0.3),
        focused_match_background: theme.warning.opacity(0.75),
        cursor_shape: match settings.cursor_shape {
            CursorStyle::Block => CursorShape::Block,
            CursorStyle::Beam => CursorShape::Beam,
            CursorStyle::Underline => CursorShape::Underline,
        },
        cursor_blink: settings.cursor_blink,
    }
}

fn background_appearance(material: WindowMaterial, cx: &App) -> WindowBackgroundAppearance {
    match material {
        WindowMaterial::Opaque => WindowBackgroundAppearance::Opaque,
        WindowMaterial::Translucent => WindowBackgroundAppearance::Transparent,
        // Wayland 上的模糊由 crate::blur 按窗口框与圆角申请，GPUI 只需透明
        WindowMaterial::Blur if cx.compositor_name() == "Wayland" => {
            WindowBackgroundAppearance::Transparent
        }
        // Windows 10/11 都支持背景模糊（Mica 在 Windows 10 上无效）
        WindowMaterial::Blur => WindowBackgroundAppearance::Blurred,
    }
}

fn rgb(c: HexColor) -> Rgb {
    let [r, g, b] = c.0;
    Rgb::new(r, g, b)
}

fn ui_rgb(c: Hsla) -> Rgb {
    let c = Rgba::from(c);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb::new(byte(c.r), byte(c.g), byte(c.b))
}

fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}
