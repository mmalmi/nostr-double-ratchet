pub mod app_keys;
pub mod group_codec;
pub mod message_builders;
pub mod message_origin;
pub mod multi_device;
pub mod nostr_codec;
pub mod one_to_many;
pub mod shared_channel;

pub use app_keys::{is_app_keys_event, AppKeys, DeviceEntry, DeviceLabels};
pub use group_codec::{JsonGroupPayloadCodecV1, NostrGroupManager};
pub use message_builders::*;
pub use message_origin::{classify_message_origin, MessageOrigin};
pub use multi_device::{
    apply_app_keys_snapshot, apply_app_keys_snapshot_with_required_device,
    evaluate_device_registration_state, resolve_conversation_candidate_pubkeys,
    resolve_invite_owner_routing, resolve_rumor_peer_pubkey, select_latest_app_keys_from_events,
    should_require_relay_registration_confirmation, AppKeysSnapshot, AppKeysSnapshotDecision,
    DeviceRegistrationState, InviteOwnerRoutingResolution,
};
pub use nostr_codec::{
    group_sender_key_message_event, invite_response_event, invite_unsigned_event, invite_url,
    message_event, parse_group_sender_key_message_event, parse_invite_event,
    parse_invite_response_event, parse_invite_url, parse_message_event, parse_roster_event,
    roster_unsigned_event, DecodedRosterEvent, GROUP_SENDER_KEY_MESSAGE_KIND, INVITE_EVENT_KIND,
    INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND, ROSTER_EVENT_KIND,
};
pub use one_to_many::*;
pub use shared_channel::SharedChannel;

pub use nostr_double_ratchet::*;

impl OwnerClaimVerifier for AppKeys {
    fn has_device(&self, _device_pubkey: DevicePubkey, device_identity: nostr::PublicKey) -> bool {
        self.get_device(&device_identity).is_some()
    }
}

pub const APP_KEYS_EVENT_KIND: u32 = ROSTER_EVENT_KIND;
pub const CHAT_MESSAGE_KIND: u32 = 14;
pub const CHAT_SETTINGS_KIND: u32 = 10448;
pub const REACTION_KIND: u32 = 7;
pub const RECEIPT_KIND: u32 = 15;
pub const TYPING_KIND: u32 = 25;
pub const SHARED_CHANNEL_KIND: u32 = 4;
pub const EXPIRATION_TAG: &str = "expiration";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOptions {
    pub expires_at: Option<u64>,
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatSettingsPayloadV1 {
    #[serde(rename = "type")]
    pub typ: String,
    pub v: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_ttl_seconds: Option<u64>,
}

pub mod utils {
    pub use nostr_double_ratchet::utils::*;

    use nostr::{PublicKey, SecretKey};
    use nostr_double_ratchet::{Error, Result};

    pub fn pubkey_from_hex(hex_str: &str) -> Result<PublicKey> {
        let bytes = hex::decode(hex_str)?;
        if bytes.len() != 32 {
            return Err(Error::InvalidEvent("Invalid pubkey length".to_string()));
        }
        PublicKey::from_slice(&bytes).map_err(|e| Error::InvalidEvent(e.to_string()))
    }

    pub fn resolve_expiration_seconds(
        options: &crate::SendOptions,
        now_seconds: u64,
    ) -> Result<Option<u64>> {
        let has_expires_at = options.expires_at.is_some();
        let has_ttl = options.ttl_seconds.is_some();
        if has_expires_at && has_ttl {
            return Err(Error::InvalidEvent(
                "Provide either expires_at or ttl_seconds, not both".to_string(),
            ));
        }

        if let Some(expires_at) = options.expires_at {
            return Ok(Some(expires_at));
        }

        if let Some(ttl) = options.ttl_seconds {
            return now_seconds
                .checked_add(ttl)
                .ok_or_else(|| Error::InvalidEvent("ttl_seconds overflow".to_string()))
                .map(Some);
        }

        Ok(None)
    }

    pub fn secret_key_from_bytes(bytes: &[u8; 32]) -> Result<SecretKey> {
        SecretKey::from_slice(bytes).map_err(Into::into)
    }
}

pub fn process_invite_response_event(
    invite: &Invite,
    event: &nostr::Event,
    inviter_private_key: [u8; 32],
) -> Result<Option<InviteResponse>> {
    let envelope = parse_invite_response_event(event)
        .map_err(|error| Error::InvalidEvent(error.to_string()))?;
    let mut rng = rand::rngs::OsRng;
    let mut ctx = ProtocolContext::new(UnixSeconds(event.created_at.as_secs()), &mut rng);
    let mut invite = invite.clone();
    invite
        .process_response(&mut ctx, &envelope, inviter_private_key)
        .map(Some)
}

pub trait SessionNostrExt {
    fn send_event(&mut self, event: nostr::UnsignedEvent) -> Result<nostr::Event>;
    fn receive(&mut self, event: &nostr::Event) -> Result<Option<String>>;
}

impl SessionNostrExt for Session {
    fn send_event(&mut self, mut event: nostr::UnsignedEvent) -> Result<nostr::Event> {
        event.ensure_id();
        let payload = serde_json::to_vec(&event)?;
        let now = UnixSeconds(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        let plan = self.plan_send(&payload, now)?;
        let envelope = self.apply_send(plan).envelope;
        message_event(&envelope).map_err(|error| Error::InvalidEvent(error.to_string()))
    }

    fn receive(&mut self, event: &nostr::Event) -> Result<Option<String>> {
        let envelope =
            parse_message_event(event).map_err(|error| Error::InvalidEvent(error.to_string()))?;
        if !self.matches_sender(envelope.sender) {
            return Ok(None);
        }
        let mut rng = rand::rngs::OsRng;
        let mut ctx = ProtocolContext::new(UnixSeconds(event.created_at.as_secs()), &mut rng);
        let plan = self.plan_receive(&mut ctx, &envelope)?;
        let outcome = self.apply_receive(plan);
        let plaintext = String::from_utf8(outcome.payload)
            .map_err(|error| Error::Decryption(error.to_string()))?;
        Ok(Some(plaintext))
    }
}

pub trait InviteNostrExt {
    fn get_url(&self, root: &str) -> Result<String>;
    fn get_event(&self) -> Result<nostr::UnsignedEvent>;
    fn from_url(url: &str) -> Result<Invite>;
    fn from_event(event: &nostr::Event) -> Result<Invite>;
    fn process_invite_response(
        &self,
        event: &nostr::Event,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>>;
}

impl InviteNostrExt for Invite {
    fn get_url(&self, root: &str) -> Result<String> {
        invite_url(self, root).map_err(|error| Error::InvalidEvent(error.to_string()))
    }

    fn get_event(&self) -> Result<nostr::UnsignedEvent> {
        if self.device_id.is_none() {
            return Err(Error::DeviceIdRequired);
        }
        invite_unsigned_event(self).map_err(|error| Error::InvalidEvent(error.to_string()))
    }

    fn from_url(url: &str) -> Result<Invite> {
        parse_invite_url(url).map_err(|error| Error::InvalidEvent(error.to_string()))
    }

    fn from_event(event: &nostr::Event) -> Result<Invite> {
        parse_invite_event(event).map_err(|error| Error::InvalidEvent(error.to_string()))
    }

    fn process_invite_response(
        &self,
        event: &nostr::Event,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>> {
        process_invite_response_event(self, event, inviter_private_key)
    }
}
