use thiserror::Error as ThisError;

#[derive(Debug, Clone, ThisError)]
pub enum Error {
    #[error("decryption failed: {0}")]
    Decryption(String),
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("invalid header")]
    InvalidHeader,
    #[error("invite error: {0}")]
    Invite(String),
    #[error("session is not ready")]
    SessionNotReady,
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("sender is not allowed to send first")]
    NotInitiator,
    #[error("too many skipped messages")]
    TooManySkippedMessages,
}

pub type Result<T> = std::result::Result<T, Error>;
