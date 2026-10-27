//! 字体：内置字体、桌面环境的界面字体，以及供设置页选择的字体列表（首次用到时在后台探测，缓存到进程结束）。

use std::borrow::Cow;
use std::sync::Arc;
use std::thread::JoinHandle;

use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, FontStyle, FontWeight, Global, TextSystem, font, px};

#[derive(Default)]
struct MonospaceFonts {
    list: Option<Arc<[String]>>,
    loading: bool,
}

impl Global for MonospaceFonts {}

/// 桌面环境设置的界面字体，启动时探测一次。
#[derive(Debug, Clone, Default)]
pub(crate) struct SystemFont {
    pub(crate) family: Option<String>,
    /// 逻辑像素。
    pub(crate) size: Option<f32>,
}

impl Global for SystemFont {}

/// 内置字体 SCell Mono：正体与斜体两个可变字体的 TTC，zstd 压缩。由 `cargo xtask fonts` 生成。
const BUNDLED: &[u8] = include_bytes!("../assets/fonts/SCellMono.ttc.zst");

/// 正在后台解压的内置字体。
pub struct BundledFont(JoinHandle<std::io::Result<Vec<u8>>>);

impl BundledFont {
    /// 在后台线程解压内置字体（约 35 MB），与启动时的其他工作并行；[`crate::init`] 时取回并注册。
    pub fn load() -> Self {
        Self(std::thread::spawn(|| {
            // 生成时写入了原始大小
            let size = zstd::zstd_safe::get_frame_content_size(BUNDLED)
                .ok()
                .flatten()
                .unwrap_or_default();
            zstd::bulk::decompress(BUNDLED, usize::try_from(size).unwrap_or_default())
        }))
    }
}

/// 注册内置字体，再探测并记录系统界面字体。系统字体只采用本机确实装了的，否则留给 GPUI 的默认回退。
pub(crate) fn init(cx: &mut App, bundled: BundledFont) {
    let started = std::time::Instant::now();
    match bundled.0.join() {
        Ok(Ok(data)) => {
            if let Err(err) = cx.text_system().add_fonts(vec![Cow::Owned(data)]) {
                tracing::error!(%err, "无法注册内置字体");
            }
        }
        Ok(Err(err)) => tracing::error!(%err, "无法解压内置字体"),
        Err(_) => tracing::error!("解压内置字体的线程崩溃"),
    }
    tracing::debug!(waited = ?started.elapsed(), "内置字体已注册");

    let mut font = platform::system_font();
    if let Some(family) = &font.family {
        let installed = cx.text_system().all_font_names();
        if !installed.iter().any(|f| f == family) {
            tracing::info!(family, "系统界面字体未安装，使用默认字体");
            font.family = None;
        }
    }
    tracing::debug!(?font, "系统界面字体");
    cx.set_global(font);
}

/// 在后台预先加载界面与终端主字体。首帧排版原本要在主线程上同步读取并解析它们（每个数十毫秒），
/// 放在窗口创建（GPU 初始化）期间并行完成；没赶上时首帧照常按需加载。
pub(crate) fn prewarm(cx: &App) {
    let terminal = crate::appearance::Appearance::terminal_style(cx);
    let mut fonts = vec![font(cx.theme().font_family.clone())];
    for (weight, style) in [
        (FontWeight::NORMAL, FontStyle::Normal),
        (FontWeight::BOLD, FontStyle::Normal),
        (FontWeight::NORMAL, FontStyle::Italic),
    ] {
        let mut f = font(terminal.font_family.clone());
        f.weight = weight;
        f.style = style;
        fonts.push(f);
    }
    spawn_prewarm(fonts, cx);
}

/// 终端回退字体（中日韩、Emoji）只在首次遇到对应字符时才用到。在首个窗口画出后再加载：
/// 加载期间占着文本系统，抢在首帧之前会拖慢首帧。
pub(crate) fn prewarm_fallbacks(cx: &App) {
    let terminal = crate::appearance::Appearance::terminal_style(cx);
    let fonts = terminal.fallbacks.iter().map(|f| font(f.clone())).collect();
    spawn_prewarm(fonts, cx);
}

fn spawn_prewarm(fonts: Vec<gpui_kit::Font>, cx: &App) {
    let text_system = cx.text_system().clone();
    cx.background_executor()
        .spawn(async move {
            let started = std::time::Instant::now();
            text_system.prewarm_fonts(&fonts);
            tracing::debug!(count = fonts.len(), elapsed = ?started.elapsed(), "字体预热完成");
        })
        .detach();
}

pub(crate) fn system(cx: &App) -> &SystemFont {
    cx.global::<SystemFont>()
}

/// 全部已安装的字体族，按名称排序。
pub(crate) fn families(cx: &App) -> Vec<String> {
    let mut all = cx.text_system().all_font_names();
    all.retain(|f| !f.starts_with('.'));
    all.sort_by_key(|f| f.to_lowercase());
    all.dedup();
    all
}

