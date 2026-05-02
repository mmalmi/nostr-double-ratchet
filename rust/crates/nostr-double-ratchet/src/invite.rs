use crate::{
    owner_pubkey_from_device_pubkey, random_secret_key_bytes, secret_key_from_bytes, DevicePubkey,
    DeviceRoster, DomainError, OwnerPubkey, ProtocolContext, Result, Session, UnixSeconds,
    INVITE_EVENT_KIND, INVITE_RESPONSE_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::{Alphabet, Kind, PublicKey, SingleLetterTag};
use rand::rngs::OsRng;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invite {
    pub inviter_device_pubkey: DevicePubkey,
    pub inviter_ephemeral_public_key: DevicePubkey,
    #[serde(with = "serde_bytes_array")]
    pub shared_secret: [u8; 32],
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_option_bytes_array"
    )]
    pub inviter_ephemeral_private_key: Option<[u8; 32]>,
    pub max_uses: Option<usize>,
    pub used_by: Vec<DevicePubkey>,
    pub created_at: UnixSeconds,
    pub inviter_owner_pubkey: Option<OwnerPubkey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub inviter: PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_public_key: Option<PublicKey>,
}

#[derive(Debug, Clone)]
pub struct InviteResponse {
    pub session: Session,
    pub invitee_device_pubkey: DevicePubkey,
    pub invitee_owner_pubkey: Option<OwnerPubkey>,
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteResponseEnvelope {
    pub sender: DevicePubkey,
    pub signer_secret_key: [u8; 32],
    pub recipient: DevicePubkey,
    pub created_at: UnixSeconds,
    pub content: String,
}

impl InviteResponse {
    pub fn resolved_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key.unwrap_or(self.invitee_identity)
    }

    pub fn claimed_owner_pubkey(&self) -> Option<OwnerPubkey> {
        self.invitee_owner_pubkey
    }

    pub fn has_verified_owner_claim(&self, verifier: Option<&dyn OwnerClaimVerifier>) -> bool {
        let owner_pubkey = self
            .invitee_owner_pubkey
            .unwrap_or_else(|| owner_pubkey_from_device_pubkey(self.invitee_device_pubkey));

        if owner_pubkey == owner_pubkey_from_device_pubkey(self.invitee_device_pubkey) {
            return true;
        }

        verifier.is_some_and(|verifier| {
            verifier.has_device(self.invitee_device_pubkey, self.invitee_identity)
        })
    }
}

pub trait OwnerClaimVerifier {
    fn has_device(&self, device_pubkey: DevicePubkey, device_identity: PublicKey) -> bool;
}

impl OwnerClaimVerifier for DeviceRoster {
    fn has_device(&self, device_pubkey: DevicePubkey, _device_identity: PublicKey) -> bool {
        self.get_device(&device_pubkey).is_some()
    }
}

impl OwnerClaimVerifier for crate::AppKeys {
    fn has_device(&self, _device_pubkey: DevicePubkey, device_identity: PublicKey) -> bool {
        self.get_device(&device_identity).is_some()
    }
}

impl Invite {
    pub fn create_new(
        inviter: PublicKey,
        device_id: Option<String>,
        max_uses: Option<usize>,
    ) -> Result<Self> {
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(
            UnixSeconds(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            ),
            &mut rng,
        );
        let mut invite = Self::create_new_with_context(
            &mut ctx,
            DevicePubkey::from_bytes(inviter.to_bytes()),
            None,
            max_uses,
        )?;
        invite.device_id = device_id;
        Ok(invite)
    }

    pub fn create_new_with_context<R>(
        ctx: &mut ProtocolContext<'_, R>,
        inviter_device_pubkey: DevicePubkey,
        inviter_owner_pubkey: Option<OwnerPubkey>,
        max_uses: Option<usize>,
    ) -> Result<Self>
    where
        R: RngCore + CryptoRng,
    {
        let inviter_ephemeral_private_key = random_secret_key_bytes(ctx.rng)?;
        let inviter_ephemeral_public_key =
            crate::device_pubkey_from_secret_bytes(&inviter_ephemeral_private_key)?;
        let shared_secret = random_secret_key_bytes(ctx.rng)?;

        Ok(Self {
            inviter_device_pubkey,
            inviter_ephemeral_public_key,
            shared_secret,
            inviter_ephemeral_private_key: Some(inviter_ephemeral_private_key),
            max_uses,
            used_by: Vec::new(),
            created_at: ctx.now,
            inviter_owner_pubkey,
            purpose: None,
            inviter: inviter_device_pubkey.to_nostr()?,
            device_id: None,
            owner_public_key: inviter_owner_pubkey
                .map(|owner| owner.to_nostr())
                .transpose()?,
        })
    }

