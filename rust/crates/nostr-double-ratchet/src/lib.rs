pub mod error;
pub mod group;
pub mod group_manager;
pub mod ids;
pub mod invite;
pub mod protocol_types;
pub mod roster;
pub mod roster_editor;
pub mod sender_key;
pub mod session;
pub mod session_manager;
pub mod utils;

pub use error::{DomainError, Error, Result};
pub use group::*;
pub use group_manager::*;
pub use ids::{DevicePubkey, OwnerPubkey, UnixSeconds};
pub use invite::{Invite, InviteResponse, InviteResponseEnvelope, OwnerClaimVerifier};
pub use protocol_types::{ProtocolContext, MAX_SKIP};
pub use roster::{AuthorizedDevice, DeviceRoster, RosterSnapshotDecision};
pub use roster_editor::RosterEditor;
pub use sender_key::*;
pub use session::{
    Header, MessageEnvelope, ReceiveOutcome, ReceivePlan, SendOutcome, SendPlan,
    SerializableKeyPair, Session, SessionState, SkippedKeysEntry,
};
pub use session_manager::{
    Delivery, DeviceRecordSnapshot, PreparedSend, ProcessedInviteResponse, PruneReport,
    ReceivedMessage, RelayGap, SessionManager, SessionManagerSnapshot, UserRecordSnapshot,
};

pub(crate) use ids::owner_pubkey_from_device_pubkey;
pub(crate) use utils::{
    device_pubkey_from_secret_bytes, kdf, random_secret_key_bytes, secret_key_from_bytes,
};

#[cfg(test)]
mod architecture_tests {
    #[test]
    fn core_modules_do_not_manage_runtime_or_wire() {
        let sources = [
            include_str!("group.rs"),
            include_str!("group_manager.rs"),
            include_str!("invite.rs"),
            include_str!("roster.rs"),
            include_str!("roster_editor.rs"),
            include_str!("sender_key.rs"),
            include_str!("session.rs"),
            include_str!("session_manager.rs"),
        ];
        for banned in [
            "NdrRuntime",
            "SessionManagerEvent",
            "StorageAdapter",
            "crossbeam",
            "tokio",
            "EventBuilder",
            "UnsignedEvent",
            "Filter",
            "nostr_codec",
            "pairwise_codec",
            "GroupWireEnvelopeV1",
            "GroupPairwisePayloadV1",
            "GroupSenderKeyPlaintextV1",
            "wire_format_version",
        ] {
            for source in sources {
                assert!(
                    !source.contains(banned),
                    "core should stay deterministic and wire/runtime-free; found `{banned}`"
                );
            }
        }
    }
}
