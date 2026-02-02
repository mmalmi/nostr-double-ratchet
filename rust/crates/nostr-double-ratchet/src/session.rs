use crate::{
    pubsub::build_filter,
    utils::{kdf, pubkey_from_hex},
    Error, EventCallback, Header, Result, SerializableKeyPair, SessionState, SkippedKeysEntry,
    Unsubscribe, MAX_SKIP, MESSAGE_EVENT_KIND, REACTION_KIND, RECEIPT_KIND, TYPING_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::PublicKey;
use nostr::{EventBuilder, Keys, Tag, Timestamp, UnsignedEvent};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct Session {
    pub state: SessionState,
    pub name: String,
    pub(crate) nostr_unsubscribe: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) nostr_next_unsubscribe: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) skipped_subscription: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) internal_subscriptions: Arc<Mutex<Vec<EventCallback>>>,
    pub(crate) current_key_subid: Arc<Mutex<Option<String>>>,
    pub(crate) next_key_subid: Arc<Mutex<Option<String>>>,
    pub(crate) event_tx: Option<crossbeam_channel::Sender<crate::SessionManagerEvent>>,
    pub(crate) pubsub: Option<Arc<dyn crate::NostrPubSub>>,
}

impl Session {
    pub fn new(state: SessionState, name: String) -> Self {
        Self {
            state,
            name,
            nostr_unsubscribe: Arc::new(Mutex::new(None)),
            nostr_next_unsubscribe: Arc::new(Mutex::new(None)),
            skipped_subscription: Arc::new(Mutex::new(None)),
            internal_subscriptions: Arc::new(Mutex::new(Vec::new())),
            current_key_subid: Arc::new(Mutex::new(None)),
            next_key_subid: Arc::new(Mutex::new(None)),
            event_tx: None,
            pubsub: None,
        }
    }

    pub fn set_event_tx(
        &mut self,
        event_tx: crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) {
        self.event_tx = Some(event_tx);
    }

    pub fn set_pubsub(&mut self, pubsub: Arc<dyn crate::NostrPubSub>) {
        self.pubsub = Some(pubsub);
    }
}

impl Session {
    pub fn init(
        their_ephemeral_nostr_public_key: PublicKey,
        our_ephemeral_nostr_private_key: [u8; 32],
        is_initiator: bool,
        shared_secret: [u8; 32],
        name: Option<String>,
    ) -> Result<Self> {
        let our_keys = Keys::new(nostr::SecretKey::from_slice(
            &our_ephemeral_nostr_private_key,
        )?);
        let our_next_private_key = nostr::Keys::generate().secret_key().to_secret_bytes();
        let our_next_keys = Keys::new(nostr::SecretKey::from_slice(&our_next_private_key)?);

        let (root_key, sending_chain_key, our_current_nostr_key, our_next_nostr_key);

        if is_initiator {
            let our_current_pubkey = our_keys.public_key();
            let conversation_key = nip44::v2::ConversationKey::derive(
                our_next_keys.secret_key(),
                &their_ephemeral_nostr_public_key,
            );
            let kdf_outputs = kdf(&shared_secret, conversation_key.as_bytes(), 2);
            root_key = kdf_outputs[0];
            sending_chain_key = Some(kdf_outputs[1]);
            our_current_nostr_key = Some(SerializableKeyPair {
                public_key: our_current_pubkey,
                private_key: our_ephemeral_nostr_private_key,
            });
            our_next_nostr_key = SerializableKeyPair {
                public_key: our_next_keys.public_key(),
                private_key: our_next_private_key,
            };
        } else {
            root_key = shared_secret;
            sending_chain_key = None;
            our_current_nostr_key = None;
            our_next_nostr_key = SerializableKeyPair {
                public_key: our_keys.public_key(),
                private_key: our_ephemeral_nostr_private_key,
            };
        }

        // theirCurrentNostrPublicKey is NEVER set in init - it's populated dynamically when processing messages
        // Both initiator and non-initiator only set theirNextNostrPublicKey initially
        let their_current = None;
        let their_next = Some(their_ephemeral_nostr_public_key);

        let state = SessionState {
            root_key,
            their_current_nostr_public_key: their_current,
            their_next_nostr_public_key: their_next,
            our_current_nostr_key,
            our_next_nostr_key,
            receiving_chain_key: None,
            sending_chain_key,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        };

        Ok(Self {
            state,
            name: name.unwrap_or_else(|| "session".to_string()),
            nostr_unsubscribe: Arc::new(Mutex::new(None)),
            nostr_next_unsubscribe: Arc::new(Mutex::new(None)),
            skipped_subscription: Arc::new(Mutex::new(None)),
            internal_subscriptions: Arc::new(Mutex::new(Vec::new())),
            current_key_subid: Arc::new(Mutex::new(None)),
            next_key_subid: Arc::new(Mutex::new(None)),
            event_tx: None,
            pubsub: None,
        })
    }

