use rust_i18n::t;
use std::io;

pub type Result<T, E = VfsError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VfsError {
    #[error("{}", t!("error.not_found"))]
    NotFound,
    #[error("{}", t!("error.permission_denied"))]
    PermissionDenied,
    #[error("{}", t!("error.already_exists"))]
    AlreadyExists,
    #[error("{}", t!("error.unsupported"))]
    Unsupported,
    /// 服务器返回的失败及其说明。
    #[error("{0}")]
    Failure(String),
    #[error("{}", t!("error.connection_lost"))]
    ConnectionLost,
    /// 协议细节（英文），供排查。
    #[error("{}", t!("error.protocol", err = .0))]
    Protocol(String),
    #[error(transparent)]
    Io(io::Error),
}

impl From<io::Error> for VfsError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            io::ErrorKind::Unsupported => Self::Unsupported,
            _ => Self::Io(e),
        }
    }
}
