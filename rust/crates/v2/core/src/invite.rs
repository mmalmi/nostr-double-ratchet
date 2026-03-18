use crate::{
    Error, InviteId, Result, SessionInitInput, SessionReceiveMeta, SessionState, SessionId,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::PublicKey;
use nostr::{Event, EventBuilder, Keys, SecretKey, Tag, Timestamp};
use serde::{Deserialize, Serialize};

pub const INVITE_RESPONSE_KIND: u32 = 1059;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InviteState {
    pub invite_id: Option<InviteId>,
    pub inviter_ephemeral_public_key: PublicKey,
    pub shared_secret: [u8; 32],
    pub inviter: PublicKey,
    pub inviter_ephemeral_private_key: Option<[u8; 32]>,
    pub device_id: Option<String>,
    pub max_uses: Option<usize>,
    pub used_by: Vec<PublicKey>,
    pub created_at: u64,
    pub purpose: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

#[derive(Debug, Clone)]
pub struct InviteCreateInput {
    pub invite_id: Option<InviteId>,
    pub inviter: PublicKey,
    pub inviter_ephemeral_private_key: [u8; 32],
    pub shared_secret: [u8; 32],
    pub created_at: u64,
    pub device_id: Option<String>,
    pub max_uses: Option<usize>,
    pub purpose: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

#[derive(Debug, Clone)]
pub struct InviteAcceptInput {
    pub invitee_public_key: PublicKey,
    pub invitee_identity_private_key: [u8; 32],
    pub invitee_session_private_key: [u8; 32],
    pub invitee_next_nostr_private_key: [u8; 32],
    pub envelope_sender_private_key: [u8; 32],
    pub response_created_at: u64,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone)]
pub struct InviteAcceptResult {
    pub next_invite: InviteState,
    pub session: SessionState,
    pub response_event: Event,
}

#[derive(Debug, Clone)]
pub struct InviteProcessResponseInput {
    pub event: Event,
    pub inviter_identity_private_key: [u8; 32],
    pub inviter_next_nostr_private_key: [u8; 32],
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone)]
pub struct InviteResponseMeta {
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
    pub session_meta: Option<SessionReceiveMeta>,
}

#[derive(Debug, Clone)]
pub enum InviteProcessResponseResult {
    NotForThisInvite { next: InviteState },
    Accepted {
        next_invite: InviteState,
        session: SessionState,
        meta: InviteResponseMeta,
    },
    InvalidRelevant {
        next: InviteState,
        error: Error,
    },
}

impl InviteState {
    pub fn create(input: InviteCreateInput) -> Result<Self> {
        let inviter_ephemeral_public_key = nostr::Keys::new(
            nostr::SecretKey::from_slice(&input.inviter_ephemeral_private_key)
                .map_err(|e| Error::Invite(e.to_string()))?,
        )
        .public_key();

        Ok(Self {
            invite_id: input.invite_id,
            inviter_ephemeral_public_key,
            shared_secret: input.shared_secret,
            inviter: input.inviter,
            inviter_ephemeral_private_key: Some(input.inviter_ephemeral_private_key),
            device_id: input.device_id,
            max_uses: input.max_uses,
            used_by: Vec::new(),
            created_at: input.created_at,
            purpose: input.purpose,
            owner_public_key: input.owner_public_key,
        })
    }

    pub fn accept(&self, _input: InviteAcceptInput) -> Result<InviteAcceptResult> {
        let input = _input;
        if let Some(max_uses) = self.max_uses {
            if self.used_by.len() >= max_uses {
                return Err(Error::Invite("invite has reached max uses".to_string()));
            }
        }

        let invitee_session_keys = Keys::new(
            SecretKey::from_slice(&input.invitee_session_private_key)
                .map_err(|e| Error::Invite(e.to_string()))?,
        );
        let invitee_session_public_key = invitee_session_keys.public_key();

        let session = SessionState::init(SessionInitInput {
            session_id: input.session_id,
            their_ephemeral_nostr_public_key: self.inviter_ephemeral_public_key,
            our_ephemeral_nostr_private_key: input.invitee_session_private_key,
            our_next_nostr_private_key: input.invitee_next_nostr_private_key,
            is_initiator: true,
            shared_secret: self.shared_secret,
        })?;

        let mut payload = serde_json::Map::new();
        payload.insert(
            "sessionKey".to_string(),
            serde_json::Value::String(invitee_session_public_key.to_hex()),
        );
        if let Some(device_id) = input.device_id.clone() {
            payload.insert("deviceId".to_string(), serde_json::Value::String(device_id));
        }
        if let Some(owner_public_key) = input.owner_public_key {
            payload.insert(
                "ownerPublicKey".to_string(),
                serde_json::Value::String(owner_public_key.to_hex()),
            );
        }
        let payload_json = serde_json::Value::Object(payload).to_string();

        let invitee_identity_secret = SecretKey::from_slice(&input.invitee_identity_private_key)
            .map_err(|e| Error::Invite(e.to_string()))?;
        let dh_encrypted = nip44::encrypt(
            &invitee_identity_secret,
            &self.inviter,
            payload_json,
            Version::V2,
        )
        .map_err(|e| Error::Decryption(e.to_string()))?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)
            .map_err(|e| Error::Decryption(e.to_string()))?;
        let inner_encrypted = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);
        let inner_event = serde_json::json!({
            "pubkey": input.invitee_public_key.to_hex(),
            "content": inner_encrypted,
            "created_at": input.response_created_at,
        });

        let envelope_keys = Keys::new(
            SecretKey::from_slice(&input.envelope_sender_private_key)
                .map_err(|e| Error::Invite(e.to_string()))?,
        );
        let envelope_content = nip44::encrypt(
            envelope_keys.secret_key(),
            &self.inviter_ephemeral_public_key,
            inner_event.to_string(),
            Version::V2,
        )
        .map_err(|e| Error::Decryption(e.to_string()))?;

        let outer_event = EventBuilder::new(
            nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16),
            envelope_content,
        )
        .tag(
            Tag::parse(&["p".to_string(), self.inviter_ephemeral_public_key.to_hex()])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        )
        .custom_created_at(Timestamp::from(input.response_created_at))
        .build(envelope_keys.public_key())
        .sign_with_keys(&envelope_keys)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;

        let mut next_invite = self.clone();
        if !next_invite.used_by.contains(&input.invitee_public_key) {
            next_invite.used_by.push(input.invitee_public_key);
        }

        Ok(InviteAcceptResult {
            next_invite,
            session,
            response_event: outer_event,
        })
    }

    pub fn process_response(
        &self,
        _input: InviteProcessResponseInput,
    ) -> InviteProcessResponseResult {
        let input = _input;
        if u32::from(input.event.kind.as_u16()) != INVITE_RESPONSE_KIND {
            return InviteProcessResponseResult::NotForThisInvite { next: self.clone() };
        }

        let tagged_pubkey = input
            .event
            .tags
            .iter()
            .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("p"))
            .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()));
        if tagged_pubkey.as_deref() != Some(self.inviter_ephemeral_public_key.to_hex().as_str()) {
            return InviteProcessResponseResult::NotForThisInvite { next: self.clone() };
        }

        let snapshot = self.clone();
        let outcome = (|| -> Result<(InviteState, SessionState, InviteResponseMeta)> {
            let inviter_ephemeral_private_key = self
                .inviter_ephemeral_private_key
                .ok_or_else(|| Error::Invite("missing inviter ephemeral private key".to_string()))?;
            let inviter_ephemeral_secret = SecretKey::from_slice(&inviter_ephemeral_private_key)
                .map_err(|e| Error::Invite(e.to_string()))?;
            let decrypted = nip44::decrypt(&inviter_ephemeral_secret, &input.event.pubkey, &input.event.content)
                .map_err(|e| Error::Decryption(e.to_string()))?;
            let inner_event: serde_json::Value =
                serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;

            let invitee_identity_hex = inner_event
                .get("pubkey")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Invite("missing invitee pubkey".to_string()))?;
            let invitee_identity =
                PublicKey::from_hex(invitee_identity_hex).map_err(|e| Error::Invite(e.to_string()))?;
            let inner_content = inner_event
                .get("content")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Invite("missing inner content".to_string()))?;

            let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(inner_content)
                .map_err(|e| Error::Serialization(e.to_string()))?;
            let dh_encrypted_ciphertext =
                String::from_utf8(nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
                    .map_err(|e| Error::Decryption(e.to_string()))?)
                .map_err(|e| Error::Serialization(e.to_string()))?;

            let inviter_identity_secret = SecretKey::from_slice(&input.inviter_identity_private_key)
                .map_err(|e| Error::Invite(e.to_string()))?;
            let dh_decrypted = nip44::decrypt(
                &inviter_identity_secret,
                &invitee_identity,
                &dh_encrypted_ciphertext,
            )
            .map_err(|e| Error::Decryption(e.to_string()))?;
            let payload: serde_json::Value =
                serde_json::from_str(&dh_decrypted).map_err(|e| Error::Serialization(e.to_string()))?;

            let session_key_hex = payload
                .get("sessionKey")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Invite("missing sessionKey".to_string()))?;
            let invitee_session_pubkey =
                PublicKey::from_hex(session_key_hex).map_err(|e| Error::Invite(e.to_string()))?;
            let device_id = payload
                .get("deviceId")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let owner_public_key = payload
                .get("ownerPublicKey")
                .and_then(|value| value.as_str())
                .map(PublicKey::from_hex)
                .transpose()
                .map_err(|e| Error::Invite(e.to_string()))?;

            let session = SessionState::init(SessionInitInput {
                session_id: input.session_id,
                their_ephemeral_nostr_public_key: invitee_session_pubkey,
                our_ephemeral_nostr_private_key: inviter_ephemeral_private_key,
                our_next_nostr_private_key: input.inviter_next_nostr_private_key,
                is_initiator: false,
                shared_secret: self.shared_secret,
            })?;

            let mut next_invite = self.clone();
            if !next_invite.used_by.contains(&invitee_identity) {
                next_invite.used_by.push(invitee_identity);
            }

            Ok((
                next_invite,
                session,
                InviteResponseMeta {
                    invitee_identity,
                    device_id,
                    owner_public_key,
                    session_meta: None,
                },
            ))
        })();

        match outcome {
            Ok((next_invite, session, meta)) => InviteProcessResponseResult::Accepted {
                next_invite,
                session,
                meta,
            },
            Err(error) => InviteProcessResponseResult::InvalidRelevant {
                next: snapshot,
                error,
            },
        }
    }
}
