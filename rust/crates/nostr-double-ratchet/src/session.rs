use crate::{
    utils::{kdf, pubkey_from_hex},
    Error, Header, Result, SerializableKeyPair, SessionState, SkippedKeysEntry, MAX_SKIP,
    MESSAGE_EVENT_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::{Event, EventBuilder, EventId, Keys, PublicKey, SecretKey, Tag, Timestamp, UnsignedEvent};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub state: SessionState,
}

#[derive(Debug, Clone)]
pub struct SessionInitInput {
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
    pub next: Session,
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

#[derive(Debug)]
pub enum SessionReceiveResult {
    NotForThisSession { next: Session },
    Decrypted {
        next: Session,
        plaintext: String,
        inner_event: Option<UnsignedEvent>,
        meta: SessionReceiveMeta,
    },
    InvalidRelevant {
        next: Session,
        error: Error,
    },
}

impl Session {
    pub fn new(state: SessionState) -> Self {
        Self { state }
    }

    pub fn init(input: SessionInitInput) -> Result<Self> {
        let our_keys = Keys::new(SecretKey::from_slice(&input.our_ephemeral_nostr_private_key)?);
        let our_current_nostr_key = if input.is_initiator {
            Some(SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: input.our_ephemeral_nostr_private_key,
            })
        } else {
            None
        };

        let our_next_nostr_key = if input.is_initiator {
            let next_keys = Keys::new(SecretKey::from_slice(&input.our_next_nostr_private_key)?);
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
            let next_sk = SecretKey::from_slice(&our_next_nostr_key.private_key)?;
            let conversation_key =
                nip44::v2::ConversationKey::derive(&next_sk, &input.their_ephemeral_nostr_public_key);
            let outputs = kdf(&input.shared_secret, conversation_key.as_bytes(), 2);
            (outputs[0], Some(outputs[1]))
        } else {
            (input.shared_secret, None)
        };

        Ok(Self {
            state: SessionState {
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
            },
        })
    }

    pub fn can_send(&self) -> bool {
        self.state.their_next_nostr_public_key.is_some() && self.state.our_current_nostr_key.is_some()
    }

    pub fn send_event(&self, input: SessionSendInput) -> Result<SessionSendResult> {
        if !self.can_send() {
            return Err(Error::NotInitiator);
        }

        let mut next = self.clone();
        let inner_event = normalize_inner_event(input.inner_event, input.now_ms)?;
        let rumor_json =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;
        let (header, encrypted_data) = ratchet_encrypt(&mut next.state, &rumor_json)?;

        let our_current = next
            .state
            .our_current_nostr_key
            .as_ref()
            .ok_or(Error::NotInitiator)?;
        let their_next = next
            .state
            .their_next_nostr_public_key
            .ok_or(Error::SessionNotReady)?;

        let our_secret = SecretKey::from_slice(&our_current.private_key)?;
        let encrypted_header = nip44::encrypt(
            &our_secret,
            &their_next,
            serde_json::to_string(&header).map_err(|e| Error::Serialization(e.to_string()))?,
            Version::V2,
        )?;

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
            .state
            .their_next_nostr_public_key
            .is_some_and(|pk| pk == sender)
            || self
                .state
                .their_current_nostr_public_key
                .is_some_and(|pk| pk == sender);

        if u32::from(input.outer_event.kind.as_u16()) != MESSAGE_EVENT_KIND || !expected_sender {
            return SessionReceiveResult::NotForThisSession { next: self.clone() };
        }

        let snapshot = self.clone();
        let mut next = self.clone();
        let outcome = (|| -> Result<(Session, String, Option<UnsignedEvent>, SessionReceiveMeta)> {
            let encrypted_header = input
                .outer_event
                .tags
                .iter()
                .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("header"))
                .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()))
                .ok_or(Error::InvalidHeader)?;

            let (header, should_ratchet) = decrypt_header(&next.state, &encrypted_header, &sender)?;
            let their_next_hex = next
                .state
                .their_next_nostr_public_key
                .map(|pk| hex::encode(pk.to_bytes()))
                .unwrap_or_default();
            if header.next_public_key != their_next_hex {
                next.state.their_current_nostr_public_key = next.state.their_next_nostr_public_key;
                next.state.their_next_nostr_public_key = Some(pubkey_from_hex(&header.next_public_key)?);
            }

