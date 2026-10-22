//! 运行状态（`state.toml`）：最近连接的主机、上次退出时的窗口布局等。由程序维护，读不到或损坏时从空状态开始。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ConfigError, HostId};

/// 最近使用列表的长度。
const RECENT_MAX: usize = 10;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SavedState {
    /// 最近连接的主机，最近的在前。
    pub recent_hosts: Vec<HostId>,
    /// 上次退出时打开的窗口。
    pub windows: Vec<SavedWindow>,
}

/// 由 UI 层整体构造，不加 `non_exhaustive`。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWindow {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    /// 窗口内容区的逻辑像素尺寸。
    pub size: Option<(f32, f32)>,
    pub sidebar_collapsed: bool,
    pub context_open: bool,
    pub transfers_open: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SavedTab {
    Terminals {
        /// 用户重命名的标题。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        layout: SavedLayout,
        /// 聚焦窗格在分屏树叶子中的序号。
        #[serde(default)]
        focused: usize,
    },
    Sftp {
        host: HostId,
    },
    Settings,
}

/// 分屏树。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SavedLayout {
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    Ssh {
        host: HostId,
    },
    Split {
        /// 左右排列；否则上下排列。
        row: bool,
        children: Vec<SavedLayout>,
    },
}

impl SavedState {
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), "运行状态文件无法解析，已忽略：{e}");
            Self::default()
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        crate::paths::write_atomic(path, &text)
    }

    /// 记一次连接：移到最前，超出长度的丢弃。
    pub fn touch_host(&mut self, id: &HostId) {
        self.recent_hosts.retain(|h| h != id);
        self.recent_hosts.insert(0, id.clone());
        self.recent_hosts.truncate(RECENT_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_hosts_move_to_front_and_round_trip() {
        let mut state = SavedState::default();
        for i in 0..12 {
            state.touch_host(&HostId(format!("h{i}")));
        }
        state.touch_host(&HostId("h5".into()));
        assert_eq!(state.recent_hosts.len(), RECENT_MAX);
        assert_eq!(state.recent_hosts[0].0, "h5");
        assert_eq!(state.recent_hosts[1].0, "h11");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        assert_eq!(SavedState::load(&path), SavedState::default());
        state.save(&path).unwrap();
        assert_eq!(SavedState::load(&path), state);
        std::fs::write(&path, "recent_hosts = 3").unwrap();
        assert_eq!(SavedState::load(&path), SavedState::default());

        let local = |cwd: &str| SavedLayout::Local {
            program: None,
            args: vec!["-l".into()],
            cwd: Some(cwd.into()),
        };
        state.windows = vec![SavedWindow {
            size: Some((900., 600.)),
            context_open: true,
            active: 1,
            tabs: vec![
                SavedTab::Terminals {
                    title: Some("构建".into()),
                    layout: SavedLayout::Split {
                        row: true,
                        children: vec![
                            local("/tmp"),
                            SavedLayout::Split {
                                row: false,
                                children: vec![
                                    SavedLayout::Ssh {
                                        host: HostId("h1".into()),
                                    },
                                    local("/"),
                                ],
                            },
                        ],
                    },
                    focused: 2,
                },
                SavedTab::Sftp {
                    host: HostId("h1".into()),
                },
                SavedTab::Settings,
            ],
            ..SavedWindow::default()
        }];
        state.save(&path).unwrap();
        assert_eq!(SavedState::load(&path), state);
    }
}
