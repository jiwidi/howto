use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Configuration(String),
    #[error("{0}")]
    Dependency(String),
    #[error("{0}")]
    Model(String),
    #[error("{0}")]
    Server(String),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    InvalidResponse(String),
    #[error("{0}")]
    Execution(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Configuration(_) | Self::Dependency(_) | Self::Model(_) => 3,
            Self::Server(_) | Self::Network(_) | Self::InvalidResponse(_) => 4,
            Self::Execution(_) | Self::Io(_) | Self::Json(_) => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
