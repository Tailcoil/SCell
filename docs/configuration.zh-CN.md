# 配置

[English](configuration.md)

| | Linux | Windows |
|---|---|---|
| 设置 | `~/.config/scell/settings.toml` | `%APPDATA%\scell\settings.toml` |
| 日志 | `~/.local/state/scell/logs` | `%LOCALAPPDATA%\scell\logs` |

设置文件保存后立即生效，大部分选项也可以在设置页（Ctrl+,）里修改。

## 快捷键

命令面板（Ctrl+Shift+P）列出了全部命令及其快捷键。要修改，在 `[keys]` 中写上命令名，写空列表表示取消：

```toml
[keys]
new_tab = "ctrl-t"
paste = ["ctrl-v", "shift-insert"]
find = []
```

命令名：`new_tab`、`new_window`、`close_pane`、`close_tab`、`close_window`、`next_tab`、`previous_tab`、`tab_1` … `tab_9`、`rename_tab`、`move_tab_to_new_window`、`split_right`、`split_down`、`focus_left`、`focus_right`、`focus_up`、`focus_down`、`maximize_pane`、`command_palette`、`quick_connect`、`find`、`toggle_sidebar`、`toggle_side_panel`、`toggle_transfers`、`open_sftp`、`open_settings`、`increase_font_size`、`decrease_font_size`、`reset_font_size`、`copy`、`paste`、`previous_prompt`、`next_prompt`、`scroll_page_up`、`scroll_page_down`

## Shell 集成

在命令之间跳转（Ctrl+Shift+↑/↓）依赖 shell 输出的提示符标记（OSC 133）；SSH 会话中，文件面板依据 shell 上报的目录（OSC 7）跟随切换。fish 默认就会输出这两项。

zsh，加入 `~/.zshrc`：

```zsh
_scell_mark() { print -n "\e]133;A\a\e]7;file://$HOST${PWD//\%/%25}\a" }
precmd_functions+=(_scell_mark)
```

bash，加入 `~/.bashrc`：

```bash
_scell_mark() { printf "\e]133;A\a\e]7;file://%s%s\a" "$HOSTNAME" "${PWD//\%/%25}"; }
PROMPT_COMMAND="_scell_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```
