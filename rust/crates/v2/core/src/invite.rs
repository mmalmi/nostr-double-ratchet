use crate::session::{SessionError, SessionId, SessionInitInput, SessionState};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::{Event, EventBuilder, Keys, PublicKey, SecretKey, Tag, Timestamp};
use thiserror::Error;

pub const INVITE_RESPONSE_KIND: u32 = 1059;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InviteId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteResponseMeta {
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
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
        error: InviteError,
    },
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum InviteError {
    #[error("failed to decrypt invite payload: {0}")]
    Decryption(String),
    #[error("invalid invite event: {0}")]
    InvalidEvent(String),
    #[error("invite error: {0}")]
    Invite(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("session error: {0}")]
    Session(#[from] SessionError),
}

pub type InviteResult<T> = std::result::Result<T, InviteError>;

impl InviteState {
    pub fn create(input: InviteCreateInput) -> InviteResult<Self> {
        let inviter_ephemeral_public_key = Keys::new(
            SecretKey::from_slice(&input.inviter_ephemeral_private_key)
                .map_err(|e| InviteError::Invite(e.to_string()))?,
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

    pub fn accept(&self, input: InviteAcceptInput) -> InviteResult<InviteAcceptResult> {
        if let Some(max_uses) = self.max_uses {
            if self.used_by.len() >= max_uses {
                return Err(InviteError::Invite(
                    "invite has reached max uses".to_string(),
                ));
            }
        }

        let invitee_session_keys = Keys::new(
            SecretKey::from_slice(&input.invitee_session_private_key)
                .map_err(|e| InviteError::Invite(e.to_string()))?,
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
            .map_err(|e| InviteError::Invite(e.to_string()))?;
        let dh_encrypted = nip44::encrypt(
            &invitee_identity_secret,
            &self.inviter,
            payload_json,
            Version::V2,
        )
        .map_err(|e| InviteError::Decryption(e.to_string()))?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)
            .map_err(|e| InviteError::Decryption(e.to_string()))?;
        let inner_encrypted = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);
        let inner_event = serde_json::json!({
            "pubkey": input.invitee_public_key.to_hex(),
            "content": inner_encrypted,
            "created_at": input.response_created_at,
        });

        let envelope_keys = Keys::new(
            SecretKey::from_slice(&input.envelope_sender_private_key)
                .map_err(|e| InviteError::Invite(e.to_string()))?,
        );
        let envelope_content = nip44::encrypt(
            envelope_keys.secret_key(),
            &self.inviter_ephemeral_public_key,
            inner_event.to_string(),
            Version::V2,
        )
        .map_err(|e| InviteError::Decryption(e.to_string()))?;

        let outer_event = EventBuilder::new(
            nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16),
            envelope_content,
        )
        .tag(
            Tag::parse(&["p".to_string(), self.inviter_ephemeral_public_key.to_hex()])
                .map_err(|e| InviteError::InvalidEvent(e.to_string()))?,
        )
        .custom_created_at(Timestamp::from(input.response_created_at))
        .build(envelope_keys.public_key())
        .sign_with_keys(&envelope_keys)
        .map_err(|e| InviteError::InvalidEvent(e.to_string()))?;

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

    pub fn process_response(&self, input: InviteProcessResponseInput) -> InviteProcessResponseResult {
        if u32::from(input.event.kind.as_u16()) != INVITE_RESPONSE_KIND {
            return InviteProcessResponseResult::NotForThisInvite { next: self.clone() };
        }

        let tagged_pubkey = input
            .event
            .tags
            .iter()
            .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("p"))
            .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()));
        let expected_tag = self.inviter_ephemeral_public_key.to_hex();
        if tagged_pubkey.as_deref() != Some(expected_tag.as_str()) {
            return InviteProcessResponseResult::NotForThisInvite { next: self.clone() };
        }

        let snapshot = self.clone();
        let outcome = (|| -> InviteResult<(InviteState, SessionState, InviteResponseMeta)> {
            let inviter_ephemeral_private_key = self
                .inviter_ephemeral_private_key
                .ok_or_else(|| InviteError::Invite("missing inviter ephemeral private key".to_string()))?;
            let inviter_ephemeral_secret = SecretKey::from_slice(&inviter_ephemeral_private_key)
                .map_err(|e| InviteError::Invite(e.to_string()))?;
            let decrypted = nip44::decrypt(&inviter_ephemeral_secret, &input.event.pubkey, &input.event.content)
                .map_err(|e| InviteError::Decryption(e.to_string()))?;
            let inner_event: serde_json::Value =
                serde_json::from_str(&decrypted).map_err(|e| InviteError::Serialization(e.to_string()))?;

            let invitee_identity_hex = inner_event
                .get("pubkey")
                .and_then(|value| value.as_str())
                .ok_or_else(|| InviteError::Invite("missing invitee pubkey".to_string()))?;
            let invitee_identity =
                PublicKey::from_hex(invitee_identity_hex).map_err(|e| InviteError::Invite(e.to_string()))?;
            let inner_content = inner_event
                .get("content")
                .and_then(|value| value.as_str())
                .ok_or_else(|| InviteError::Invite("missing inner content".to_string()))?;

            let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(inner_content)
                .map_err(|e| InviteError::Serialization(e.to_string()))?;
            let dh_encrypted_ciphertext = String::from_utf8(
                nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
                    .map_err(|e| InviteError::Decryption(e.to_string()))?,
            )
            .map_err(|e| InviteError::Serialization(e.to_string()))?;

            let inviter_identity_secret = SecretKey::from_slice(&input.inviter_identity_private_key)
                .map_err(|e| InviteError::Invite(e.to_string()))?;
            let dh_decrypted = nip44::decrypt(
                &inviter_identity_secret,
                &invitee_identity,
                &dh_encrypted_ciphertext,
            )
            .map_err(|e| InviteError::Decryption(e.to_string()))?;

            let (invitee_session_pubkey, device_id, owner_public_key) =
                decode_response_payload(&dh_decrypted)?;

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

fn decode_response_payload(
    decrypted_payload: &str,
) -> InviteResult<(PublicKey, Option<String>, Option<PublicKey>)> {
    let payload: serde_json::Value = match serde_json::from_str(decrypted_payload) {
        Ok(payload) => payload,
        Err(_) => {
            let legacy_session_pubkey = PublicKey::from_hex(decrypted_payload)
                .map_err(|e| InviteError::Invite(e.to_string()))?;
            return Ok((legacy_session_pubkey, None, None));
        }
    };

    let session_key_hex = payload
        .get("sessionKey")
        .and_then(|value| value.as_str())
        .ok_or_else(|| InviteError::Invite("missing sessionKey".to_string()))?;
    let invitee_session_pubkey =
        PublicKey::from_hex(session_key_hex).map_err(|e| InviteError::Invite(e.to_string()))?;
    let device_id = payload
        .get("deviceId")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let owner_public_key = payload
        .get("ownerPublicKey")
        .and_then(|value| value.as_str())
        .map(PublicKey::from_hex)
        .transpose()
        .map_err(|e| InviteError::Invite(e.to_string()))?;

    Ok((invitee_session_pubkey, device_id, owner_public_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(byte: u8) -> (SecretKey, PublicKey) {
        let bytes = [byte; 32];
        let sk = SecretKey::from_slice(&bytes).unwrap();
        let pk = Keys::new(sk.clone()).public_key();
        (sk, pk)
    }

    fn key_bytes(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn invite_accept_returns_session_and_response() {
        let (_, inviter_pk) = keypair(40);
        let invite = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_pk,
            inviter_ephemeral_private_key: [41u8; 32],
            shared_secret: [42u8; 32],
            created_at: 100,
            device_id: Some("inviter-device".to_string()),
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, invitee_pk) = keypair(44);

        let accepted = invite
            .accept(InviteAcceptInput {
                invitee_public_key: invitee_pk,
                invitee_identity_private_key: key_bytes(44),
                invitee_session_private_key: key_bytes(45),
                invitee_next_nostr_private_key: key_bytes(46),
                envelope_sender_private_key: key_bytes(47),
                response_created_at: 101,
                device_id: Some("invitee-device".to_string()),
                owner_public_key: Some(invitee_pk),
                session_id: None,
            })
            .unwrap();

        assert_eq!(u32::from(accepted.response_event.kind.as_u16()), INVITE_RESPONSE_KIND);
        assert!(accepted.session.can_send());
    }

    #[test]
    fn invite_accept_enforces_max_uses() {
        let (_, inviter_pk) = keypair(48);
        let invite = InviteState {
            invite_id: None,
            inviter_ephemeral_public_key: Keys::new(SecretKey::from_slice(&key_bytes(49)).unwrap()).public_key(),
            shared_secret: key_bytes(50),
            inviter: inviter_pk,
            inviter_ephemeral_private_key: Some(key_bytes(49)),
            device_id: None,
            max_uses: Some(1),
            used_by: vec![keypair(51).1],
            created_at: 1,
            purpose: None,
            owner_public_key: None,
        };

        let result = invite.accept(InviteAcceptInput {
            invitee_public_key: keypair(52).1,
            invitee_identity_private_key: key_bytes(52),
            invitee_session_private_key: key_bytes(53),
            invitee_next_nostr_private_key: key_bytes(54),
            envelope_sender_private_key: key_bytes(55),
            response_created_at: 2,
            device_id: None,
            owner_public_key: None,
            session_id: None,
        });

        assert!(matches!(result, Err(InviteError::Invite(_))));
    }

    #[test]
    fn invite_process_matching_response_returns_accepted() {
        let (inviter_sk, inviter_pk) = keypair(60);
        let invite = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_pk,
            inviter_ephemeral_private_key: key_bytes(61),
            shared_secret: key_bytes(62),
            created_at: 200,
            device_id: Some("inviter".to_string()),
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, invitee_pk) = keypair(64);
        let accepted = invite
            .accept(InviteAcceptInput {
                invitee_public_key: invitee_pk,
                invitee_identity_private_key: key_bytes(64),
                invitee_session_private_key: key_bytes(65),
                invitee_next_nostr_private_key: key_bytes(66),
                envelope_sender_private_key: key_bytes(67),
                response_created_at: 201,
                device_id: Some("invitee".to_string()),
                owner_public_key: Some(invitee_pk),
                session_id: None,
            })
            .unwrap();

        let processed = invite.process_response(InviteProcessResponseInput {
            event: accepted.response_event,
            inviter_identity_private_key: inviter_sk.to_secret_bytes(),
            inviter_next_nostr_private_key: key_bytes(68),
            session_id: None,
        });

        match processed {
            InviteProcessResponseResult::Accepted { meta, .. } => {
                assert_eq!(meta.invitee_identity, invitee_pk);
                assert_eq!(meta.owner_public_key, Some(invitee_pk));
            }
            other => panic!("expected accepted invite response, got {other:?}"),
        }
    }

    #[test]
    fn invite_process_nonmatching_response_returns_not_for_this_invite() {
        let (_, inviter_a) = keypair(70);
        let invite_a = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_a,
            inviter_ephemeral_private_key: key_bytes(71),
            shared_secret: key_bytes(72),
            created_at: 300,
            device_id: Some("a".to_string()),
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, inviter_b) = keypair(73);
        let invite_b = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_b,
            inviter_ephemeral_private_key: key_bytes(74),
            shared_secret: key_bytes(75),
            created_at: 301,
            device_id: Some("b".to_string()),
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, invitee_pk) = keypair(76);
        let accepted = invite_b
            .accept(InviteAcceptInput {
                invitee_public_key: invitee_pk,
                invitee_identity_private_key: key_bytes(76),
                invitee_session_private_key: key_bytes(77),
                invitee_next_nostr_private_key: key_bytes(78),
                envelope_sender_private_key: key_bytes(79),
                response_created_at: 302,
                device_id: None,
                owner_public_key: None,
                session_id: None,
            })
            .unwrap();

        let processed = invite_a.process_response(InviteProcessResponseInput {
            event: accepted.response_event,
            inviter_identity_private_key: key_bytes(70),
            inviter_next_nostr_private_key: key_bytes(80),
            session_id: None,
        });

        assert!(matches!(
            processed,
            InviteProcessResponseResult::NotForThisInvite { .. }
        ));
    }

    #[test]
    fn invite_process_invalid_relevant_response_returns_invalid_relevant() {
        let (inviter_sk, inviter_pk) = keypair(81);
        let invite = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_pk,
            inviter_ephemeral_private_key: key_bytes(82),
            shared_secret: key_bytes(83),
            created_at: 400,
            device_id: Some("inviter".to_string()),
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, invitee_pk) = keypair(84);
        let mut accepted = invite
            .accept(InviteAcceptInput {
                invitee_public_key: invitee_pk,
                invitee_identity_private_key: key_bytes(84),
                invitee_session_private_key: key_bytes(85),
                invitee_next_nostr_private_key: key_bytes(86),
                envelope_sender_private_key: key_bytes(87),
                response_created_at: 401,
                device_id: None,
                owner_public_key: None,
                session_id: None,
            })
            .unwrap()
            .response_event;
        accepted.content = "tampered".to_string();

        let processed = invite.process_response(InviteProcessResponseInput {
            event: accepted,
            inviter_identity_private_key: inviter_sk.to_secret_bytes(),
            inviter_next_nostr_private_key: key_bytes(88),
            session_id: None,
        });

        match processed {
            InviteProcessResponseResult::InvalidRelevant { next, .. } => assert_eq!(next, invite),
            other => panic!("expected invalid relevant result, got {other:?}"),
        }
    }

    #[test]
    fn process_response_accepts_legacy_raw_session_key_payload() {
        let (inviter_identity_sk, inviter_identity_pk) = keypair(90);
        let invite = InviteState::create(InviteCreateInput {
            invite_id: None,
            inviter: inviter_identity_pk,
            inviter_ephemeral_private_key: key_bytes(91),
            shared_secret: key_bytes(92),
            created_at: 10,
            device_id: None,
            max_uses: None,
            purpose: None,
            owner_public_key: None,
        })
        .unwrap();
        let (_, invitee_identity_pk) = keypair(93);
        let invitee_identity_sk = SecretKey::from_slice(&key_bytes(93)).unwrap();
        let invitee_session_keys = Keys::new(SecretKey::from_slice(&key_bytes(94)).unwrap());

        let dh_encrypted = nip44::encrypt(
            &invitee_identity_sk,
            &inviter_identity_pk,
            invitee_session_keys.public_key().to_hex(),
            Version::V2,
        )
        .unwrap();
        let conversation_key = nip44::v2::ConversationKey::new(invite.shared_secret);
        let inner_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted).unwrap();
        let inner_event = serde_json::json!({
            "pubkey": invitee_identity_pk.to_hex(),
            "content": base64::engine::general_purpose::STANDARD.encode(inner_bytes),
            "created_at": 11_u64,
        });

        let envelope_keys = Keys::new(SecretKey::from_slice(&key_bytes(95)).unwrap());
        let envelope_content = nip44::encrypt(
            envelope_keys.secret_key(),
            &invite.inviter_ephemeral_public_key,
            inner_event.to_string(),
            Version::V2,
        )
        .unwrap();
        let event = EventBuilder::new(
            nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16),
            envelope_content,
        )
        .tag(Tag::parse(&["p".to_string(), invite.inviter_ephemeral_public_key.to_hex()]).unwrap())
        .custom_created_at(Timestamp::from(11_u64))
        .build(envelope_keys.public_key())
        .sign_with_keys(&envelope_keys)
        .unwrap();

        let processed = invite.process_response(InviteProcessResponseInput {
            event,
            inviter_identity_private_key: inviter_identity_sk.to_secret_bytes(),
            inviter_next_nostr_private_key: key_bytes(96),
            session_id: None,
        });

        match processed {
            InviteProcessResponseResult::Accepted { meta, .. } => {
                assert_eq!(meta.invitee_identity, invitee_identity_pk);
            }
            other => panic!("expected accepted result, got {other:?}"),
        }
    }
}