    pub fn serialize(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn get_url(&self, root: &str) -> Result<String> {
        crate::nostr_codec::invite_url(self, root)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
    }

    pub fn from_url(url: &str) -> Result<Self> {
        crate::nostr_codec::parse_invite_url(url)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
    }

    pub fn get_event(&self) -> Result<nostr::UnsignedEvent> {
        if self.device_id.is_none() {
            return Err(crate::Error::DeviceIdRequired);
        }
        crate::nostr_codec::invite_unsigned_event(self)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
    }

    pub fn from_event(event: &nostr::Event) -> Result<Self> {
        crate::nostr_codec::parse_invite_event(event)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
    }

    pub fn accept(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
    ) -> Result<(Session, nostr::Event)> {
        self.accept_with_owner(invitee_public_key, invitee_private_key, device_id, None)
    }

    pub fn accept_with_owner(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
        owner_public_key: Option<PublicKey>,
    ) -> Result<(Session, nostr::Event)> {
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(now_seconds(), &mut rng);
        let (session, envelope) = self.accept_with_owner_context_and_device(
            &mut ctx,
            DevicePubkey::from_bytes(invitee_public_key.to_bytes()),
            invitee_private_key,
            owner_public_key.map(|owner| OwnerPubkey::from_bytes(owner.to_bytes())),
            device_id,
        )?;
        let event = crate::nostr_codec::invite_response_event(&envelope)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?;
        Ok((session, event))
    }

    pub fn accept_with_context<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
    ) -> Result<(Session, InviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.accept_with_owner_context(ctx, invitee_public_key, invitee_private_key, None)
    }

