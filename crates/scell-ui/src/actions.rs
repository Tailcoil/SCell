use std::collections::BTreeMap;
use std::rc::Rc;

use gpui_kit::{
    Action, App, DummyKeyboardMapper, Global, InvalidKeystrokeError, KeyBinding,
    KeyBindingContextPredicate, KeyBindingMetaIndex, Keystroke, Unbind, actions,
};
use rust_i18n::t;
use scell_config::settings::KeyList;
use scell_term_view::{Copy, NextPrompt, Paste, PreviousPrompt, ScrollPageDown, ScrollPageUp};

actions!(
    workspace,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleSidebar,
        ToggleContextPanel,
        ToggleTransfers,
        Find,
        RestartTab,
        SplitRight,
        SplitDown,
        ClosePane,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        ToggleZoom,
        TogglePalette,
        QuickConnect,
        OpenSettings,
        NewWindow,
        CloseWindow,
        MoveToNewWindow,
        RenameTab,
        OpenSftp,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize
    ]
);

/// 切换到第 N 个标签（从 0 计）；超出时切到最后一个，与浏览器的 Ctrl+9 一致。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct ActivateTab(pub(crate) usize);

pub(crate) const WORKSPACE_CONTEXT: &str = "Workspace";
const WS: &str = WORKSPACE_CONTEXT;
const TERMINAL: &str = "Terminal";
/// 只在终端主屏生效，备用屏里的程序（vim、less 等）照常收到这些按键。
const PRIMARY: &str = "Terminal && !alt_screen";

/// 可在 `settings.toml` 的 `[keys]` 里改键的命令。
struct Command {
    /// 配置里用的名字。
    name: &'static str,
    action: fn() -> Box<dyn Action>,
    context: &'static str,
    keys: &'static [&'static str],
}

const fn command(
    name: &'static str,
    action: fn() -> Box<dyn Action>,
    context: &'static str,
    keys: &'static [&'static str],
) -> Command {
    Command {
        name,
        action,
        context,
        keys,
    }
}

fn new<A: Action + Default>() -> Box<dyn Action> {
    Box::new(A::default())
}

fn tab<const N: usize>() -> Box<dyn Action> {
    Box::new(ActivateTab(N))
}

/// 默认按键统一使用 Ctrl+Shift 或 Ctrl+Alt 组合，避免占用 shell、tmux、vim 的按键；
/// 字号缩放、设置与终端翻页沿用各终端的通行按键。
#[rustfmt::skip]
const COMMANDS: &[Command] = &[
    command("new_tab",                new::<NewTab>,             WS,       &["ctrl-shift-t"]),
    command("new_window",             new::<NewWindow>,          WS,       &["ctrl-shift-n"]),
    command("close_pane",             new::<ClosePane>,          WS,       &["ctrl-shift-w"]),
    command("close_tab",              new::<CloseTab>,           WS,       &[]),
    command("close_window",           new::<CloseWindow>,        WS,       &[]),
    command("next_tab",               new::<NextTab>,            WS,       &["ctrl-tab"]),
    command("previous_tab",           new::<PrevTab>,            WS,       &["ctrl-shift-tab"]),
    command("tab_1",                  tab::<0>,                  WS,       &["ctrl-alt-1"]),
    command("tab_2",                  tab::<1>,                  WS,       &["ctrl-alt-2"]),
    command("tab_3",                  tab::<2>,                  WS,       &["ctrl-alt-3"]),
    command("tab_4",                  tab::<3>,                  WS,       &["ctrl-alt-4"]),
    command("tab_5",                  tab::<4>,                  WS,       &["ctrl-alt-5"]),
    command("tab_6",                  tab::<5>,                  WS,       &["ctrl-alt-6"]),
    command("tab_7",                  tab::<6>,                  WS,       &["ctrl-alt-7"]),
    command("tab_8",                  tab::<7>,                  WS,       &["ctrl-alt-8"]),
    command("tab_9",                  tab::<8>,                  WS,       &["ctrl-alt-9"]),
    command("rename_tab",             new::<RenameTab>,          WS,       &[]),
    command("move_tab_to_new_window", new::<MoveToNewWindow>,    WS,       &[]),
    command("split_right",            new::<SplitRight>,         WS,       &["alt-shift-="]),
    command("split_down",             new::<SplitDown>,          WS,       &["alt-shift--"]),
    command("focus_left",             new::<FocusLeft>,          WS,       &["ctrl-alt-left"]),
    command("focus_right",            new::<FocusRight>,         WS,       &["ctrl-alt-right"]),
    command("focus_up",               new::<FocusUp>,            WS,       &["ctrl-alt-up"]),
    command("focus_down",             new::<FocusDown>,          WS,       &["ctrl-alt-down"]),
    command("maximize_pane",          new::<ToggleZoom>,         WS,       &["ctrl-shift-enter"]),
    command("command_palette",        new::<TogglePalette>,      WS,       &["ctrl-shift-p"]),
    command("quick_connect",          new::<QuickConnect>,       WS,       &["ctrl-shift-o"]),
    command("find",                   new::<Find>,               WS,       &["ctrl-shift-f"]),
    command("toggle_sidebar",         new::<ToggleSidebar>,      WS,       &["ctrl-shift-b"]),
    command("toggle_side_panel",      new::<ToggleContextPanel>, WS,       &["ctrl-alt-b"]),
    command("toggle_transfers",       new::<ToggleTransfers>,    WS,       &["ctrl-shift-j"]),
    command("open_sftp",              new::<OpenSftp>,           WS,       &[]),
    command("open_settings",          new::<OpenSettings>,       WS,       &["ctrl-,"]),
    // Ctrl+- 在 readline 中的撤销可用 Ctrl+Shift+- 代替
    command("increase_font_size",     new::<IncreaseFontSize>,   WS,       &["ctrl-=", "ctrl-+"]),
    command("decrease_font_size",     new::<DecreaseFontSize>,   WS,       &["ctrl--"]),
    command("reset_font_size",        new::<ResetFontSize>,      WS,       &["ctrl-0"]),
    command("copy",                   new::<Copy>,               TERMINAL, &["ctrl-shift-c"]),
    command("paste",                  new::<Paste>,              TERMINAL, &["ctrl-shift-v", "shift-insert"]),
    command("previous_prompt",        new::<PreviousPrompt>,     PRIMARY,  &["ctrl-shift-up"]),
    command("next_prompt",            new::<NextPrompt>,         PRIMARY,  &["ctrl-shift-down"]),
    command("scroll_page_up",         new::<ScrollPageUp>,       PRIMARY,  &["shift-pageup"]),
    command("scroll_page_down",       new::<ScrollPageDown>,     PRIMARY,  &["shift-pagedown"]),
];

