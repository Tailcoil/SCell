<p align="center">
  <img src="assets/icons/scell.svg" width="112" alt="SCell logo">
</p>

<h1 align="center">SCell</h1>

<p align="center">More than fast: a clean, intuitive, all-in-one modern terminal</p>

<p align="center">
  <a href="https://github.com/Tailcoil/scell/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Tailcoil/scell/ci.yml?branch=main&label=CI" alt="CI"></a>
  <a href="https://github.com/Tailcoil/scell/releases/latest"><img src="https://img.shields.io/github/v/release/Tailcoil/scell?label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20Windows-1f5560" alt="Platform: Linux | Windows">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-1f5560" alt="License: Apache-2.0"></a>
  <br>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-1f5560?logo=rust&logoColor=white" alt="Rust"></a>
  <a href="https://www.gpui.rs"><img src="https://img.shields.io/badge/GPUI-1f5560?logo=zedindustries&logoColor=white" alt="GPUI"></a>
  <a href="https://github.com/longbridge/gpui-component"><img src="https://img.shields.io/badge/gpui--component-1f5560" alt="gpui-component"></a>
  <a href="https://github.com/alacritty/alacritty"><img src="https://img.shields.io/badge/alacritty__terminal-1f5560?logo=alacritty&logoColor=white" alt="alacritty_terminal"></a>
  <a href="https://github.com/Eugeny/russh"><img src="https://img.shields.io/badge/russh-1f5560" alt="russh"></a>
</p>

<p align="center">
  <a href="https://github.com/Tailcoil/scell/releases/latest">Download</a> ·
  <a href="docs/configuration.md">Configuration</a> ·
  <a href="README.zh-CN.md">简体中文</a>
</p>

![SCell with split panes and the server status panel](assets/screenshots/en/overview.webp)

- SSH host manager with chained jump hosts
- Dual-pane SFTP with resumable transfers
- Agentless server monitoring
- Local, remote and SOCKS port forwarding
- GBK, Big5, Shift_JIS and other legacy encodings
- Vertical tabs and split panes

![Host editor, SFTP tab, command palette and the light theme](assets/screenshots/en/gallery.webp)

## Install

| System | Install |
|---|---|
| Windows | [`setup.exe`, portable `.zip`](https://github.com/Tailcoil/scell/releases/latest) |
| Debian, Ubuntu | [`.deb`](https://github.com/Tailcoil/scell/releases/latest) |
| Fedora | [`.rpm`](https://github.com/Tailcoil/scell/releases/latest) |
| Arch Linux | [`.pkg.tar.zst`](https://github.com/Tailcoil/scell/releases/latest/download/scell-bin-x86_64.pkg.tar.zst), install with `sudo pacman -U` |
| Nix | `nix run github:Tailcoil/scell` |
| Other Linux | [AppImage, `.tar.gz`](https://github.com/Tailcoil/scell/releases/latest) |

## Build

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libfontconfig-dev libfreetype-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev
cargo build --release
```

## License

[Apache-2.0](LICENSE). The Catppuccin theme uses the [Catppuccin](https://catppuccin.com) palette.
