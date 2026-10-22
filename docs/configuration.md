# Configuration

[简体中文](configuration.zh-CN.md)

| | Linux | Windows |
|---|---|---|
| Settings | `~/.config/scell/settings.toml` | `%APPDATA%\scell\settings.toml` |
| Logs | `~/.local/state/scell/logs` | `%LOCALAPPDATA%\scell\logs` |

Settings apply when the file is saved. Most of them are also on the settings page (Ctrl+,).

## Shortcuts

The command palette (Ctrl+Shift+P) shows every command with its shortcut. To change a shortcut, add the command to `[keys]`; an empty list removes it:

```toml
[keys]
new_tab = "ctrl-t"
paste = ["ctrl-v", "shift-insert"]
find = []
```

Commands: `new_tab`, `new_window`, `close_pane`, `close_tab`, `close_window`, `next_tab`, `previous_tab`, `tab_1` … `tab_9`, `rename_tab`, `move_tab_to_new_window`, `split_right`, `split_down`, `focus_left`, `focus_right`, `focus_up`, `focus_down`, `maximize_pane`, `command_palette`, `quick_connect`, `find`, `toggle_sidebar`, `toggle_side_panel`, `toggle_transfers`, `open_sftp`, `open_settings`, `increase_font_size`, `decrease_font_size`, `reset_font_size`, `copy`, `paste`, `previous_prompt`, `next_prompt`, `scroll_page_up`, `scroll_page_down`

## Shell integration

Jumping between commands (Ctrl+Shift+↑/↓) relies on the prompt marks the shell prints (OSC 133). In SSH sessions, the file panel follows the directory the shell reports (OSC 7). fish does both by default.

zsh, in `~/.zshrc`:

```zsh
_scell_mark() { print -n "\e]133;A\a\e]7;file://$HOST${PWD//\%/%25}\a" }
precmd_functions+=(_scell_mark)
```

bash, in `~/.bashrc`:

```bash
_scell_mark() { printf "\e]133;A\a\e]7;file://%s%s\a" "$HOSTNAME" "${PWD//\%/%25}"; }
PROMPT_COMMAND="_scell_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
```
