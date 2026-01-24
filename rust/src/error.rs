use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error("Invalid header")]
    InvalidHeader,

    #[error("Too many skipped messages")]
    TooManySkippedMessages,

    #[error("Not initiator, cannot send first message")]
    NotInitiator,

    #[error("Event must be unsigned")]
    EventMustBeUnsigned,

    #[error("Failed to decrypt header with available keys")]
    FailedToDecryptHeader,

    #[error("Invalid event: {0}")]
    InvalidEvent(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Session not ready")]
    SessionNotReady,

    #[error("Device ID required")]
    DeviceIdRequired,

    #[error("Invite error: {0}")]
    Invite(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Hex(#[from] hex::FromHexError),

    #[error(transparent)]
    NostrKey(#[from] nostr::key::Error),

    #[error(transparent)]
    Nostr(#[from] nostr::event::Error),

    #[error(transparent)]
    Nip44(#[from] nostr::nips::nip44::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
