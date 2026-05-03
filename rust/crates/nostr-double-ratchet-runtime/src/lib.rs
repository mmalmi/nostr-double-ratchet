pub mod app_keys_manager;
pub mod delegate_manager;
pub mod direct_message_subscriptions;
pub mod file_storage;
pub mod message_queue;
#[cfg(feature = "nearby")]
pub mod nearby;
#[cfg(feature = "nearby-mdns")]
pub mod nearby_lan;
pub mod protocol_backfill;
pub mod pubsub;
pub mod runtime;
pub mod storage;
pub mod user_record;

pub use app_keys_manager::AppKeysManager;
pub use delegate_manager::{DelegateManager, DelegatePayload};
pub use direct_message_subscriptions::{
    build_direct_message_backfill_filter, direct_message_subscription_authors,
    DirectMessageSubscriptionTracker,
};
pub use file_storage::{DebouncedFileStorage, FileStorageAdapter};
pub use message_queue::{MessageQueue, QueueEntry};
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
pub use nostr_double_ratchet::*;
pub use nostr_double_ratchet_nostr::{
    self as nostr_adapter, is_app_keys_event, AppKeys, DeviceEntry, DeviceLabels,
    APP_KEYS_EVENT_KIND, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, EXPIRATION_TAG,
    GROUP_SENDER_KEY_MESSAGE_KIND, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
    REACTION_KIND, RECEIPT_KIND, SHARED_CHANNEL_KIND, TYPING_KIND,
};
pub use nostr_double_ratchet_nostr::{
    apply_app_keys_snapshot, apply_app_keys_snapshot_with_required_device,
    evaluate_device_registration_state, resolve_conversation_candidate_pubkeys,
    resolve_invite_owner_routing, resolve_rumor_peer_pubkey, select_latest_app_keys_from_events,
    should_require_relay_registration_confirmation, AppKeysSnapshot, AppKeysSnapshotDecision,
    ChatSettingsPayloadV1, DeviceRegistrationState, InviteOwnerRoutingResolution,
    JsonGroupPayloadCodecV1, NostrGroupManager, SendOptions,
};
pub use nostr_double_ratchet_nostr::{message_builders, message_origin, multi_device, nostr_codec};
pub use nostr_double_ratchet_pairwise_codec as pairwise_codec;
pub use protocol_backfill::{
    NdrProtocolBackfillOptions, DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS,
    DEFAULT_MESSAGE_BACKFILL_LOOKBACK_SECS,
};
pub use pubsub::{ChannelPubSub, NostrPubSub, SessionEvent};
pub use runtime::{
    AcceptInviteResult, GroupOuterSubscriptionPlan, MessagePushSessionStateSnapshot, NdrRuntime,
    QueuedMessageDiagnostic, QueuedMessageStage, SessionManagerEvent,
};
pub use storage::{InMemoryStorage, StorageAdapter};
pub use user_record::{DeviceRecord, StoredDeviceRecord, StoredUserRecord, UserRecord};

pub mod utils {
    pub use nostr_double_ratchet_nostr::utils::*;
}

pub trait InviteRuntimeExt {
    fn listen(&self, event_tx: &crossbeam_channel::Sender<SessionManagerEvent>) -> Result<()>;
    fn from_user(
        user_pubkey: nostr::PublicKey,
        event_tx: &crossbeam_channel::Sender<SessionManagerEvent>,
    ) -> Result<()>;
}

impl InviteRuntimeExt for Invite {
    fn listen(&self, event_tx: &crossbeam_channel::Sender<SessionManagerEvent>) -> Result<()> {
        let filter = pubsub::build_filter()
            .kinds(vec![INVITE_RESPONSE_KIND as u64])
            .pubkeys(vec![self.inviter_ephemeral_public_key.to_nostr()?])
            .build();
        let filter_json = serde_json::to_string(&filter)?;
        event_tx
            .send(SessionManagerEvent::Subscribe {
                subid: format!("invite-response-{}", uuid::Uuid::new_v4()),
                filter_json,
            })
            .map_err(|error| Error::InvalidEvent(error.to_string()))?;
        Ok(())
    }

    fn from_user(
        user_pubkey: nostr::PublicKey,
        event_tx: &crossbeam_channel::Sender<SessionManagerEvent>,
    ) -> Result<()> {
        let filter = nostr::Filter::new()
            .kind(nostr::Kind::from(INVITE_EVENT_KIND as u16))
            .authors(vec![user_pubkey])
            .custom_tag(
                nostr::SingleLetterTag::lowercase(nostr::Alphabet::L),
                "double-ratchet/invites",
            );
        let filter_json = serde_json::to_string(&filter)?;
        event_tx
            .send(SessionManagerEvent::Subscribe {
                subid: format!("invite-user-{}", uuid::Uuid::new_v4()),
                filter_json,
            })
            .map_err(|error| Error::InvalidEvent(error.to_string()))?;
        Ok(())
    }
}
