use base64::Engine;
use hkdf::Hkdf;
use nostr::nips::nip44::{self, Version};
use nostr::{Event, EventBuilder, EventId, Keys, PublicKey, SecretKey, Tag, Timestamp, UnsignedEvent};
use sha2::Sha256;
use std::collections::HashMap;
use thiserror::Error;

pub const MESSAGE_EVENT_KIND: u32 = 1060;
pub const MAX_SKIP: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    number: u32,
    previous_chain_length: u32,
    next_public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializableKeyPair {
    pub public_key: PublicKey,
    pub private_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedKeysEntry {
    pub header_keys: Vec<[u8; 32]>,
    pub message_keys: HashMap<u32, [u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub session_id: Option<SessionId>,
    pub root_key: [u8; 32],
    pub their_current_nostr_public_key: Option<PublicKey>,
    pub their_next_nostr_public_key: Option<PublicKey>,
    pub our_current_nostr_key: Option<SerializableKeyPair>,
    pub our_next_nostr_key: SerializableKeyPair,
    pub receiving_chain_key: Option<[u8; 32]>,
    pub sending_chain_key: Option<[u8; 32]>,
    pub sending_chain_message_number: u32,
    pub receiving_chain_message_number: u32,
    pub previous_sending_chain_message_count: u32,
    pub skipped_keys: HashMap<PublicKey, SkippedKeysEntry>,
}

#[derive(Debug, Clone)]
pub struct SessionInitInput {
    pub session_id: Option<SessionId>,
    pub their_ephemeral_nostr_public_key: PublicKey,
    pub our_ephemeral_nostr_private_key: [u8; 32],
    pub our_next_nostr_private_key: [u8; 32],
    pub is_initiator: bool,
    pub shared_secret: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SessionSendInput {
    pub inner_event: UnsignedEvent,
    pub now_secs: u64,
    pub now_ms: u64,
}

#[derive(Debug, Clone)]
pub struct SessionSendResult {
    pub next: SessionState,
    pub outer_event: Event,
    pub inner_event: UnsignedEvent,
}

#[derive(Debug, Clone)]
pub struct SessionReceiveInput {
    pub outer_event: Event,
    pub replacement_next_nostr_private_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReceiveMeta {
    pub sender: PublicKey,
    pub outer_event_id: EventId,
}

#[derive(Debug, Clone)]
pub enum SessionReceiveResult {
    NotForThisSession { next: SessionState },
    Decrypted {
        next: SessionState,
        inner_event: UnsignedEvent,
        meta: SessionReceiveMeta,
    },
    InvalidRelevant {
        next: SessionState,
        error: SessionError,
    },
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("failed to decrypt session payload: {0}")]
    Decryption(String),
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("missing or invalid session header")]
    InvalidHeader,
    #[error("session is not ready")]
    SessionNotReady,
    #[error("session is not the initiator")]
    NotInitiator,
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("too many skipped messages")]
    TooManySkippedMessages,
}

pub type SessionResult<T> = std::result::Result<T, SessionError>;

impl SessionState {
    pub fn init(input: SessionInitInput) -> SessionResult<Self> {
        let our_keys = Keys::new(
            SecretKey::from_slice(&input.our_ephemeral_nostr_private_key)
                .map_err(|e| SessionError::InvalidEvent(e.to_string()))?,
        );
        let our_current_nostr_key = if input.is_initiator {
            Some(SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: input.our_ephemeral_nostr_private_key,
            })
        } else {
            None
        };

        let our_next_nostr_key = if input.is_initiator {
            let next_keys = Keys::new(
                SecretKey::from_slice(&input.our_next_nostr_private_key)
                    .map_err(|e| SessionError::InvalidEvent(e.to_string()))?,
            );
            SerializableKeyPair {
                public_key: next_keys.public_key(),
                private_key: input.our_next_nostr_private_key,
            }
        } else {
            SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: input.our_ephemeral_nostr_private_key,
            }
        };

        let (root_key, sending_chain_key) = if input.is_initiator {
            let next_sk = SecretKey::from_slice(&our_next_nostr_key.private_key)
                .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
            let conversation_key =
                nip44::v2::ConversationKey::derive(&next_sk, &input.their_ephemeral_nostr_public_key);
            let outputs = kdf(&input.shared_secret, conversation_key.as_bytes(), 2);
            (outputs[0], Some(outputs[1]))
        } else {
            (input.shared_secret, None)
        };

        Ok(Self {
            session_id: input.session_id,
            root_key,
            their_current_nostr_public_key: None,
            their_next_nostr_public_key: Some(input.their_ephemeral_nostr_public_key),
            our_current_nostr_key,
            our_next_nostr_key,
            receiving_chain_key: None,
            sending_chain_key,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        })
    }

    pub fn can_send(&self) -> bool {
        self.their_next_nostr_public_key.is_some() && self.our_current_nostr_key.is_some()
    }

    pub fn send_event(&self, input: SessionSendInput) -> SessionResult<SessionSendResult> {
        if !self.can_send() {
            return Err(SessionError::NotInitiator);
        }

        let mut next = self.clone();
        let inner_event = normalize_inner_event(input.inner_event, input.now_ms)?;
        let rumor_json = serde_json::to_string(&inner_event)
            .map_err(|e| SessionError::Serialization(e.to_string()))?;
        let (header, encrypted_data) = ratchet_encrypt(&mut next, &rumor_json)?;

        let our_current = next
            .our_current_nostr_key
            .as_ref()
            .ok_or(SessionError::NotInitiator)?;
        let their_next = next
            .their_next_nostr_public_key
            .ok_or(SessionError::SessionNotReady)?;

        let our_secret = SecretKey::from_slice(&our_current.private_key)
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
        let encrypted_header = nip44::encrypt(
            &our_secret,
            &their_next,
            encode_header(&header)?,
            Version::V2,
        )
        .map_err(|e| SessionError::Decryption(e.to_string()))?;

        let header_tag = Tag::parse(&["header".to_string(), encrypted_header])
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
        let unsigned_event = EventBuilder::new(
            nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16),
            encrypted_data,
        )
        .tags(vec![header_tag])
        .custom_created_at(Timestamp::from(input.now_secs))
        .build(our_current.public_key);

        let author_keys = Keys::new(our_secret);
        let outer_event = unsigned_event
            .sign_with_keys(&author_keys)
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;

        Ok(SessionSendResult {
            next,
            outer_event,
            inner_event,
        })
    }

    pub fn receive_event(&self, input: SessionReceiveInput) -> SessionReceiveResult {
        let sender = input.outer_event.pubkey;
        let expected_sender = self
            .their_next_nostr_public_key
            .is_some_and(|pk| pk == sender)
            || self
                .their_current_nostr_public_key
                .is_some_and(|pk| pk == sender);

        if u32::from(input.outer_event.kind.as_u16()) != MESSAGE_EVENT_KIND || !expected_sender {
            return SessionReceiveResult::NotForThisSession { next: self.clone() };
        }

        let snapshot = self.clone();
        let mut next = self.clone();
        let outcome = (|| -> SessionResult<(SessionState, UnsignedEvent, SessionReceiveMeta)> {
            let encrypted_header = input
                .outer_event
                .tags
                .iter()
                .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("header"))
                .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()))
                .ok_or(SessionError::InvalidHeader)?;

            let (header, should_ratchet) = decrypt_header(&next, &encrypted_header, &sender)?;
            let their_next_hex = next
                .their_next_nostr_public_key
                .map(|pk| hex::encode(pk.to_bytes()))
                .unwrap_or_default();
            if header.next_public_key != their_next_hex {
                next.their_current_nostr_public_key = next.their_next_nostr_public_key;
                next.their_next_nostr_public_key = Some(pubkey_from_hex(&header.next_public_key)?);
            }

            if should_ratchet {
                if next.receiving_chain_key.is_some() {
                    skip_message_keys(&mut next, header.previous_chain_length, &sender)?;
                }
                ratchet_step(&mut next, input.replacement_next_nostr_private_key)?;
            }

            let plaintext = ratchet_decrypt(&mut next, &header, &input.outer_event.content, &sender)?;
            let mut inner_event: UnsignedEvent = serde_json::from_str(&plaintext)
                .map_err(|e| SessionError::Serialization(e.to_string()))?;
            inner_event.id = None;
            inner_event.ensure_id();

            Ok((
                next,
                inner_event,
                SessionReceiveMeta {
                    sender,
                    outer_event_id: input.outer_event.id,
                },
            ))
        })();

        match outcome {
            Ok((next, inner_event, meta)) => SessionReceiveResult::Decrypted {
                next,
                inner_event,
                meta,
            },
            Err(error) => SessionReceiveResult::InvalidRelevant {
                next: snapshot,
                error,
            },
        }
    }
}