    /// Subscribe to kind 1060 messages for this session's ratchet keys
    pub fn subscribe_to_messages(&mut self) -> Result<()> {
        // Prefer pubsub if available, fallback to event_tx for backward compat
        if let Some(ref pubsub) = self.pubsub {
            if let Some(current_pk) = self.state.their_current_nostr_public_key {
                let filter = build_filter()
                    .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                    .authors(vec![current_pk])
                    .build();

                let subid = pubsub.subscribe(filter)?;
                *self.current_key_subid.lock().unwrap() = Some(subid);
            }

            if let Some(next_pk) = self.state.their_next_nostr_public_key {
                let filter = build_filter()
                    .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                    .authors(vec![next_pk])
                    .build();

                let subid = pubsub.subscribe(filter)?;
                *self.next_key_subid.lock().unwrap() = Some(subid);
            }
        } else if let Some(ref event_tx) = self.event_tx {
            // Fallback to old event_tx method
            if let Some(current_pk) = self.state.their_current_nostr_public_key {
                let filter = build_filter()
                    .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                    .authors(vec![current_pk])
                    .build();

                let filter_json = serde_json::to_string(&filter)?;
                let subid = format!("session-current-{}", uuid::Uuid::new_v4());

                event_tx
                    .send(crate::SessionManagerEvent::Subscribe(filter_json))
                    .map_err(|_| crate::Error::Storage("Failed to send subscribe".to_string()))?;

                *self.current_key_subid.lock().unwrap() = Some(subid);
            }

            if let Some(next_pk) = self.state.their_next_nostr_public_key {
                let filter = build_filter()
                    .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                    .authors(vec![next_pk])
                    .build();

                let filter_json = serde_json::to_string(&filter)?;
                let subid = format!("session-next-{}", uuid::Uuid::new_v4());

                event_tx
                    .send(crate::SessionManagerEvent::Subscribe(filter_json))
                    .map_err(|_| crate::Error::Storage("Failed to send subscribe".to_string()))?;

                *self.next_key_subid.lock().unwrap() = Some(subid);
            }
        }

        Ok(())
    }

    /// Update subscriptions after ratchet step (keys changed)
    pub fn update_subscriptions(&mut self) -> Result<()> {
        // Unsubscribe from old keys
        if let Some(event_tx) = &self.event_tx {
            if let Some(old_subid) = self.current_key_subid.lock().unwrap().take() {
                let _ = event_tx.send(crate::SessionManagerEvent::Unsubscribe(old_subid));
            }
            if let Some(old_subid) = self.next_key_subid.lock().unwrap().take() {
                let _ = event_tx.send(crate::SessionManagerEvent::Unsubscribe(old_subid));
            }
        }

        // Subscribe to new keys
        self.subscribe_to_messages()
    }

    pub fn can_send(&self) -> bool {
        self.state.their_next_nostr_public_key.is_some()
            && self.state.our_current_nostr_key.is_some()
    }