            if should_ratchet {
                if next.state.receiving_chain_key.is_some() {
                    skip_message_keys(
                        &mut next.state,
                        header.previous_chain_length,
                        &sender,
                    )?;
                }
                ratchet_step(
                    &mut next.state,
                    input.replacement_next_nostr_private_key,
                )?;
            }

            let plaintext =
                ratchet_decrypt(&mut next.state, &header, &input.outer_event.content, &sender)?;
            let plaintext = normalize_inner_rumor_id(&plaintext);
            let inner_event = parse_inner_event(&plaintext)?;

            Ok((
                next,
                plaintext,
                inner_event,
                SessionReceiveMeta {
                    sender,
                    outer_event_id: input.outer_event.id,
                },
            ))
        })();

        match outcome {
            Ok((next, plaintext, inner_event, meta)) => SessionReceiveResult::Decrypted {
                next,
                plaintext,
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

fn normalize_inner_event(mut event: UnsignedEvent, now_ms: u64) -> Result<UnsignedEvent> {
    let has_ms_tag = event
        .tags
        .iter()
        .any(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("ms"));
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

fn parse_inner_event(plaintext: &str) -> Result<Option<UnsignedEvent>> {
    match serde_json::from_str::<UnsignedEvent>(plaintext) {
        Ok(mut inner_event) => {
            inner_event.id = None;
            inner_event.ensure_id();
            Ok(Some(inner_event))
        }
        Err(unsigned_event_error) => {
            serde_json::from_str::<serde_json::Value>(plaintext)
                .map_err(|_| Error::Serialization(unsigned_event_error.to_string()))?;
            Ok(None)
        }
    }
}

fn normalize_inner_rumor_id(plaintext: &str) -> String {
    let mut value: serde_json::Value = match serde_json::from_str(plaintext) {
        Ok(value) => value,
        Err(_) => return plaintext.to_string(),
    };

    let obj = match value.as_object_mut() {
        Some(obj) => obj,
        None => return plaintext.to_string(),
    };

    let pubkey = match obj.get("pubkey").and_then(|value| value.as_str()) {
        Some(pubkey) => pubkey,
        None => return plaintext.to_string(),
    };
    let created_at = match obj.get("created_at").and_then(|value| value.as_u64()) {
        Some(created_at) => created_at,
        None => return plaintext.to_string(),
    };
    let kind = match obj.get("kind").and_then(|value| value.as_u64()) {
        Some(kind) => kind,
        None => return plaintext.to_string(),
    };
    let content = match obj.get("content").and_then(|value| value.as_str()) {
        Some(content) => content,
        None => return plaintext.to_string(),
    };

    let tags_value = match obj.get("tags").and_then(|value| value.as_array()) {
        Some(tags) => tags,
        None => return plaintext.to_string(),
    };

    let mut tags: Vec<Vec<String>> = Vec::with_capacity(tags_value.len());
    for tag in tags_value {
        let arr = match tag.as_array() {
            Some(arr) => arr,
            None => return plaintext.to_string(),
        };
        let mut out: Vec<String> = Vec::with_capacity(arr.len());
        for value in arr {
            let string = match value.as_str() {
                Some(string) => string,
                None => return plaintext.to_string(),
            };
            out.push(string.to_string());
        }
        tags.push(out);
    }

    let canonical = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let canonical_json = match serde_json::to_string(&canonical) {
        Ok(json) => json,
        Err(_) => return plaintext.to_string(),
    };

    let computed = hex::encode(Sha256::digest(canonical_json.as_bytes()));
    obj.insert("id".to_string(), serde_json::Value::String(computed));

    serde_json::to_string(&value).unwrap_or_else(|_| plaintext.to_string())
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
    let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext)?;
    Ok((
        header,
        base64::engine::general_purpose::STANDARD.encode(encrypted_bytes),
    ))
}

fn decrypt_header(
    state: &SessionState,
    encrypted_header: &str,
    sender: &PublicKey,
) -> Result<(Header, bool)> {
    if let Some(current) = &state.our_current_nostr_key {
        let current_secret = SecretKey::from_slice(&current.private_key)?;
        if let Ok(decrypted) = nip44::decrypt(&current_secret, sender, encrypted_header) {
            let header =
                serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
            return Ok((header, false));
        }
    }

    let next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)?;
    let decrypted = nip44::decrypt(&next_secret, sender, encrypted_header)?;
    let header =
        serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
    Ok((header, true))
}

fn ratchet_step(state: &mut SessionState, replacement_next_private_key: [u8; 32]) -> Result<()> {
    state.previous_sending_chain_message_count = state.sending_chain_message_number;
    state.sending_chain_message_number = 0;
    state.receiving_chain_message_number = 0;

    let our_next_secret = SecretKey::from_slice(&state.our_next_nostr_key.private_key)?;
    let their_next = state
        .their_next_nostr_public_key
        .ok_or(Error::SessionNotReady)?;

    let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_secret, &their_next);
    let outputs = kdf(&state.root_key, conversation_key1.as_bytes(), 2);
    state.receiving_chain_key = Some(outputs[1]);
    state.our_current_nostr_key = Some(state.our_next_nostr_key.clone());

    let replacement_keys = Keys::new(SecretKey::from_slice(&replacement_next_private_key)?);
    state.our_next_nostr_key = SerializableKeyPair {
        public_key: replacement_keys.public_key(),
        private_key: replacement_next_private_key,
    };

    let replacement_secret = SecretKey::from_slice(&replacement_next_private_key)?;
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

    if state.receiving_chain_key.is_none() {
        return Err(Error::SessionNotReady);
    }

    skip_message_keys(state, header.number, sender)?;
    let receiving_chain_key = state.receiving_chain_key.ok_or(Error::SessionNotReady)?;
    let outputs = kdf(&receiving_chain_key, &[1u8], 2);
    state.receiving_chain_key = Some(outputs[0]);
    state.receiving_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(outputs[1]);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| Error::Decryption(e.to_string()))?;
    let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
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
            let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
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
        let state = Session::init(SessionInitInput {
            their_ephemeral_nostr_public_key: their_ephemeral_pubkey,
            our_ephemeral_nostr_private_key: our_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [3u8; 32],
            is_initiator: true,
            shared_secret: [4u8; 32],
        })
        .unwrap();

        let output = state
            .send_event(SessionSendInput {
                inner_event: inner_event(14, "hello", 1),
                now_secs: 10,
                now_ms: 10_000,
            })
            .unwrap();

        assert_eq!(state.state.sending_chain_message_number, 0);
        assert_ne!(
            output.next.state.sending_chain_message_number,
            state.state.sending_chain_message_number
        );
        assert_eq!(u32::from(output.outer_event.kind.as_u16()), MESSAGE_EVENT_KIND);
        assert!(output.inner_event.id.is_some());
    }

    #[test]
    fn session_receive_relevant_event_returns_decrypted() {
        let (_, alice_identity_pk) = keypair(10);
        let (alice_ephemeral_sk, alice_ephemeral_pk) = keypair(11);
        let (bob_ephemeral_sk, bob_ephemeral_pk) = keypair(12);

        let alice = Session::init(SessionInitInput {
            their_ephemeral_nostr_public_key: bob_ephemeral_pk,
            our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [13u8; 32],
            is_initiator: true,
            shared_secret: [14u8; 32],
        })
        .unwrap();

        let bob = Session::init(SessionInitInput {
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
            SessionReceiveResult::Decrypted {
                inner_event: Some(inner_event),
                meta,
                ..
            } => {
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
        let state = Session::init(SessionInitInput {
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

        let alice = Session::init(SessionInitInput {
            their_ephemeral_nostr_public_key: bob_ephemeral_pk,
            our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
            our_next_nostr_private_key: [32u8; 32],
            is_initiator: true,
            shared_secret: [33u8; 32],
        })
        .unwrap();

        let bob = Session::init(SessionInitInput {
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

    #[test]
    fn skip_message_keys_prunes_to_max_skip() {
        let our_keys = Keys::generate();
        let mut state = SessionState {
            root_key: [0u8; 32],
            their_current_nostr_public_key: None,
            their_next_nostr_public_key: None,
            our_current_nostr_key: None,
            our_next_nostr_key: SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: our_keys.secret_key().to_secret_bytes(),
            },
            receiving_chain_key: Some([7u8; 32]),
            sending_chain_key: None,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        };

        let sender = Keys::generate().public_key();

        skip_message_keys(&mut state, MAX_SKIP as u32, &sender).unwrap();
        skip_message_keys(&mut state, (MAX_SKIP * 2) as u32, &sender).unwrap();

        let entry = state.skipped_keys.get(&sender).unwrap();
        assert!(entry.message_keys.len() <= MAX_SKIP);
        assert!(!entry.message_keys.contains_key(&0));
        assert!(entry.message_keys.contains_key(&((MAX_SKIP * 2 - 1) as u32)));
    }
}
