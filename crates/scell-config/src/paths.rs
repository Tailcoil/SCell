use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ConfigError;

/// 配置目录布局。`SCELL_CONFIG_DIR` 优先于平台默认目录（Linux `~/.config/scell`，Windows `%APPDATA%\scell`）。
///
/// 运行状态（最近使用等）不属于配置，默认放在平台的状态目录（Linux `~/.local/state/scell`，
/// Windows `%LOCALAPPDATA%\scell`），不混进用户可能纳入版本管理的配置目录；指定了 `SCELL_CONFIG_DIR` 时放在其中，便于隔离。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    root: PathBuf,
    state: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Option<Self> {
        if let Some(dir) = std::env::var_os("SCELL_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return Some(Self::from_root(dir));
        }
        let root = dirs::config_dir()?.join("scell");
        let state = dirs::state_dir().or_else(dirs::data_local_dir).map_or_else(
            || root.join("state.toml"),
            |d| d.join("scell").join("state.toml"),
        );
        Some(Self { root, state })
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            state: root.join("state.toml"),
            root,
        }
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    pub fn hosts(&self) -> PathBuf {
        self.root.join("hosts.toml")
    }

    /// 日志与崩溃记录，与运行状态放在一起。
    pub fn logs_dir(&self) -> PathBuf {
        self.state.parent().unwrap_or(&self.root).join("logs")
    }

    /// 用户主题目录，其中每个 `.json` 文件是一个主题集。
    pub fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }
}

/// 把 `~` 开头的路径展开到家目录；家目录未知时原样返回。
pub fn expand_tilde(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_owned(),
    }
}

/// 本机登录名，作为主机未写用户名时的默认值。
pub fn local_user() -> Option<String> {
    ["USER", "USERNAME"]
        .into_iter()
        .find_map(|v| std::env::var(v).ok().filter(|u| !u.is_empty()))
}

/// 原子写入：先写临时文件再改名，热重载不会读到写了一半的文件。
/// 临时文件名按次唯一，同一文件的并发写入互不踩踏，以最后一次改名为准。
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let io_err = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err)?;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}-{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(io_err)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(e)
    })
}
