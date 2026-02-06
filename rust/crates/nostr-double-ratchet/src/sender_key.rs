use base64::Engine;
use nostr::nips::nip44;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{Error, Result};

/// Maximum number of message keys we will derive-and-cache to support out-of-order delivery.
///
/// This is separate from the 1:1 Double Ratchet MAX_SKIP, because group chats can legitimately
/// have higher volume and different delivery patterns.
pub const SENDER_KEY_MAX_SKIP: usize = 10_000;

/// Bound the amount of cached skipped message keys to limit memory/CPU DoS.
pub const SENDER_KEY_MAX_STORED_SKIPPED_KEYS: usize = 2_000;

const SENDER_KEY_KDF_SALT: &[u8] = b"ndr-sender-key-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SenderKeyDistribution {
    pub group_id: String,
    pub key_id: u32,
    #[serde(with = "serde_bytes_array")]
    pub chain_key: [u8; 32],
    pub iteration: u32,
    pub created_at: u64,
}

impl SenderKeyDistribution {
    pub fn new(group_id: String, key_id: u32, chain_key: [u8; 32], iteration: u32) -> Self {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            group_id,
            key_id,
            chain_key,
            iteration,
            created_at,
        }
    }

    pub fn new_random(group_id: String, key_id: u32) -> Self {
        Self::new(group_id, key_id, rand::random::<[u8; 32]>(), 0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SenderKeyState {
    pub key_id: u32,
    #[serde(with = "serde_bytes_array")]
    chain_key: [u8; 32],
    iteration: u32,
    #[serde(with = "serde_hashmap_u32_bytes", default)]
    skipped_message_keys: HashMap<u32, [u8; 32]>,
}

impl SenderKeyState {
    pub fn new(key_id: u32, chain_key: [u8; 32], iteration: u32) -> Self {
        Self {
            key_id,
            chain_key,
            iteration,
            skipped_message_keys: HashMap::new(),
        }
    }

    pub fn chain_key(&self) -> [u8; 32] {
        self.chain_key
    }

    pub fn iteration(&self) -> u32 {
        self.iteration
    }

    pub fn skipped_len(&self) -> usize {
        self.skipped_message_keys.len()
    }

    pub fn encrypt(&mut self, plaintext: &str) -> Result<(u32, String)> {
        let message_number = self.iteration;
        let (next_chain_key, message_key) = derive_message_key(&self.chain_key);

        self.chain_key = next_chain_key;
        self.iteration = self.iteration.saturating_add(1);

        let conversation_key = nip44::v2::ConversationKey::new(message_key);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, plaintext)?;
        let ciphertext = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);

        Ok((message_number, ciphertext))
    }

    pub fn decrypt(&mut self, message_number: u32, ciphertext: &str) -> Result<String> {
        // Old message: try cached skipped key.
        if message_number < self.iteration {
            let message_key = self
                .skipped_message_keys
                .remove(&message_number)
                .ok_or_else(|| {
                    Error::Decryption("Missing skipped sender key message".to_string())
                })?;

            return decrypt_with_message_key(&message_key, ciphertext);
        }

        // Fast-fail if the sender is too far ahead.
        let delta = (message_number - self.iteration) as usize;
        if delta > SENDER_KEY_MAX_SKIP {
            return Err(Error::TooManySkippedMessages);
        }

        // Derive and cache keys for skipped messages so we can decrypt out-of-order later.
        while self.iteration < message_number {
            let (next_chain_key, message_key) = derive_message_key(&self.chain_key);
            self.chain_key = next_chain_key;
            self.skipped_message_keys
                .insert(self.iteration, message_key);
            self.iteration = self.iteration.saturating_add(1);
        }

        // Now decrypt the current message using the next derived key.
        let (next_chain_key, message_key) = derive_message_key(&self.chain_key);
        self.chain_key = next_chain_key;
        self.iteration = self.iteration.saturating_add(1);

        // Prune skipped cache if it grows unbounded.
        prune_skipped(&mut self.skipped_message_keys);

        decrypt_with_message_key(&message_key, ciphertext)
    }
}

fn decrypt_with_message_key(message_key: &[u8; 32], ciphertext: &str) -> Result<String> {
    let conversation_key = nip44::v2::ConversationKey::new(*message_key);
    let ciphertext_bytes = base64::engine::general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| Error::Decryption(e.to_string()))?;

    let plaintext_bytes = nip44::v2::decrypt_to_bytes(&conversation_key, &ciphertext_bytes)?;
    String::from_utf8(plaintext_bytes).map_err(|e| Error::Decryption(e.to_string()))
}

fn derive_message_key(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let outputs = crate::utils::kdf(chain_key, SENDER_KEY_KDF_SALT, 2);
    (outputs[0], outputs[1])
}

fn prune_skipped(map: &mut HashMap<u32, [u8; 32]>) {
    if map.len() <= SENDER_KEY_MAX_STORED_SKIPPED_KEYS {
        return;
    }

    // Remove oldest first (smallest message number).
    let mut keys: Vec<u32> = map.keys().cloned().collect();
    keys.sort_unstable();
    let to_remove = map.len().saturating_sub(SENDER_KEY_MAX_STORED_SKIPPED_KEYS);
    for k in keys.into_iter().take(to_remove) {
        map.remove(&k);
    }
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
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("Invalid 32-byte hex"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

mod serde_hashmap_u32_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(map: &HashMap<u32, [u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_map: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.to_string(), hex::encode(v)))
            .collect();
        string_map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<u32, [u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_map: HashMap<String, String> = HashMap::deserialize(deserializer)?;
        let mut out = HashMap::new();
        for (k, v) in string_map {
            let idx: u32 = k.parse().map_err(serde::de::Error::custom)?;
            let bytes = hex::decode(&v).map_err(serde::de::Error::custom)?;
            if bytes.len() != 32 {
                return Err(serde::de::Error::custom("Invalid 32-byte hex"));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            out.insert(idx, arr);
        }
        Ok(out)
    }
}
