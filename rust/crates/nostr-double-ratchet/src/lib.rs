pub mod app_keys;
pub mod app_keys_manager;
pub mod delegate_manager;
pub mod direct_message_subscriptions;
pub mod error;
pub mod file_storage;
pub mod group;
pub mod group_manager;
pub mod invite;
pub mod message_builders;
pub mod message_origin;
pub mod message_queue;
pub mod multi_device;
#[cfg(feature = "nearby")]
pub mod nearby;
#[cfg(feature = "nearby-mdns")]
pub mod nearby_lan;
pub mod one_to_many;
pub mod protocol_backfill;
pub mod pubsub;
pub mod runtime;
pub mod sender_key;
pub mod session;
pub mod session_manager;
pub mod shared_channel;
pub mod storage;
pub mod types;
pub mod user_record;
pub mod utils;

pub use app_keys::{is_app_keys_event, AppKeys, DeviceEntry, DeviceLabels};
pub use app_keys_manager::AppKeysManager;
pub use delegate_manager::{DelegateManager, DelegatePayload};
pub use direct_message_subscriptions::{
    build_direct_message_backfill_filter, direct_message_subscription_authors,
    DirectMessageSubscriptionTracker,
};
pub use error::{Error, Result};
pub use file_storage::{DebouncedFileStorage, FileStorageAdapter};
pub use group::*;
pub use group_manager::*;
pub use invite::{Invite, InviteResponse};
pub use message_builders::*;
pub use message_origin::{classify_message_origin, MessageOrigin};
pub use message_queue::{MessageQueue, QueueEntry};
pub use multi_device::{
    apply_app_keys_snapshot, apply_app_keys_snapshot_with_required_device,
    evaluate_device_registration_state, resolve_conversation_candidate_pubkeys,
    resolve_invite_owner_routing, resolve_rumor_peer_pubkey, select_latest_app_keys_from_events,
    should_require_relay_registration_confirmation, AppKeysSnapshot, AppKeysSnapshotDecision,
    DeviceRegistrationState, InviteOwnerRoutingResolution,
};
#[cfg(feature = "nearby")]
pub use nearby::{
    decode_nearby_frame_json, encode_nearby_frame_json, nearby_frame_body_len_from_header,
    read_nearby_frame, NearbyFrameAssembler, NEARBY_FRAME_HEADER_BYTES,
    NEARBY_MAX_FRAME_BODY_BYTES,
};
#[cfg(feature = "nearby-mdns")]
pub use nearby_lan::{
    is_allowed_nearby_peer, NearbyLanConfig, NearbyLanError, NearbyLanIncoming, NearbyLanService,
    IRIS_NEARBY_SERVICE_TYPE,
};
pub use one_to_many::*;
pub use protocol_backfill::{
    NdrProtocolBackfillOptions, DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS,
    DEFAULT_MESSAGE_BACKFILL_LOOKBACK_SECS,
};
pub use pubsub::{ChannelPubSub, NostrPubSub, SessionEvent};
pub use runtime::NdrRuntime;
pub use sender_key::*;
pub use session::Session;
pub use session_manager::{
    AcceptInviteResult, MessagePushSessionStateSnapshot, QueuedMessageDiagnostic,
    QueuedMessageStage, SessionManager, SessionManagerEvent,
};
pub use shared_channel::SharedChannel;
pub use storage::{InMemoryStorage, StorageAdapter};
pub use types::*;
pub use user_record::{DeviceRecord, StoredDeviceRecord, StoredUserRecord, UserRecord};

#[cfg(test)]
mod architecture_tests {
    #[test]
    fn session_core_does_not_manage_pubsub_or_subscriptions() {
        let source = include_str!("session.rs");
        for banned in [
            "NostrPubSub",
            "SessionManagerEvent",
            "crossbeam_channel",
            "subscribe_to_messages",
            "update_subscriptions",
            "CHAT_MESSAGE_KIND",
            "REACTION_KIND",
            "RECEIPT_KIND",
            "TYPING_KIND",
            "EXPIRATION_TAG",
            "send_reaction",
            "send_reply",
            "send_receipt",
            "send_typing",
        ] {
            assert!(
                !source.contains(banned),
                "Session should stay pure ratchet state/encryption; found `{banned}`"
            );
        }
    }

    #[test]
    fn session_manager_does_not_own_direct_message_subscriptions() {
        let source = include_str!("session_manager.rs");
        for banned in [
            "sync_session_message_subscription",
            "session_message_subscription",
            "session-manager-messages-",
        ] {
            assert!(
                !source.contains(banned),
                "SessionManager should expose direct-message authors, not own subscriptions; found `{banned}`"
            );
        }
    }
}
