//! 多行粘贴确认：显示行数与开头几行，确认后才发给终端。

use std::rc::Rc;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme, WindowExt, v_flex};
use gpui_kit::{App, ParentElement, Styled, div};
use rust_i18n::t;
use scell_term_view::PasteGuard;

/// 预览的行数与每行字符数，更多的部分省略。
const PREVIEW_LINES: usize = 6;
const PREVIEW_CHARS: usize = 120;

/// 按设置决定终端视图是否需要粘贴确认。
pub(crate) fn guard(cx: &App) -> Option<PasteGuard> {
    crate::config::ConfigState::settings(cx)
        .terminal
        .confirm_multiline_paste
        .then(|| Rc::new(confirm) as PasteGuard)
}

fn confirm(
    text: String,
    view: gpui_kit::WeakEntity<scell_term_view::TerminalView>,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let lines: Vec<&str> = text.lines().collect();
    let count = lines.len();
    let mut preview: Vec<String> = lines
        .iter()
        .take(PREVIEW_LINES)
        .map(|l| match l.char_indices().nth(PREVIEW_CHARS) {
            Some((i, _)) => format!("{}…", &l[..i]),
            None => (*l).to_owned(),
        })
        .collect();
    if count > PREVIEW_LINES {
        preview.push("…".into());
    }
    let preview = preview.join("\n");
    let text = Rc::new(text);
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let text = text.clone();
        let view = view.clone();
        let theme = cx.theme();
        alert
            // 主题背景在毛玻璃材质下是半透明的，对话框要用不透明的表面色
            .bg(crate::appearance::Appearance::surface(cx))
            .title(t!("paste.title", count = count).to_string())
            .description(
                v_flex().w_full().child(
                    div()
                        .w_full()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .whitespace_normal()
                        .child(preview.clone()),
                ),
            )
            .ok_text(t!("paste.confirm").to_string())
            .ok_variant(ButtonVariant::Primary)
            .cancel_text(t!("transfers.cancel").to_string())
            .show_cancel(true)
            .on_ok(move |_, _, cx| {
                let _ = view.update(cx, |view, cx| view.paste(&text, cx));
                true
            })
    });
}