fn binding(
    keys: &str,
    action: Box<dyn Action>,
    context: &str,
) -> Result<KeyBinding, InvalidKeystrokeError> {
    let predicate = KeyBindingContextPredicate::parse(context).ok().map(Rc::new);
    KeyBinding::load(keys, action, predicate, false, None, &DummyKeyboardMapper)
}

pub(crate) fn bind_default_keys(cx: &mut App) {
    cx.bind_keys(COMMANDS.iter().flat_map(|c| {
        c.keys
            .iter()
            .filter_map(|key| binding(key, (c.action)(), c.context).ok())
    }));
    // 只在进程已退出的终端里生效
    cx.bind_keys([KeyBinding::new(
        "enter",
        RestartTab,
        Some("Terminal && exited"),
    )]);
}

/// 平台层产生的命名键（另有 f1–f35）；其余按键都是单个字符。
const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "menu",
    "print",
    "pause",
];

/// GPUI 接受任意键名，写错的键名（如 `ctrl-shift-pgup`）会静默失效，这里先校验。
fn valid_keys(keys: &str) -> bool {
    keys.split_whitespace().all(|part| {
        Keystroke::parse(part).is_ok_and(|k| {
            k.key.chars().count() == 1
                || NAMED_KEYS.contains(&k.key.as_str())
                || k.key
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .is_some_and(|n| (1..=35).contains(&n))
        })
    })
}

/// 用户改键带这个标记，重新应用时据此换掉上一轮的绑定；其余绑定（含组件库的）原样保留。
const USER_KEYS: KeyBindingMetaIndex = KeyBindingMetaIndex(0);

/// 上一次应用的 `[keys]`。
struct AppliedKeys(BTreeMap<String, KeyList>);

impl Global for AppliedKeys {}

/// 应用 `[keys]` 中的改键，返回无法识别的条目。须在所有默认按键绑定之后调用。
pub(crate) fn apply_user_keys(keys: &BTreeMap<String, KeyList>, cx: &mut App) -> Vec<String> {
    if cx
        .try_global::<AppliedKeys>()
        .is_some_and(|applied| &applied.0 == keys)
    {
        return Vec::new();
    }
    let mut errors = Vec::new();
    let mut bindings = Vec::new();
    for (name, list) in keys {
        let Some(command) = COMMANDS.iter().find(|c| c.name == name) else {
            errors.push(t!("keys.unknown", name = name).into_owned());
            continue;
        };
        // 先撤掉默认按键，同一按键上的其他命令不受影响
        let unbind = Unbind((command.action)().name().into());
        for key in command.keys {
            if let Ok(b) = binding(key, unbind.boxed_clone(), command.context) {
                bindings.push(b.with_meta(USER_KEYS));
            }
        }
        for key in list.iter() {
            match binding(key, (command.action)(), command.context) {
                Ok(b) if valid_keys(key) => bindings.push(b.with_meta(USER_KEYS)),
                _ => errors.push(t!("keys.invalid", name = name, key = key).into_owned()),
            }
        }
    }
    let kept: Vec<KeyBinding> = cx
        .key_bindings()
        .borrow()
        .bindings()
        .filter(|b| b.meta() != Some(USER_KEYS))
        .cloned()
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(kept);
    cx.bind_keys(bindings);
    cx.set_global(AppliedKeys(keys.clone()));
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keys_parse() {
        for c in COMMANDS {
            for key in c.keys {
                assert!(
                    binding(key, (c.action)(), c.context).is_ok(),
                    "{} {key}",
                    c.name
                );
            }
        }
    }

    #[test]
    fn key_names_are_checked() {
        for ok in [
            "ctrl-t",
            "ctrl-shift-pageup",
            "alt-shift--",
            "ctrl-,",
            "f12",
            "ctrl-k ctrl-s",
        ] {
            assert!(valid_keys(ok), "{ok}");
        }
        for bad in [
            "not a key",
            "ctrl-shift-pgup",
            "ctrl-",
            "ctrl-shft-x",
            "f36",
        ] {
            assert!(!valid_keys(bad), "{bad}");
        }
    }

    #[test]
    fn command_names_are_unique() {
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }
}
