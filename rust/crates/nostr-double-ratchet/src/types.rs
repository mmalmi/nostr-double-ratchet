use nostr::PublicKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MESSAGE_EVENT_KIND: u32 = 1060;
pub const INVITE_EVENT_KIND: u32 = 30078;
pub const APP_KEYS_EVENT_KIND: u32 = 30078;
pub const INVITE_RESPONSE_KIND: u32 = 1059;
pub const CHAT_MESSAGE_KIND: u32 = 14;
pub const REACTION_KIND: u32 = 7;
pub const RECEIPT_KIND: u32 = 15;
pub const TYPING_KIND: u32 = 25;
pub const SHARED_CHANNEL_KIND: u32 = 4;
pub const MAX_SKIP: usize = 1000;

/// NIP-40-style expiration tag name.
///
/// For disappearing messages, include this tag in the *inner* rumor event:
/// `["expiration", "<unix seconds>"]`.
///
/// Note: Purging/deleting expired messages is the responsibility of the client/storage.
pub const EXPIRATION_TAG: &str = "expiration";

#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// UNIX timestamp in seconds when the message should expire.
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub number: u32,
    pub previous_chain_length: u32,
    pub next_public_key: String,
}

#[derive(Debug, Clone)]
pub struct KeyPair {
    pub public_key: PublicKey,
    pub private_key: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(with = "serde_bytes_array")]
    pub root_key: [u8; 32],

    #[serde(with = "serde_option_pubkey", default)]
    pub their_current_nostr_public_key: Option<PublicKey>,
    #[serde(with = "serde_option_pubkey", default)]
    pub their_next_nostr_public_key: Option<PublicKey>,

    pub our_current_nostr_key: Option<SerializableKeyPair>,
    pub our_next_nostr_key: SerializableKeyPair,

    #[serde(with = "serde_option_bytes_array", default)]
    pub receiving_chain_key: Option<[u8; 32]>,

    #[serde(with = "serde_option_bytes_array", default)]
    pub sending_chain_key: Option<[u8; 32]>,

    pub sending_chain_message_number: u32,
    pub receiving_chain_message_number: u32,
    pub previous_sending_chain_message_count: u32,

    #[serde(with = "serde_pubkey_hashmap")]
    pub skipped_keys: HashMap<PublicKey, SkippedKeysEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableKeyPair {
    #[serde(with = "serde_pubkey")]
    pub public_key: PublicKey,
    #[serde(with = "serde_bytes_array")]
    pub private_key: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedKeysEntry {
    #[serde(with = "serde_vec_bytes_array")]
    pub header_keys: Vec<[u8; 32]>,

    #[serde(with = "serde_hashmap_u32_bytes")]
    pub message_keys: HashMap<u32, [u8; 32]>,
}

pub type Unsubscribe = Box<dyn FnOnce() + Send>;

pub type EventCallback = Box<dyn Fn(nostr::Event, nostr::Event) + Send>;

mod serde_pubkey {
    use nostr::PublicKey;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(pk: &PublicKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(pk.to_bytes()))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PublicKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        PublicKey::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}

mod serde_option_pubkey {
    use nostr::PublicKey;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(pk: &Option<PublicKey>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match pk {
            Some(p) => serializer.serialize_str(&hex::encode(p.to_bytes())),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<PublicKey>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => {
                let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
                Ok(Some(
                    PublicKey::from_slice(&bytes).map_err(serde::de::Error::custom)?,
                ))
            }
            None => Ok(None),
        }
    }
}

mod serde_pubkey_hashmap {
    use nostr::PublicKey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(
        map: &HashMap<PublicKey, super::SkippedKeysEntry>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let string_map: HashMap<String, &super::SkippedKeysEntry> = map
            .iter()
            .map(|(k, v)| (hex::encode(k.to_bytes()), v))
            .collect();
        string_map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<PublicKey, super::SkippedKeysEntry>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let string_map: HashMap<String, super::SkippedKeysEntry> =
            HashMap::deserialize(deserializer)?;
        string_map
            .into_iter()
            .map(|(k, v)| {
                let bytes = hex::decode(&k).map_err(serde::de::Error::custom)?;
                let pk = PublicKey::from_slice(&bytes).map_err(serde::de::Error::custom)?;
                Ok((pk, v))
            })
            .collect()
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
        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(array)
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
            Some(s) => {
                let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Ok(Some(array))
            }
            None => Ok(None),
        }
    }
}

mod serde_vec_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(vec: &Vec<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(vec.len()))?;
        for bytes in vec {
            seq.serialize_element(&hex::encode(bytes))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec: Vec<String> = Vec::deserialize(deserializer)?;
        vec.into_iter()
            .map(|s| {
                let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Ok(array)
            })
            .collect()
    }
}

mod serde_hashmap_u32_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(map: &HashMap<u32, [u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map_serializer = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            map_serializer.serialize_entry(k, &hex::encode(v))?;
        }
        map_serializer.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<u32, [u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let map: HashMap<u32, String> = HashMap::deserialize(deserializer)?;
        map.into_iter()
            .map(|(k, v)| {
                let bytes = hex::decode(&v).map_err(serde::de::Error::custom)?;
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Ok((k, array))
            })
            .collect()
    }
}
