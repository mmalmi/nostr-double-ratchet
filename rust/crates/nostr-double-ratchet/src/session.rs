use crate::{
    utils::{kdf, pubkey_from_hex},
    Error, Header, Result, SerializableKeyPair, SessionState, SkippedKeysEntry, MAX_SKIP,
    MESSAGE_EVENT_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::PublicKey;
use nostr::{Keys, Tag, Timestamp, UnsignedEvent};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub struct Session {
    pub state: SessionState,
    pub name: String,
}

impl Session {
    pub fn new(state: SessionState, name: String) -> Self {
        Self { state, name }
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
            )?;
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
        })
    }

    pub fn can_send(&self) -> bool {
        self.state.their_next_nostr_public_key.is_some()
            && self.state.our_current_nostr_key.is_some()
    }

    pub fn send_event(&mut self, mut event: UnsignedEvent) -> Result<nostr::Event> {
        if !self.can_send() {
            return Err(Error::NotInitiator);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let now_s = now.as_secs();

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
                .custom_created_at(Timestamp::from(now_s))
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
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext.as_bytes())?;
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

        let conversation_key1 = nip44::v2::ConversationKey::derive(&our_next_sk, &their_next_pk)?;
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
        let conversation_key2 = nip44::v2::ConversationKey::derive(&our_next_sk2, &their_next_pk)?;
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

        // Bound stored skipped keys to avoid unbounded memory growth when many messages are missed.
        prune_skipped_message_keys(&mut entry.message_keys);
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
        // Snapshot state so we can roll back on decryption failures (e.g. duplicates/replays).
        let snapshot = crate::utils::deep_copy_state(&self.state);

        let result = (|| {
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
            let (header, should_ratchet) =
                self.decrypt_header(&encrypted_header, &sender_pubkey)?;

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
            }

            let plaintext = self.ratchet_decrypt(&header, &event.content, &sender_pubkey)?;

            Ok(Some(normalize_inner_rumor_id(&plaintext)))
        })();

        if result.is_err() {
            self.state = snapshot;
        }

        result
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

    pub fn close(&self) {}
}

fn normalize_inner_rumor_id(plaintext: &str) -> String {
    let mut v: serde_json::Value = match serde_json::from_str(plaintext) {
        Ok(v) => v,
        Err(_) => return plaintext.to_string(),
    };

    let obj = match v.as_object_mut() {
        Some(obj) => obj,
        None => return plaintext.to_string(),
    };

    let pubkey = match obj.get("pubkey").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return plaintext.to_string(),
    };

    let created_at = match obj.get("created_at").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return plaintext.to_string(),
    };

    let kind = match obj.get("kind").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return plaintext.to_string(),
    };

    let content = match obj.get("content").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return plaintext.to_string(),
    };

    let tags_value = match obj.get("tags").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return plaintext.to_string(),
    };

    // NIP-01 expects tags to be an array of string arrays. If it's not, keep the plaintext as-is.
    let mut tags: Vec<Vec<String>> = Vec::with_capacity(tags_value.len());
    for tag in tags_value {
        let arr = match tag.as_array() {
            Some(arr) => arr,
            None => return plaintext.to_string(),
        };
        let mut out: Vec<String> = Vec::with_capacity(arr.len());
        for v in arr {
            let s = match v.as_str() {
                Some(s) => s,
                None => return plaintext.to_string(),
            };
            out.push(s.to_string());
        }
        tags.push(out);
    }

    // NIP-01 event id hash is sha256(JSON.stringify([0,pubkey,created_at,kind,tags,content])).
    let canonical = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let canonical_json = match serde_json::to_string(&canonical) {
        Ok(s) => s,
        Err(_) => return plaintext.to_string(),
    };

    let computed = hex::encode(Sha256::digest(canonical_json.as_bytes()));
    obj.insert("id".to_string(), serde_json::Value::String(computed));

    serde_json::to_string(&v).unwrap_or_else(|_| plaintext.to_string())
}

fn prune_skipped_message_keys(map: &mut HashMap<u32, [u8; 32]>) {
    if map.len() <= MAX_SKIP {
        return;
    }

    // Drop the oldest skipped keys first (smallest message numbers).
    // This sacrifices decrypting very old out-of-order messages in exchange for bounded memory.
    let mut keys: Vec<u32> = map.keys().copied().collect();
    keys.sort_unstable();
    let to_remove = map.len().saturating_sub(MAX_SKIP);
    for k in keys.into_iter().take(to_remove) {
        map.remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_message_keys_prunes_to_max_skip() {
        let our_keys = Keys::generate();
        let our_next = SerializableKeyPair {
            public_key: our_keys.public_key(),
            private_key: our_keys.secret_key().to_secret_bytes(),
        };

        let mut session = Session::new(
            SessionState {
                root_key: [0u8; 32],
                their_current_nostr_public_key: None,
                their_next_nostr_public_key: None,
                our_current_nostr_key: None,
                our_next_nostr_key: our_next,
                receiving_chain_key: Some([7u8; 32]),
                sending_chain_key: None,
                sending_chain_message_number: 0,
                receiving_chain_message_number: 0,
                previous_sending_chain_message_count: 0,
                skipped_keys: HashMap::new(),
            },
            "test".to_string(),
        );

        let sender = Keys::generate().public_key();

        session.skip_message_keys(MAX_SKIP as u32, &sender).unwrap();
        session
            .skip_message_keys((MAX_SKIP * 2) as u32, &sender)
            .unwrap();

        let entry = session.state.skipped_keys.get(&sender).unwrap();
        assert!(
            entry.message_keys.len() <= MAX_SKIP,
            "expected skipped keys to be pruned to MAX_SKIP"
        );
        // Oldest key should be gone; newest should remain.
        assert!(!entry.message_keys.contains_key(&0));
        assert!(entry
            .message_keys
            .contains_key(&((MAX_SKIP * 2 - 1) as u32)));
    }
}
