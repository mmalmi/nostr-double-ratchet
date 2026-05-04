use crate::{Delivery, DevicePubkey, InviteResponseEnvelope, OwnerPubkey, RelayGap, UnixSeconds};
use serde::{Deserialize, Deserializer, Serialize};

pub type SenderEventPubkey = DevicePubkey;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupStrategy {
    PairwiseFanout,
    SenderKey,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct GroupProtocol {
    pub strategy: GroupStrategy,
    pub version: u16,
}

impl GroupProtocol {
    #[allow(non_upper_case_globals)]
    pub const PairwiseFanoutV1: Self = Self::pairwise_fanout_v1();
    #[allow(non_upper_case_globals)]
    pub const SenderKeyV1: Self = Self::sender_key_v1();

    pub const fn pairwise_fanout_v1() -> Self {
        Self {
            strategy: GroupStrategy::PairwiseFanout,
            version: 1,
        }
    }

    pub const fn sender_key_v1() -> Self {
        Self {
            strategy: GroupStrategy::SenderKey,
            version: 1,
        }
    }

    pub fn is_pairwise_fanout_v1(self) -> bool {
        self == Self::pairwise_fanout_v1()
    }

    pub fn is_sender_key_v1(self) -> bool {
        self == Self::sender_key_v1()
    }
}

impl<'de> Deserialize<'de> for GroupProtocol {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Legacy(String),
            Descriptor {
                strategy: GroupStrategy,
                version: u16,
            },
        }

        match Wire::deserialize(deserializer)? {
            Wire::Legacy(value) if value == "pairwise_fanout_v1" => {
                Ok(GroupProtocol::pairwise_fanout_v1())
            }
            Wire::Legacy(value) if value == "sender_key_v1" => Ok(GroupProtocol::sender_key_v1()),
            Wire::Legacy(value) => Err(serde::de::Error::custom(format!(
                "unsupported group protocol `{value}`"
            ))),
            Wire::Descriptor { strategy, version } => Ok(GroupProtocol { strategy, version }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupSnapshot {
    pub group_id: String,
    pub protocol: GroupProtocol,
    pub name: String,
    pub created_by: OwnerPubkey,
    pub members: Vec<OwnerPubkey>,
    pub admins: Vec<OwnerPubkey>,
    pub revision: u64,
    pub created_at: UnixSeconds,
    pub updated_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupManagerSnapshot {
    pub local_owner_pubkey: OwnerPubkey,
    pub groups: Vec<GroupSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender_keys: Vec<GroupSenderKeyRecordSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPreparedPublish {
    pub deliveries: Vec<Delivery>,
    pub invite_responses: Vec<InviteResponseEnvelope>,
    pub sender_key_messages: Vec<GroupSenderKeyMessageEnvelope>,
    pub relay_gaps: Vec<RelayGap>,
    pub pending_fanouts: Vec<GroupPendingFanout>,
}

impl GroupPreparedPublish {
    pub fn empty() -> Self {
        Self {
            deliveries: Vec::new(),
            invite_responses: Vec::new(),
            sender_key_messages: Vec::new(),
            relay_gaps: Vec::new(),
            pending_fanouts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroupPendingFanout {
    Remote {
        recipient_owner: OwnerPubkey,
        payload: Vec<u8>,
    },
    LocalSiblings {
        payload: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPreparedSend {
    pub group_id: String,
    pub remote: GroupPreparedPublish,
    pub local_sibling: GroupPreparedPublish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCreateResult {
    pub group: GroupSnapshot,
    pub prepared: GroupPreparedSend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupPayloadEncodeContext {
    pub local_device_pubkey: DevicePubkey,
    pub created_at: UnixSeconds,
}

pub trait GroupPayloadCodec: Clone {
    fn is_pairwise_payload(&self, payload: &[u8]) -> bool;

    fn encode_pairwise_command(
        &self,
        ctx: GroupPayloadEncodeContext,
        command: &GroupPairwiseCommand,
    ) -> crate::Result<Vec<u8>>;

    fn decode_pairwise_command(
        &self,
        payload: &[u8],
    ) -> crate::Result<Option<GroupPairwiseCommand>>;

    fn encode_sender_key_plaintext(
        &self,
        plaintext: &GroupSenderKeyPlaintext,
    ) -> crate::Result<Vec<u8>>;

    fn decode_sender_key_plaintext(
        &self,
        payload: &[u8],
    ) -> crate::Result<Option<GroupSenderKeyPlaintext>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupPairwiseCommand {
    MetadataSnapshot {
        snapshot: GroupSnapshot,
    },
    GroupMessage {
        group_id: String,
        revision: u64,
        body: Vec<u8>,
    },
    SenderKeyDistribution {
        distribution: SenderKeyDistribution,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSenderKeyPlaintext {
    pub group_id: String,
    pub revision: u64,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReceivedMessage {
    pub group_id: String,
    pub sender_owner: OwnerPubkey,
    pub sender_device: Option<DevicePubkey>,
    pub body: Vec<u8>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupIncomingEvent {
    MetadataUpdated(GroupSnapshot),
    Message(GroupReceivedMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupSenderKeyRecordSnapshot {
    pub group_id: String,
    pub sender_owner: OwnerPubkey,
    pub sender_device: DevicePubkey,
    pub sender_event_pubkey: SenderEventPubkey,
    #[serde(default, with = "serde_option_bytes_array")]
    pub sender_event_secret_key: Option<[u8; 32]>,
    pub latest_key_id: Option<u32>,
    pub states: Vec<crate::SenderKeyState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SenderKeyDistribution {
    pub group_id: String,
    pub key_id: u32,
    pub sender_event_pubkey: SenderEventPubkey,
    #[serde(with = "serde_bytes_array")]
    pub chain_key: [u8; 32],
    pub iteration: u32,
    pub created_at: UnixSeconds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupSenderKeyMessageEnvelope {
    pub group_id: String,
    pub sender_event_pubkey: SenderEventPubkey,
    #[serde(with = "serde_bytes_array")]
    pub signer_secret_key: [u8; 32],
    pub key_id: u32,
    pub message_number: u32,
    pub created_at: UnixSeconds,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupSenderKeyMessage {
    pub group_id: String,
    pub sender_event_pubkey: SenderEventPubkey,
    pub key_id: u32,
    pub message_number: u32,
    pub created_at: UnixSeconds,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupSenderKeyHandleResult {
    Event(GroupIncomingEvent),
    PendingDistribution {
        group_id: String,
        sender_event_pubkey: SenderEventPubkey,
        key_id: u32,
    },
    PendingRevision {
        group_id: String,
        current_revision: u64,
        required_revision: u64,
    },
    Ignored,
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
        let value = String::deserialize(deserializer)?;
        let bytes = hex::decode(value).map_err(serde::de::Error::custom)?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| serde::de::Error::custom("expected 32-byte hex"))
    }
}

mod serde_option_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match bytes {
            Some(bytes) => serializer.serialize_str(&hex::encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|value| {
                let bytes = hex::decode(value).map_err(serde::de::Error::custom)?;
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map_err(|_| serde::de::Error::custom("expected 32-byte hex"))
            })
            .transpose()
    }
}