fn kdf(input1: &[u8], input2: &[u8], num_outputs: usize) -> Vec<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(input2), input1);
    let mut outputs = Vec::with_capacity(num_outputs);
    for i in 1..=num_outputs {
        let mut okm = [0u8; 32];
        hk.expand(&[i as u8], &mut okm)
            .expect("32 bytes is a valid HKDF output length");
        outputs.push(okm);
    }
    outputs
}

fn normalize_inner_event(mut event: UnsignedEvent, now_ms: u64) -> SessionResult<UnsignedEvent> {
    let has_ms_tag = event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("ms"));
    if !has_ms_tag {
        let ms_tag = Tag::parse(&["ms".to_string(), now_ms.to_string()])
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
        let mut builder = EventBuilder::new(event.kind, &event.content);
        for tag in event.tags.iter() {
            builder = builder.tag(tag.clone());
        }
        builder = builder.tag(ms_tag);
        event = builder.custom_created_at(event.created_at).build(event.pubkey);
    }
    event.id = None;
    event.ensure_id();
    Ok(event)
}

fn ratchet_encrypt(state: &mut SessionState, plaintext: &str) -> SessionResult<(Header, String)> {
    let sending_chain_key = state
        .sending_chain_key
        .ok_or(SessionError::SessionNotReady)?;
    let outputs = kdf(&sending_chain_key, &[1u8], 2);
    state.sending_chain_key = Some(outputs[0]);
    let message_key = outputs[1];

    let header = Header {
        number: state.sending_chain_message_number,
        previous_chain_length: state.previous_sending_chain_message_count,
        next_public_key: hex::encode(state.our_next_nostr_key.public_key.to_bytes()),
    };
    state.sending_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(message_key);
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext)
        .map_err(|e| SessionError::Decryption(e.to_string()))?;
    Ok((
        header,
        base64::engine::general_purpose::STANDARD.encode(encrypted_bytes),
    ))
}