/// 已探测到的等宽字体；尚未探测完时返回 `None` 并在后台开始探测，完成后刷新窗口。
pub(crate) fn monospace(cx: &mut App) -> Option<Arc<[String]>> {
    let state = cx.default_global::<MonospaceFonts>();
    if state.list.is_some() || state.loading {
        return state.list.clone();
    }
    state.loading = true;
    let text_system = cx.text_system().clone();
    let task = cx
        .background_executor()
        .spawn(async move { detect(&text_system) });
    cx.spawn(async move |cx| {
        let list = task.await;
        cx.update(|cx| {
            let state = cx.global_mut::<MonospaceFonts>();
            state.list = Some(list.into());
            state.loading = false;
            cx.refresh_windows();
        });
    })
    .detach();
    None
}

fn detect(text_system: &TextSystem) -> Vec<String> {
    let started = std::time::Instant::now();
    let fonts: Vec<String> = text_system
        .all_font_names()
        .into_iter()
        .filter(|family| is_monospace(text_system, family))
        .collect();
    tracing::debug!(count = fonts.len(), elapsed = ?started.elapsed(), "等宽字体探测完成");
    fonts
}

/// 窄字符与宽字符的步进相同即视为等宽。缺字（如 Emoji 字体）或解析时回退到其他字体的不算。
fn is_monospace(text_system: &TextSystem, family: &str) -> bool {
    let id = text_system.resolve_font(&font(family.to_owned()));
    if text_system
        .get_font_for_id(id)
        .is_none_or(|f| f.family.as_ref() != family)
    {
        return false;
    }
    let size = px(16.);
    let advance = |ch| text_system.advance(id, size, ch).map(|s| s.width).ok();
    match (advance('i'), advance('W'), advance('m')) {
        (Some(i), Some(w), Some(m)) => {
            i > px(0.) && (i - w).abs() < px(0.01) && (i - m).abs() < px(0.01)
        }
        _ => false,
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::process::Command;

    use super::SystemFont;

    /// 桌面设置里的字号是点（pt），换算成逻辑像素（96 DPI 下 1pt = 4/3 px）。
    fn points_to_px(pt: f32) -> f32 {
        (pt * 4. / 3.).round()
    }

    /// 依次尝试 KDE、GNOME 的设置与 fontconfig 的默认无衬线字体。
    pub(super) fn system_font() -> SystemFont {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let kde = desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE"));
        let found = if kde { kde_font() } else { gnome_font() };
        found.or_else(fontconfig_font).unwrap_or_default()
    }

    /// `~/.config/kdeglobals` 的 `[General] font=族名,点数,…`。
    fn kde_font() -> Option<SystemFont> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config"))
            })?;
        let text = std::fs::read_to_string(config.join("kdeglobals")).ok()?;
        parse_kde(&text)
    }

    pub(super) fn parse_kde(text: &str) -> Option<SystemFont> {
        let mut in_general = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_general = line == "[General]";
            } else if in_general && let Some(value) = line.strip_prefix("font=") {
                let mut parts = value.split(',');
                let family = parts.next()?.trim().to_owned();
                let size = parts.next().and_then(|p| p.trim().parse::<f32>().ok());
                return Some(SystemFont {
                    family: (!family.is_empty()).then_some(family),
                    size: size.filter(|s| *s > 0.).map(points_to_px),
                });
            }
        }
        None
    }

    /// `gsettings get org.gnome.desktop.interface font-name`，形如 `'Cantarell 11'`。
    fn gnome_font() -> Option<SystemFont> {
        let out = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "font-name"])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        parse_gnome(&String::from_utf8_lossy(&out.stdout))
    }

    pub(super) fn parse_gnome(value: &str) -> Option<SystemFont> {
        let value = value.trim().trim_matches('\'');
        let (family, size) = match value.rsplit_once(' ') {
            Some((family, size)) if size.parse::<f32>().is_ok() => (family, size.parse().ok()),
            _ => (value, None),
        };
        (!family.is_empty()).then(|| SystemFont {
            family: Some(family.to_owned()),
            size: size.map(points_to_px),
        })
    }

    fn fontconfig_font() -> Option<SystemFont> {
        let out = Command::new("fc-match")
            .args(["-f", "%{family[0]}", "sans-serif"])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let family = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!family.is_empty()).then_some(SystemFont {
            family: Some(family),
            size: None,
        })
    }
}

/// Windows 与 macOS 上 GPUI 的 `.SystemUIFont` 已对应系统字体。
#[cfg(not(all(unix, not(target_os = "macos"))))]
mod platform {
    use super::SystemFont;

    pub(super) fn system_font() -> SystemFont {
        SystemFont::default()
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::platform::{parse_gnome, parse_kde};

    #[test]
    fn parses_desktop_font_settings() {
        let kde = "[Colors]\nfont=x\n[General]\nfont=Maple Mono NF CN,12,-1,5,300,0\n";
        let f = parse_kde(kde).unwrap();
        assert_eq!(f.family.as_deref(), Some("Maple Mono NF CN"));
        assert_eq!(f.size, Some(16.));
        let g = parse_gnome("'Noto Sans CJK SC 11'\n").unwrap();
        assert_eq!(g.family.as_deref(), Some("Noto Sans CJK SC"));
        assert_eq!(g.size, Some(15.));
        assert_eq!(parse_gnome("'Cantarell'").unwrap().size, None);
    }
}
