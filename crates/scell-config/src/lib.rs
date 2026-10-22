//! 持久化：设置、主题、主机库、凭据、运行状态与快捷键。
//!
//! 机密只进 OS 凭据库，配置文件中不出现明文。

rust_i18n::i18n!("locales", fallback = "en");

mod error;
pub mod hosts;
mod import;
mod paths;
mod search;
pub mod secrets;
pub mod settings;
mod state;
pub mod theme;
mod watch;

pub use error::ConfigError;
pub use hosts::{HostBook, HostId, HostProfile};
pub use import::{default_ssh_config, import_ssh_config};
pub use paths::{ConfigPaths, expand_tilde, local_user};
pub use search::search_hosts;
pub use settings::Settings;
pub use state::{SavedLayout, SavedState, SavedTab, SavedWindow};
pub use watch::{ConfigChange, ConfigWatcher};