    pub fn send(&mut self, text: String) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();
        self.send_event(EventBuilder::text_note(text).build(dummy_keys.public_key()))
    }

    /// Send a reaction to a message through the encrypted session.
    ///
    /// # Arguments
    /// * `message_id` - The ID of the message being reacted to
    /// * `emoji` - The emoji or reaction content (e.g., "👍", "❤️", "+1")
    ///
    /// # Returns
    /// A signed Nostr event containing the encrypted reaction.
    pub fn send_reaction(&mut self, message_id: &str, emoji: &str) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let event = EventBuilder::new(nostr::Kind::from(REACTION_KIND as u16), emoji)
            .tag(
                Tag::parse(&["e".to_string(), message_id.to_string()])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            )
            .build(dummy_keys.public_key());

        self.send_event(event)
    }

    /// Send a delivery/read receipt for messages through the encrypted session.
    ///
    /// # Arguments
    /// * `receipt_type` - Either "delivered" or "seen"
    /// * `message_ids` - The IDs of the messages being acknowledged
    ///
    /// # Returns
    /// A signed Nostr event containing the encrypted receipt.
    pub fn send_receipt(
        &mut self,
        receipt_type: &str,
        message_ids: &[&str],
    ) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let mut builder = EventBuilder::new(nostr::Kind::from(RECEIPT_KIND as u16), receipt_type);
        for id in message_ids {
            builder = builder.tag(
                Tag::parse(&["e".to_string(), id.to_string()])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            );
        }

        self.send_event(builder.build(dummy_keys.public_key()))
    }

    /// Send a typing indicator through the encrypted session.
    ///
    /// # Returns
    /// A signed Nostr event containing the encrypted typing indicator.
    pub fn send_typing(&mut self) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let event = EventBuilder::new(nostr::Kind::from(TYPING_KIND as u16), "typing")
            .build(dummy_keys.public_key());

        self.send_event(event)
    }

    pub fn send_event(&mut self, mut event: UnsignedEvent) -> Result<nostr::Event> {
        if !self.can_send() {
            return Err(Error::NotInitiator);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        event.created_at = Timestamp::from(now);
        event.pubkey = Keys::generate().public_key();

        let ms_tag = Tag::parse(&["ms".to_string(), (now * 1000).to_string()])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let has_ms_tag = event.tags.iter().any(|t| {
            let v = t.clone().to_vec();
            v.first().map(|s| s.as_str()) == Some("ms")
        });

        if !has_ms_tag {
            let mut builder = EventBuilder::new(event.kind, &event.content);
            for tag in event.tags.iter() {
                builder = builder.tag(tag.clone());
            }
            builder = builder.tag(ms_tag);
            event = builder
                .custom_created_at(event.created_at)
                .build(event.pubkey);
        }

        // Event fields were mutated; ensure id matches the final content.
        event.id = None;
        event.ensure_id();

        let rumor_json = serde_json::to_string(&event)?;
        let (header, encrypted_data) = self.ratchet_encrypt(&rumor_json)?;

        let our_current = self.state.our_current_nostr_key.as_ref().unwrap();
        let their_next = &self.state.their_next_nostr_public_key;

        let our_sk = nostr::SecretKey::from_slice(&our_current.private_key)?;
        let their_pk = their_next.unwrap();

        let encrypted_header = nip44::encrypt(
            &our_sk,
            &their_pk,
            &serde_json::to_string(&header)?,
            Version::V2,
        )?;

        let tags = vec![Tag::parse(&["header".to_string(), encrypted_header])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?];

        let author_pubkey = our_current.public_key;

        // Build the event
        let unsigned_event =
            nostr::EventBuilder::new(nostr::Kind::from(MESSAGE_EVENT_KIND as u16), encrypted_data)
                .tags(tags)
                .custom_created_at(Timestamp::from(now))
                .build(author_pubkey);

        // Sign with the ephemeral private key before returning
        let author_secret_key = nostr::SecretKey::from_slice(&our_current.private_key)?;
        let author_keys = nostr::Keys::new(author_secret_key);
        let signed_event = unsigned_event
            .sign_with_keys(&author_keys)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;

        Ok(signed_event)
    }

    fn ratchet_encrypt(&mut self, plaintext: &str) -> Result<(Header, String)> {
        let sending_chain_key = self.state.sending_chain_key.ok_or(Error::SessionNotReady)?;

        let kdf_outputs = kdf(&sending_chain_key, &[1u8], 2);
        self.state.sending_chain_key = Some(kdf_outputs[0]);
        let message_key = kdf_outputs[1];

        let header = Header {
            number: self.state.sending_chain_message_number,
            next_public_key: hex::encode(self.state.our_next_nostr_key.public_key.to_bytes()),
            previous_chain_length: self.state.previous_sending_chain_message_count,
        };

        self.state.sending_chain_message_number += 1;

        let conversation_key = nip44::v2::ConversationKey::new(message_key);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext)?;
        let ciphertext = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);
        Ok((header, ciphertext))
    }

    fn ratchet_decrypt(
        &mut self,
        header: &Header,
        ciphertext: &str,
        nostr_sender: &PublicKey,
    ) -> Result<String> {
        if let Some(plaintext) = self.try_skipped_message_keys(header, ciphertext, nostr_sender)? {
            return Ok(plaintext);
        }

        if self.state.receiving_chain_key.is_none() {
            return Err(Error::SessionNotReady);
        }

        self.skip_message_keys(header.number, nostr_sender)?;

        let receiving_chain_key = self.state.receiving_chain_key.unwrap();

        let kdf_outputs = kdf(&receiving_chain_key, &[1u8], 2);
        self.state.receiving_chain_key = Some(kdf_outputs[0]);
        let message_key = kdf_outputs[1];

        self.state.receiving_chain_message_number += 1;

        let conversation_key = nip44::v2::ConversationKey::new(message_key);
        let ciphertext_bytes = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|e| Error::Decryption(e.to_string()))?;

        let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
        String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))
    }

    fn ratchet_step(&mut self) -> Result<()> {
        self.state.previous_sending_chain_message_count = self.state.sending_chain_message_number;
        self.state.sending_chain_message_number = 0;
        self.state.receiving_chain_message_number = 0;

        let our_next_sk = nostr::SecretKey::from_slice(&self.state.our_next_nostr_key.private_key)?;
        let their_next_pk = self
            .state
            .their_next_nostr_public_key
            .ok_or(Error::SessionNotReady)?;

        let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_sk, &their_next_pk);
        let kdf_outputs = kdf(&self.state.root_key, conversation_key1.as_bytes(), 2);

        self.state.receiving_chain_key = Some(kdf_outputs[1]);

        self.state.our_current_nostr_key = Some(self.state.our_next_nostr_key.clone());

        let our_next_keys = nostr::Keys::generate();
        let our_next_private_key = our_next_keys.secret_key().to_secret_bytes();
        self.state.our_next_nostr_key = SerializableKeyPair {
            public_key: our_next_keys.public_key(),
            private_key: our_next_private_key,
        };

        let our_next_sk2 = nostr::SecretKey::from_slice(&our_next_private_key)?;
        let conversation_key2 = nip44::v2::ConversationKey::derive(&our_next_sk2, &their_next_pk);
        let kdf_outputs2 = kdf(&kdf_outputs[0], conversation_key2.as_bytes(), 2);

        self.state.root_key = kdf_outputs2[0];
        self.state.sending_chain_key = Some(kdf_outputs2[1]);

        Ok(())
    }

    fn skip_message_keys(&mut self, until: u32, nostr_sender: &PublicKey) -> Result<()> {
        if until <= self.state.receiving_chain_message_number {
            return Ok(());
        }

        if (until - self.state.receiving_chain_message_number) as usize > MAX_SKIP {
            return Err(Error::TooManySkippedMessages);
        }

        let entry = self
            .state
            .skipped_keys
            .entry(*nostr_sender)
            .or_insert_with(|| SkippedKeysEntry {
                header_keys: Vec::new(),
                message_keys: HashMap::new(),
            });

        while self.state.receiving_chain_message_number < until {
            let receiving_chain_key = self
                .state
                .receiving_chain_key
                .ok_or(Error::SessionNotReady)?;

            let kdf_outputs = kdf(&receiving_chain_key, &[1u8], 2);
            self.state.receiving_chain_key = Some(kdf_outputs[0]);

            entry
                .message_keys
                .insert(self.state.receiving_chain_message_number, kdf_outputs[1]);
            self.state.receiving_chain_message_number += 1;
        }

        Ok(())
    }

    fn try_skipped_message_keys(
        &mut self,
        header: &Header,
        ciphertext: &str,
        nostr_sender: &PublicKey,
    ) -> Result<Option<String>> {
        if let Some(entry) = self.state.skipped_keys.get_mut(nostr_sender) {
            if let Some(message_key) = entry.message_keys.remove(&header.number) {
                let conversation_key = nip44::v2::ConversationKey::new(message_key);
                let ciphertext_bytes = base64::engine::general_purpose::STANDARD
                    .decode(ciphertext)
                    .map_err(|e| Error::Decryption(e.to_string()))?;

                let plaintext_bytes =
                    nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
                let plaintext = String::from_utf8(plaintext_bytes)
                    .map_err(|e| Error::Decryption(e.to_string()))?;

                if entry.message_keys.is_empty() {
                    self.state.skipped_keys.remove(nostr_sender);
                }

                return Ok(Some(plaintext));
            }
        }
        Ok(None)
    }

    pub fn receive(&mut self, event: &nostr::Event) -> Result<Option<String>> {
        let header_tag = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("header"))
            .cloned();

        let encrypted_header = match header_tag {
            Some(tag) => {
                let v = tag.to_vec();
                v.get(1).ok_or(Error::InvalidHeader)?.clone()
            }
            None => return Err(Error::InvalidHeader),
        };

        let sender_pubkey = event.pubkey;
        let (header, should_ratchet) = self.decrypt_header(&encrypted_header, &sender_pubkey)?;

        let sender_bytes = sender_pubkey.to_bytes();
        let their_next_matches = self
            .state
            .their_next_nostr_public_key
            .as_ref()
            .map(|pk| pk.to_bytes() == sender_bytes)
            .unwrap_or(false);
        let their_current_matches = self
            .state
            .their_current_nostr_public_key
            .as_ref()
            .map(|pk| pk.to_bytes() == sender_bytes)
            .unwrap_or(false);

        if !their_next_matches && !their_current_matches {
            return Err(Error::InvalidEvent("Unexpected sender".to_string()));
        }

        let their_next_pk_hex = self
            .state
            .their_next_nostr_public_key
            .map(|pk| hex::encode(pk.to_bytes()))
            .unwrap_or_default();

        if header.next_public_key != their_next_pk_hex {
            self.state.their_current_nostr_public_key = self.state.their_next_nostr_public_key;
            self.state.their_next_nostr_public_key =
                Some(pubkey_from_hex(&header.next_public_key)?);
        }

        if should_ratchet {
            if self.state.receiving_chain_key.is_some() {
                self.skip_message_keys(header.previous_chain_length, &sender_pubkey)?;
            }
            self.ratchet_step()?;

            // Update subscriptions after ratchet (keys changed)
            let _ = self.update_subscriptions();
        }

        let plaintext = self.ratchet_decrypt(&header, &event.content, &sender_pubkey)?;
        Ok(Some(plaintext))
    }

    fn decrypt_header(&self, encrypted_header: &str, sender: &PublicKey) -> Result<(Header, bool)> {
        if let Some(current) = &self.state.our_current_nostr_key {
            let current_sk = nostr::SecretKey::from_slice(&current.private_key)?;

            if let Ok(decrypted) =
                nostr::nips::nip44::decrypt(&current_sk, sender, encrypted_header)
            {
                let header: Header = serde_json::from_str(&decrypted)
                    .map_err(|e| Error::Serialization(e.to_string()))?;
                return Ok((header, false));
            }
        }

        let next_sk = nostr::SecretKey::from_slice(&self.state.our_next_nostr_key.private_key)?;

        let decrypted = nostr::nips::nip44::decrypt(&next_sk, sender, encrypted_header)?;
        let header: Header =
            serde_json::from_str(&decrypted).map_err(|e| Error::Serialization(e.to_string()))?;
        Ok((header, true))
    }

    pub fn close(&self) {
        if let Some(unsub) = self.nostr_unsubscribe.lock().unwrap().take() {
            unsub();
        }
        if let Some(unsub) = self.nostr_next_unsubscribe.lock().unwrap().take() {
            unsub();
        }
        if let Some(unsub) = self.skipped_subscription.lock().unwrap().take() {
            unsub();
        }
        self.internal_subscriptions.lock().unwrap().clear();

        // Unsubscribe from session-managed subscriptions
        if let Some(event_tx) = &self.event_tx {
            if let Some(subid) = self.current_key_subid.lock().unwrap().take() {
                let _ = event_tx.send(crate::SessionManagerEvent::Unsubscribe(subid));
            }
            if let Some(subid) = self.next_key_subid.lock().unwrap().take() {
                let _ = event_tx.send(crate::SessionManagerEvent::Unsubscribe(subid));
            }
        }
    }
}
