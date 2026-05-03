use nostr::PublicKey;
use serde::{Deserialize, Serialize};

/// Canonical origin classification for decrypted messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageOrigin {
    /// Message originated from this exact local device.
    LocalDevice,
    /// Message originated from another device owned by the same owner key.
    SameOwnerOtherDevice,
    /// Message originated from a different owner key.
    RemoteOwner,
    /// Provenance is incomplete, so origin cannot be classified confidently.
    Unknown,
}

impl MessageOrigin {
    /// Returns true when origin is self (local or same-owner other device).
    pub fn is_self(self) -> bool {
        matches!(
            self,
            MessageOrigin::LocalDevice | MessageOrigin::SameOwnerOtherDevice
        )
    }

    /// Returns true when origin is a cross-device self message.
    pub fn is_cross_device_self(self) -> bool {
        matches!(self, MessageOrigin::SameOwnerOtherDevice)
    }
}

/// Classify message origin from authenticated owner/device provenance.
///
/// `sender_owner_pubkey` and `sender_device_pubkey` should come from authenticated
/// session/group routing context, not untrusted inner rumor metadata.
pub fn classify_message_origin(
    our_owner_pubkey: PublicKey,
    our_device_pubkey: Option<PublicKey>,
    sender_owner_pubkey: Option<PublicKey>,
    sender_device_pubkey: Option<PublicKey>,
) -> MessageOrigin {
    if let Some(owner_pubkey) = sender_owner_pubkey {
        if owner_pubkey != our_owner_pubkey {
            return MessageOrigin::RemoteOwner;
        }

        return match (our_device_pubkey, sender_device_pubkey) {
            (Some(our_device), Some(sender_device)) if sender_device == our_device => {
                MessageOrigin::LocalDevice
            }
            (Some(_), Some(_)) => MessageOrigin::SameOwnerOtherDevice,
            _ => MessageOrigin::Unknown,
        };
    }

    match (our_device_pubkey, sender_device_pubkey) {
        (Some(our_device), Some(sender_device)) if sender_device == our_device => {
            MessageOrigin::LocalDevice
        }
        _ => MessageOrigin::Unknown,
    }
}
