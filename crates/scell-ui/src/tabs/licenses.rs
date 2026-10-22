//! 关于页的“第三方许可证”：列出程序链接的全部开源依赖。清单由 `cargo xtask licenses` 生成，许可证全文随安装包分发。

use std::sync::OnceLock;

use gpui_kit::component::{ActiveTheme, WindowExt, h_flex};
use gpui_kit::{App, ParentElement, SharedString, Styled, Window, div, px, uniform_list};
use rust_i18n::t;

const LIST: &str = include_str!("../../assets/licenses.tsv");

struct Dependency {
    name: SharedString,
    version: SharedString,
    license: SharedString,
}

fn dependencies() -> &'static [Dependency] {
    static PARSED: OnceLock<Vec<Dependency>> = OnceLock::new();
    PARSED.get_or_init(|| {
        LIST.lines()
            .filter_map(|line| {
                let mut f = line.split('\t');
                Some(Dependency {
                    name: f.next()?.to_owned().into(),
                    version: f.next()?.to_owned().into(),
                    license: f.next()?.to_owned().into(),
                })
            })
            .collect()
    })
}

pub(crate) fn count() -> usize {
    dependencies().len()
}

pub(crate) fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        dialog
            .bg(crate::appearance::Appearance::surface(cx))
            .title(t!("settings.third_party").to_string())
            .w(px(560.))
            .child(
                uniform_list("third-party-licenses", count(), move |range, _, _| {
                    dependencies()[range]
                        .iter()
                        .map(|d| {
                            h_flex()
                                .w_full()
                                .h(crate::ui::metrics::FILE_ROW_HEIGHT)
                                .gap_3()
                                .text_sm()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_x_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(d.name.clone()),
                                )
                                .child(div().w(px(90.)).text_color(muted).child(d.version.clone()))
                                .child(
                                    div()
                                        .w(px(200.))
                                        .overflow_x_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_color(muted)
                                        .child(d.license.clone()),
                                )
                        })
                        .collect()
                })
                .h(px(420.)),
            )
    });
}
