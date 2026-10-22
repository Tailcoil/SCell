use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{ConfigError, ConfigPaths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChange {
    Settings,
    Hosts,
    Themes,
}

/// 监听设置文件、主机库与主题目录。监听的是目录而非文件：编辑器常以“写临时文件再改名”的方式保存。
///
/// 一次保存通常触发多个事件，接收方应自行合并。
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// 目录不存在时会先创建。
    pub fn start(
        paths: &ConfigPaths,
    ) -> Result<(Self, flume::Receiver<ConfigChange>), ConfigError> {
        let themes_dir = paths.themes_dir();
        std::fs::create_dir_all(&themes_dir).map_err(|source| ConfigError::Io {
            path: themes_dir.clone(),
            source,
        })?;
        // 事件里是绝对路径，相对的 SCELL_CONFIG_DIR 需要先规范化才能比较
        let root = canonical(paths.root());
        let themes_dir = canonical(&themes_dir);
        let settings = root.join("settings.toml");
        let hosts = root.join("hosts.toml");

        let (tx, rx) = flume::unbounded();
        let filter_themes = themes_dir.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                for path in &event.paths {
                    let change = if *path == settings {
                        ConfigChange::Settings
                    } else if *path == hosts {
                        ConfigChange::Hosts
                    } else if path.parent() == Some(&filter_themes)
                        && path.extension().is_some_and(|e| e == "json")
                    {
                        ConfigChange::Themes
                    } else {
                        continue;
                    };
                    let _ = tx.send(change);
                }
            })?;
        watcher.watch(&root, RecursiveMode::NonRecursive)?;
        watcher.watch(&themes_dir, RecursiveMode::NonRecursive)?;
        Ok((Self { _watcher: watcher }, rx))
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn reports_settings_and_theme_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::from_root(dir.path());
        let (_watcher, rx) = ConfigWatcher::start(&paths).unwrap();

        std::fs::write(paths.settings(), "[terminal]\n").unwrap();
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, ConfigChange::Settings);

        std::fs::write(paths.themes_dir().join("a.json"), "{}").unwrap();
        let got = std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(5)).ok())
            .find(|c| *c == ConfigChange::Themes);
        assert_eq!(got, Some(ConfigChange::Themes));
    }
}