fn decrypt_header(
    state: &SessionState,
    encrypted_header: &str,
    sender: &PublicKey,
) -> SessionResult<(Header, bool)> {
    if let Some(current) = &state.our_current_nostr_key {
        let current_secret = SecretKey::from_slice(&current.private_key)
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
        if let Ok(decrypted) = nip44::decrypt(&current_secret, sender, encrypted_header) {
            let header = decode_header(&decrypted)?;
            return Ok((header, false));
        }
    }

    let next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)
        .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
    let decrypted = nip44::decrypt(&next_secret, sender, encrypted_header)
        .map_err(|e| SessionError::Decryption(e.to_string()))?;
    let header = decode_header(&decrypted)?;
    Ok((header, true))
}

fn ratchet_step(
    state: &mut SessionState,
    replacement_next_private_key: [u8; 32],
) -> SessionResult<()> {
    state.previous_sending_chain_message_count = state.sending_chain_message_number;
    state.sending_chain_message_number = 0;
    state.receiving_chain_message_number = 0;

    let our_next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)
        .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
    let their_next = state
        .their_next_nostr_public_key
        .ok_or(SessionError::SessionNotReady)?;

    let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_secret, &their_next);
    let outputs = kdf(&state.root_key, conversation_key1.as_bytes(), 2);
    state.receiving_chain_key = Some(outputs[1]);
    state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

    let replacement_keys = Keys::new(
        SecretKey::from_slice(&replacement_next_private_key)
            .map_err(|e| SessionError::InvalidEvent(e.to_string()))?,
    );
    state.our_next_nostr_key = SerializableKeyPair {
        public_key: replacement_keys.public_key(),
        private_key: replacement_next_private_key,
    };

    let replacement_secret = SecretKey::from_slice(&replacement_next_private_key)
        .map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
    let conversation_key2 = nip44::v2::ConversationKey::derive(&replacement_secret, &their_next);
    let outputs2 = kdf(&outputs[0], conversation_key2.as_bytes(), 2);
    state.root_key = outputs2[0];
    state.sending_chain_key = Some(outputs2[1]);
    Ok(())
}

fn ratchet_decrypt(
    state: &mut SessionState,
    header: &Header,
    ciphertext: &str,
    sender: &PublicKey,
) -> SessionResult<String> {
    if let Some(plaintext) = try_skipped_message_keys(state, header, ciphertext, sender)? {
        return Ok(plaintext);
    }

    if state.receiving_chain_key.is_none() {
        return Err(SessionError::SessionNotReady);
    }

    skip_message_keys(state, header.number, sender)?;
    let receiving_chain_key = state
        .receiving_chain_key
        .ok_or(SessionError::SessionNotReady)?;
    let outputs = kdf(&receiving_chain_key, &[1u8], 2);
    state.receiving_chain_key = Some(outputs[0]);
    state.receiving_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(outputs[1]);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| SessionError::Decryption(e.to_string()))?;
    let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
        .map_err(|e| SessionError::Decryption(e.to_string()))?;
    String::from_utf8(plaintext_bytes).map_err(|e| SessionError::Decryption(e.to_string()))
}

