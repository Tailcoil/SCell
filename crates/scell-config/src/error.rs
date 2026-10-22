use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("{}", rust_i18n::t!("error.watch", err = .0))]
    Watch(#[from] notify::Error),
}
