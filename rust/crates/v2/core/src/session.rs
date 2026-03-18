use crate::{Error, Result, SessionId};
use base64::Engine;
use hkdf::Hkdf;
use nostr::nips::nip44::{self, Version};
use nostr::PublicKey;
use nostr::{Event, EventBuilder, Keys, SecretKey, Tag, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;

pub const MESSAGE_EVENT_KIND: u32 = 1060;
pub const MAX_SKIP: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub number: u32,
    pub previous_chain_length: u32,
    pub next_public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerializableKeyPair {
    pub public_key: PublicKey,
    pub private_key: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedKeysEntry {
    pub header_keys: Vec<[u8; 32]>,
    pub message_keys: HashMap<u32, [u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
pub struct SessionReceiveMeta {
    pub sender: PublicKey,
    pub outer_event_id: String,
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
        error: Error,
    },
}

impl SessionState {
    pub fn init(input: SessionInitInput) -> Result<Self> {
        let our_keys = Keys::new(
            SecretKey::from_slice(&input.our_ephemeral_nostr_private_key)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
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
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
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
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
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

    pub fn send_event(&self, input: SessionSendInput) -> Result<SessionSendResult> {
        if !self.can_send() {
            return Err(Error::NotInitiator);
        }

        let mut next = self.clone();
        let inner_event = normalize_inner_event(input.inner_event, input.now_ms)?;
        let rumor_json =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;
        let (header, encrypted_data) = ratchet_encrypt(&mut next, &rumor_json)?;

        let our_current = next.our_current_nostr_key.as_ref().ok_or(Error::NotInitiator)?;
        let their_next = next
            .their_next_nostr_public_key
            .ok_or(Error::SessionNotReady)?;

        let our_secret = SecretKey::from_slice(&our_current.private_key)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let encrypted_header = nip44::encrypt(
            &our_secret,
            &their_next,
            serde_json::to_string(&header)
                .map_err(|e| Error::Serialization(e.to_string()))?,
            Version::V2,
        )
        .map_err(|e| Error::Decryption(e.to_string()))?;

        let header_tag = Tag::parse(&["header".to_string(), encrypted_header])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
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
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;

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
        let outcome = (|| -> Result<(SessionState, UnsignedEvent, SessionReceiveMeta)> {
            let encrypted_header = input
                .outer_event
                .tags
                .iter()
                .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("header"))
                .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()))
                .ok_or(Error::InvalidHeader)?;

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
            let mut inner_event: UnsignedEvent =
                serde_json::from_str(&plaintext).map_err(|e| Error::Serialization(e.to_string()))?;
            inner_event.id = None;
            inner_event.ensure_id();

            Ok((
                next,
                inner_event,
                SessionReceiveMeta {
                    sender,
                    outer_event_id: input.outer_event.id.to_string(),
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
            .expect("32 bytes is valid length");
        outputs.push(okm);
    }
    outputs
}

fn normalize_inner_event(mut event: UnsignedEvent, now_ms: u64) -> Result<UnsignedEvent> {
    let has_ms_tag = event.tags.iter().any(|tag| {
        tag.as_slice().first().map(|s| s.as_str()) == Some("ms")
    });
    if !has_ms_tag {
        let ms_tag = Tag::parse(&["ms".to_string(), now_ms.to_string()])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
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

fn ratchet_encrypt(state: &mut SessionState, plaintext: &str) -> Result<(Header, String)> {
    let sending_chain_key = state.sending_chain_key.ok_or(Error::SessionNotReady)?;
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
    let encrypted_bytes =
        nip44::v2::encrypt_to_bytes(&conversation_key, plaintext).map_err(|e| Error::Decryption(e.to_string()))?;
    Ok((
        header,
        base64::engine::general_purpose::STANDARD.encode(encrypted_bytes),
    ))
}

fn decrypt_header(state: &SessionState, encrypted_header: &str, sender: &PublicKey) -> Result<(Header, bool)> {
    if let Some(current) = &state.our_current_nostr_key {
        let current_secret = SecretKey::from_slice(&current.private_key)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        if let Ok(decrypted) = nip44::decrypt(&current_secret, sender, encrypted_header) {
            let header: Header =
                serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
            return Ok((header, false));
        }
    }

    let next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
    let decrypted =
        nip44::decrypt(&next_secret, sender, encrypted_header).map_err(|e| Error::Decryption(e.to_string()))?;
    let header: Header =
        serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
    Ok((header, true))
}

fn ratchet_step(state: &mut SessionState, replacement_next_private_key: [u8; 32]) -> Result<()> {
    state.previous_sending_chain_message_count = state.sending_chain_message_number;
    state.sending_chain_message_number = 0;
    state.receiving_chain_message_number = 0;

    let our_next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
    let their_next = state
        .their_next_nostr_public_key
        .ok_or(Error::SessionNotReady)?;

    let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_secret, &their_next);
    let outputs = kdf(&state.root_key, conversation_key1.as_bytes(), 2);
    state.receiving_chain_key = Some(outputs[1]);
    state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

    let replacement_keys = Keys::new(
        SecretKey::from_slice(&replacement_next_private_key)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?,
    );
    state.our_next_nostr_key = SerializableKeyPair {
        public_key: replacement_keys.public_key(),
        private_key: replacement_next_private_key,
    };

    let replacement_secret = SecretKey::from_slice(&replacement_next_private_key)
        .map_err(|e| Error::InvalidEvent(e.to_string()))?;
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
) -> Result<String> {
    if let Some(plaintext) = try_skipped_message_keys(state, header, ciphertext, sender)? {
        return Ok(plaintext);
    }

    let receiving_chain_key = state.receiving_chain_key.ok_or(Error::SessionNotReady)?;
    skip_message_keys(state, header.number, sender)?;
    let outputs = kdf(&receiving_chain_key, &[1u8], 2);
    state.receiving_chain_key = Some(outputs[0]);
    state.receiving_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(outputs[1]);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| Error::Decryption(e.to_string()))?;
    let plaintext_bytes =
        nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
            .map_err(|e| Error::Decryption(e.to_string()))?;
    String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))
}

fn skip_message_keys(state: &mut SessionState, until: u32, sender: &PublicKey) -> Result<()> {
    if until <= state.receiving_chain_message_number {
        return Ok(());
    }
    if (until - state.receiving_chain_message_number) as usize > MAX_SKIP {
        return Err(Error::TooManySkippedMessages);
    }

    let entry = state
        .skipped_keys
        .entry(*sender)
        .or_insert_with(|| SkippedKeysEntry {
            header_keys: Vec::new(),
            message_keys: HashMap::new(),
        });

    while state.receiving_chain_message_number < until {
        let receiving_chain_key = state.receiving_chain_key.ok_or(Error::SessionNotReady)?;
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
) -> Result<Option<String>> {
    if let Some(entry) = state.skipped_keys.get_mut(sender) {
        if let Some(message_key) = entry.message_keys.remove(&header.number) {
            let conversation_key = nip44::v2::ConversationKey::new(message_key);
            let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                .decode(ciphertext)
                .map_err(|e| Error::Decryption(e.to_string()))?;
            let plaintext_bytes =
                nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)
                    .map_err(|e| Error::Decryption(e.to_string()))?;
            let plaintext =
                String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))?;
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

fn pubkey_from_hex(hex_str: &str) -> Result<PublicKey> {
    let bytes = hex::decode(hex_str).map_err(|e| Error::InvalidEvent(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(Error::InvalidEvent("invalid pubkey length".to_string()));
    }
    PublicKey::from_slice(&bytes).map_err(|e| Error::InvalidEvent(e.to_string()))
}
