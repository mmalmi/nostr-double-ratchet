//! Error types for the FFI layer.

use std::fmt;

/// FFI-friendly error type.
#[derive(Debug, Clone, uniffi::Error)]
pub enum NdrError {
    InvalidKey(String),
    InvalidEvent(String),
    CryptoFailure(String),
    StateMismatch(String),
    Serialization(String),
    InviteError(String),
    SessionNotReady(String),
}

impl fmt::Display for NdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NdrError::InvalidKey(msg) => write!(f, "Invalid key: {}", msg),
            NdrError::InvalidEvent(msg) => write!(f, "Invalid event: {}", msg),
            NdrError::CryptoFailure(msg) => write!(f, "Crypto failure: {}", msg),
            NdrError::StateMismatch(msg) => write!(f, "State mismatch: {}", msg),
            NdrError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            NdrError::InviteError(msg) => write!(f, "Invite error: {}", msg),
            NdrError::SessionNotReady(msg) => write!(f, "Session not ready: {}", msg),
        }
    }
}

impl std::error::Error for NdrError {}

impl From<nostr_double_ratchet::Error> for NdrError {
    fn from(err: nostr_double_ratchet::Error) -> Self {
        match err {
            nostr_double_ratchet::Error::Encryption(msg) => NdrError::CryptoFailure(msg),
            nostr_double_ratchet::Error::Decryption(msg) => NdrError::CryptoFailure(msg),
            nostr_double_ratchet::Error::InvalidHeader => {
                NdrError::InvalidEvent("Invalid header".into())
            }
            nostr_double_ratchet::Error::TooManySkippedMessages => {
                NdrError::StateMismatch("Too many skipped messages".into())
            }
            nostr_double_ratchet::Error::NotInitiator => {
                NdrError::SessionNotReady("Not initiator".into())
            }
            nostr_double_ratchet::Error::EventMustBeUnsigned => {
                NdrError::InvalidEvent("Event must be unsigned".into())
            }
            nostr_double_ratchet::Error::FailedToDecryptHeader => {
                NdrError::CryptoFailure("Failed to decrypt header".into())
            }
            nostr_double_ratchet::Error::InvalidEvent(msg) => NdrError::InvalidEvent(msg),
            nostr_double_ratchet::Error::Serialization(msg) => NdrError::Serialization(msg),
            nostr_double_ratchet::Error::Storage(msg) => NdrError::Serialization(msg),
            nostr_double_ratchet::Error::SessionNotReady => {
                NdrError::SessionNotReady("Session not ready".into())
            }
            nostr_double_ratchet::Error::DeviceIdRequired => {
                NdrError::InviteError("Device ID required".into())
            }
            nostr_double_ratchet::Error::Invite(msg) => NdrError::InviteError(msg),
            nostr_double_ratchet::Error::Json(e) => NdrError::Serialization(e.to_string()),
            nostr_double_ratchet::Error::Hex(e) => NdrError::InvalidKey(e.to_string()),
            nostr_double_ratchet::Error::NostrKey(e) => NdrError::InvalidKey(e.to_string()),
            nostr_double_ratchet::Error::Nostr(e) => NdrError::InvalidEvent(e.to_string()),
            nostr_double_ratchet::Error::Nip44(e) => NdrError::CryptoFailure(e.to_string()),
        }
    }
}

impl From<serde_json::Error> for NdrError {
    fn from(err: serde_json::Error) -> Self {
        NdrError::Serialization(err.to_string())
    }
}

impl From<hex::FromHexError> for NdrError {
    fn from(err: hex::FromHexError) -> Self {
        NdrError::InvalidKey(err.to_string())
    }
}
