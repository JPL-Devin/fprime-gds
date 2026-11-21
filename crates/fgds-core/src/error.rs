/// Errors of the core and of plugin code called by it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Two registrations claim the same name; the message names both plugins (CUS-10).
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("configuration: {0}")]
    Config(String),
    /// A request that is malformed or names something that does not exist (API-8).
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Dictionary(#[from] fgds_dictionary::DictError),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(message: impl std::fmt::Display) -> Self {
        Error::Other(message.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