fn skip_message_keys(
    state: &mut SessionState,
    until: u32,
    sender: &PublicKey,
) -> SessionResult<()> {
    if until <= state.receiving_chain_message_number {
        return Ok(());
    }
    if (until - state.receiving_chain_message_number) as usize > MAX_SKIP {
        return Err(SessionError::TooManySkippedMessages);
    }

    let entry = state
        .skipped_keys
        .entry(*sender)
        .or_insert_with(|| SkippedKeysEntry {
            header_keys: Vec::new(),
            message_keys: HashMap::new(),
        });

    while state.receiving_chain_message_number < until {
        let receiving_chain_key = state
            .receiving_chain_key
            .ok_or(SessionError::SessionNotReady)?;
        let outputs = kdf(&receiving_chain_key, &[1u8], 2);
        state.receiving_chain_key = Some(outputs[0]);
        entry
            .message_keys
            .insert(state.receiving_chain_message_number, outputs[1]);
        state.receiving_chain_message_number += 1;
    }
    prune_skipped_message_keys(&mut entry.message_keys);
    Ok(())
}

fn try_skipped_message_keys(
    state: &mut SessionState,
    header: &Header,
    ciphertext: &str,
    sender: &PublicKey,
) -> SessionResult<Option<String>> {
    if let Some(entry) = state.skipped_keys.get_mut(sender) {
        if let Some(message_key) = entry.message_keys.remove(&header.number) {
            let conversation_key = nip44::v2::ConversationKey::new(message_key);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(ciphertext)
                .map_err(|e| SessionError::Decryption(e.to_string()))?;
            let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
                .map_err(|e| SessionError::Decryption(e.to_string()))?;
            let plaintext =
                String::from_utf8(plaintext_bytes).map_err(|e| SessionError::Decryption(e.to_string()))?;
            if entry.message_keys.is_empty() {
                state.skipped_keys.remove(sender);
            }
            return Ok(Some(plaintext));
        }
    }
    Ok(None)
}

fn prune_skipped_message_keys(map: &mut HashMap<u32, [u8; 32]>) {
    if map.len() <= MAX_SKIP {
        return;
    }
    let mut keys: Vec<u32> = map.keys().copied().collect();
    keys.sort_unstable();
    let to_remove = map.len().saturating_sub(MAX_SKIP);
    for key in keys.into_iter().take(to_remove) {
        map.remove(&key);
    }
}

fn encode_header(header: &Header) -> SessionResult<String> {
    serde_json::to_string(&serde_json::json!({
        "number": header.number,
        "previousChainLength": header.previous_chain_length,
        "nextPublicKey": header.next_public_key,
    }))
    .map_err(|e| SessionError::Serialization(e.to_string()))
}

fn decode_header(encoded: &str) -> SessionResult<Header> {
    let value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|e| SessionError::Serialization(e.to_string()))?;
    let number = value
        .get("number")
        .and_then(|field| field.as_u64())
        .ok_or(SessionError::InvalidHeader)? as u32;
    let previous_chain_length = value
        .get("previousChainLength")
        .and_then(|field| field.as_u64())
        .ok_or(SessionError::InvalidHeader)? as u32;
    let next_public_key = value
        .get("nextPublicKey")
        .and_then(|field| field.as_str())
        .ok_or(SessionError::InvalidHeader)?
        .to_string();

    Ok(Header {
        number,
        previous_chain_length,
        next_public_key,
    })
}