    pub fn accept_with_owner_context<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
        invitee_owner_pubkey: Option<OwnerPubkey>,
    ) -> Result<(Session, InviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.accept_with_owner_context_and_device(
            ctx,
            invitee_public_key,
            invitee_private_key,
            invitee_owner_pubkey,
            None,
        )
    }

    fn accept_with_owner_context_and_device<R>(
        &self,
        ctx: &mut ProtocolContext<'_, R>,
        invitee_public_key: DevicePubkey,
        invitee_private_key: [u8; 32],
        invitee_owner_pubkey: Option<OwnerPubkey>,
        device_id: Option<String>,
    ) -> Result<(Session, InviteResponseEnvelope)>
    where
        R: RngCore + CryptoRng,
    {
        self.ensure_accept_allowed(invitee_public_key)?;

        let invitee_session_key = random_secret_key_bytes(ctx.rng)?;
        let invitee_session_public_key =
            crate::device_pubkey_from_secret_bytes(&invitee_session_key)?;

        let session = Session::new_initiator(
            ctx,
            self.inviter_ephemeral_public_key,
            invitee_session_key,
            self.shared_secret,
        )?;

        let payload = InviteResponsePayload {
            session_key: invitee_session_public_key,
            owner_pubkey: invitee_owner_pubkey,
            device_id,
        };

        let invitee_sk = secret_key_from_bytes(&invitee_private_key)?;
        let dh_encrypted = nip44::encrypt(
            &invitee_sk,
            &self.inviter_device_pubkey.to_nostr()?,
            serde_json::to_string(&payload)?,
            Version::V2,
        )?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let encrypted_bytes =
            nip44::v2::encrypt_to_bytes(&conversation_key, dh_encrypted.as_bytes())?;
        let inner_event = InviteResponseInnerEvent {
            pubkey: invitee_public_key,
            content: base64::engine::general_purpose::STANDARD.encode(encrypted_bytes),
            created_at: ctx.now,
        };

        let random_sender_secret = random_secret_key_bytes(ctx.rng)?;
        let random_sender_pubkey = crate::device_pubkey_from_secret_bytes(&random_sender_secret)?;
        let envelope_content = nip44::encrypt(
            &secret_key_from_bytes(&random_sender_secret)?,
            &self.inviter_ephemeral_public_key.to_nostr()?,
            serde_json::to_string(&inner_event)?,
            Version::V2,
        )?;

        let jitter = if ctx.now.get() == 0 {
            0
        } else {
            ctx.rng.next_u64() % (2 * 24 * 60 * 60)
        };
        let created_at = UnixSeconds(ctx.now.get().saturating_sub(jitter));

        Ok((
            session,
            InviteResponseEnvelope {
                sender: random_sender_pubkey,
                signer_secret_key: random_sender_secret,
                recipient: self.inviter_ephemeral_public_key,
                created_at,
                content: envelope_content,
            },
        ))
    }

    pub fn process_response<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        envelope: &InviteResponseEnvelope,
        inviter_private_key: [u8; 32],
    ) -> Result<InviteResponse>
    where
        R: RngCore + CryptoRng,
    {
        let inviter_ephemeral_private_key = self
            .inviter_ephemeral_private_key
            .ok_or_else(|| crate::Error::Parse("ephemeral key not available".to_string()))?;

        let inviter_ephemeral_sk = secret_key_from_bytes(&inviter_ephemeral_private_key)?;
        let decrypted = nip44::decrypt(
            &inviter_ephemeral_sk,
            &envelope.sender.to_nostr()?,
            &envelope.content,
        )?;
        let inner_event: InviteResponseInnerEvent = serde_json::from_str(&decrypted)?;

        let ciphertext_bytes = base64::engine::general_purpose::STANDARD
            .decode(inner_event.content.as_bytes())
            .map_err(|e| crate::Error::Decryption(e.to_string()))?;
        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let dh_encrypted_ciphertext = String::from_utf8(nip44::v2::decrypt_to_bytes(
            &conversation_key,
            &ciphertext_bytes,
        )?)
        .map_err(|e| crate::Error::Decryption(e.to_string()))?;

        let inviter_sk = secret_key_from_bytes(&inviter_private_key)?;
        let dh_decrypted = nip44::decrypt(
            &inviter_sk,
            &inner_event.pubkey.to_nostr()?,
            &dh_encrypted_ciphertext,
        )?;

        let payload: InviteResponsePayload = serde_json::from_str(&dh_decrypted)?;
        self.ensure_accept_allowed(inner_event.pubkey)?;
        let session = Session::new_responder(
            ctx,
            payload.session_key,
            inviter_ephemeral_private_key,
            self.shared_secret,
        )?;
        self.record_use(inner_event.pubkey);

        Ok(InviteResponse {
            session,
            invitee_device_pubkey: inner_event.pubkey,
            invitee_owner_pubkey: payload.owner_pubkey,
            invitee_identity: inner_event.pubkey.to_nostr()?,
            device_id: payload.device_id,
            owner_public_key: payload.owner_pubkey.map(|owner| {
                PublicKey::from_slice(&owner.to_bytes()).expect("owner pubkey bytes must be valid")
            }),
        })
    }

    pub fn process_invite_response(
        &self,
        event: &nostr::Event,
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>> {
        let envelope = crate::nostr_codec::parse_invite_response_event(event)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?;
        let mut rng = OsRng;
        let mut ctx = ProtocolContext::new(UnixSeconds(event.created_at.as_secs()), &mut rng);
        let mut invite = self.clone();
        invite
            .process_response(&mut ctx, &envelope, inviter_private_key)
            .map(Some)
    }

    pub fn listen_with_pubsub(&self, pubsub: &dyn crate::NostrPubSub) -> Result<String> {
        let filter = crate::pubsub::build_filter()
            .kinds(vec![INVITE_RESPONSE_KIND as u64])
            .pubkeys(vec![self.inviter_ephemeral_public_key.to_nostr()?])
            .build();
        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-response-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn listen(
        &self,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = self.listen_with_pubsub(event_tx)?;
        Ok(())
    }

    pub fn from_user_with_pubsub(
        user_pubkey: PublicKey,
        pubsub: &dyn crate::NostrPubSub,
    ) -> Result<String> {
        let filter = nostr::Filter::new()
            .kind(Kind::from(INVITE_EVENT_KIND as u16))
            .authors(vec![user_pubkey])
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::L),
                "double-ratchet/invites",
            );

        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-user-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn from_user(
        user_pubkey: PublicKey,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = Self::from_user_with_pubsub(user_pubkey, event_tx)?;
        Ok(())
    }

    fn ensure_accept_allowed(&self, invitee_public_key: DevicePubkey) -> Result<()> {
        if self.used_by.contains(&invitee_public_key) {
            return Err(DomainError::InviteAlreadyUsed.into());
        }
        if self
            .max_uses
            .is_some_and(|max_uses| self.used_by.len() >= max_uses)
        {
            return Err(DomainError::InviteExhausted.into());
        }
        Ok(())
    }

    fn record_use(&mut self, invitee_public_key: DevicePubkey) {
        if self.used_by.contains(&invitee_public_key) {
            return;
        }
        self.used_by.push(invitee_public_key);
        self.used_by.sort();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InviteResponseInnerEvent {
    pubkey: DevicePubkey,
    content: String,
    created_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InviteResponsePayload {
    #[serde(rename = "sessionKey")]
    session_key: DevicePubkey,
    #[serde(rename = "deviceId", skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    #[serde(rename = "ownerPublicKey", skip_serializing_if = "Option::is_none")]
    owner_pubkey: Option<OwnerPubkey>,
}

fn now_seconds() -> UnixSeconds {
    UnixSeconds(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
}

mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        super::decode_hex_32(&s).map_err(serde::de::Error::custom)
    }
}

mod serde_option_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match bytes {
            Some(b) => serializer.serialize_str(&hex::encode(b)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => super::decode_hex_32(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

fn decode_hex_32(value: &str) -> std::result::Result<[u8; 32], String> {
    let bytes = hex::decode(value).map_err(|e| e.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "invalid 32-byte hex".to_string())
}