fn pubkey_from_hex(hex_str: &str) -> SessionResult<PublicKey> {
    let bytes = hex::decode(hex_str).map_err(|e| SessionError::InvalidEvent(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(SessionError::InvalidEvent(
            "invalid public key length".to_string(),
        ));
    }
    PublicKey::from_slice(&bytes).map_err(|e| SessionError::InvalidEvent(e.to_string()))
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

    fn inner_event(kind: u16, content: &str, created_at: u64) -> UnsignedEvent {
        let mut event = EventBuilder::new(nostr::Kind::Custom(kind), content)
            .custom_created_at(Timestamp::from(created_at))
            .build(Keys::generate().public_key());
        event.ensure_id();
        event
    }

    #[test]
    fn session_send_returns_next_state_and_outputs() {
        let (_, their_ephemeral_pubkey) = keypair(1);
        let (our_ephemeral_sk, _) = keypair(2);
        let init = SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: their_ephemeral_pubkey,
            our_ephemeral_nostr_private_key: our_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [3u8; 32],
            is_initiator: true,
            shared_secret: [4u8; 32],
        };
        let state = SessionState::init(init).unwrap();
        let input = SessionSendInput {
            inner_event: inner_event(14, "hello", 1),
            now_secs: 10,
            now_ms: 10_000,
        };

        let output = state.send_event(input).unwrap();

        assert_eq!(state.sending_chain_message_number, 0);
        assert_ne!(output.next.sending_chain_message_number, state.sending_chain_message_number);
        assert_eq!(u32::from(output.outer_event.kind.as_u16()), MESSAGE_EVENT_KIND);
        assert!(output.inner_event.id.is_some());
    }

    #[test]
    fn session_receive_relevant_event_returns_decrypted() {
        let (_, alice_identity_pk) = keypair(10);
        let (alice_ephemeral_sk, alice_ephemeral_pk) = keypair(11);
        let (bob_ephemeral_sk, bob_ephemeral_pk) = keypair(12);

        let alice = SessionState::init(SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: bob_ephemeral_pk,
            our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [13u8; 32],
            is_initiator: true,
            shared_secret: [14u8; 32],
        })
        .unwrap();

        let bob = SessionState::init(SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: alice_ephemeral_pk,
            our_ephemeral_nostr_private_key: bob_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [15u8; 32],
            is_initiator: false,
            shared_secret: [14u8; 32],
        })
        .unwrap();

        let send = alice
            .send_event(SessionSendInput {
                inner_event: inner_event(14, &alice_identity_pk.to_hex(), 2),
                now_secs: 20,
                now_ms: 20_000,
            })
            .unwrap();

        let received = bob.receive_event(SessionReceiveInput {
            outer_event: send.outer_event,
            replacement_next_nostr_private_key: [16u8; 32],
        });

        match received {
            SessionReceiveResult::Decrypted { inner_event, meta, .. } => {
                assert_eq!(meta.sender, alice_ephemeral_pk);
                assert_eq!(inner_event.content, alice_identity_pk.to_hex());
            }
            other => panic!("expected decrypted result, got {other:?}"),
        }
    }

    #[test]
    fn session_receive_irrelevant_event_returns_not_for_this_session() {
        let (_, their_ephemeral_pubkey) = keypair(20);
        let (our_ephemeral_sk, _) = keypair(21);
        let state = SessionState::init(SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: their_ephemeral_pubkey,
            our_ephemeral_nostr_private_key: our_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [22u8; 32],
            is_initiator: false,
            shared_secret: [23u8; 32],
        })
        .unwrap();

        let unrelated = EventBuilder::new(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16), "ciphertext")
            .custom_created_at(Timestamp::from(30))
            .build(Keys::generate().public_key())
            .sign_with_keys(&Keys::generate())
            .unwrap();

        let result = state.receive_event(SessionReceiveInput {
            outer_event: unrelated,
            replacement_next_nostr_private_key: [24u8; 32],
        });

        assert!(matches!(result, SessionReceiveResult::NotForThisSession { .. }));
    }

    #[test]
    fn session_receive_malformed_but_relevant_returns_invalid_relevant() {
        let (alice_ephemeral_sk, alice_ephemeral_pk) = keypair(30);
        let (bob_ephemeral_sk, bob_ephemeral_pk) = keypair(31);

        let alice = SessionState::init(SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: bob_ephemeral_pk,
            our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [32u8; 32],
            is_initiator: true,
            shared_secret: [33u8; 32],
        })
        .unwrap();

        let bob = SessionState::init(SessionInitInput {
            session_id: None,
            their_ephemeral_nostr_public_key: alice_ephemeral_pk,
            our_ephemeral_nostr_private_key: bob_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [34u8; 32],
            is_initiator: false,
            shared_secret: [33u8; 32],
        })
        .unwrap();

        let mut send = alice
            .send_event(SessionSendInput {
                inner_event: inner_event(14, "hello", 3),
                now_secs: 40,
                now_ms: 40_000,
            })
            .unwrap()
            .outer_event;
        send.content = "tampered".to_string();

        let received = bob.receive_event(SessionReceiveInput {
            outer_event: send,
            replacement_next_nostr_private_key: [35u8; 32],
        });

        match received {
            SessionReceiveResult::InvalidRelevant { next, .. } => assert_eq!(next, bob),
            other => panic!("expected invalid relevant result, got {other:?}"),
        }
    }
}
