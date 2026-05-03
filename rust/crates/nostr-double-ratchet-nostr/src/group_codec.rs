use nostr_double_ratchet::{
    GroupPairwiseCommand, GroupPayloadCodec, GroupProtocol, GroupSenderKeyPlaintext, OwnerPubkey,
    Result, SenderKeyDistribution, UnixSeconds,
};
use serde::{Deserialize, Serialize};

const GROUP_WIRE_FORMAT_VERSION_V1: u8 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct JsonGroupPayloadCodecV1;

pub type NostrGroupManager = nostr_double_ratchet::GroupManager<JsonGroupPayloadCodecV1>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupWireEnvelopeV1 {
    wire_format_version: u8,
    payload: GroupPairwisePayloadV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum GroupPairwisePayloadV1 {
    CreateGroup {
        group_id: String,
        protocol: GroupProtocol,
        base_revision: u64,
        new_revision: u64,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
    },
    SyncGroup {
        group_id: String,
        protocol: GroupProtocol,
        revision: u64,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
    },
    RenameGroup {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        name: String,
    },
    AddMembers {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        members: Vec<OwnerPubkey>,
    },
    RemoveMembers {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        members: Vec<OwnerPubkey>,
    },
    AddAdmins {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        admins: Vec<OwnerPubkey>,
    },
    RemoveAdmins {
        group_id: String,
        base_revision: u64,
        new_revision: u64,
        admins: Vec<OwnerPubkey>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupSenderKeyPlaintextV1 {
    wire_format_version: u8,
    group_id: String,
    revision: u64,
    body: Vec<u8>,
}

impl GroupPayloadCodec for JsonGroupPayloadCodecV1 {
    fn is_pairwise_payload(&self, payload: &[u8]) -> bool {
        serde_json::from_slice::<GroupWireEnvelopeV1>(payload)
            .map(|envelope| envelope.wire_format_version == GROUP_WIRE_FORMAT_VERSION_V1)
            .unwrap_or(false)
    }

    fn encode_pairwise_command(&self, command: &GroupPairwiseCommand) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&GroupWireEnvelopeV1 {
            wire_format_version: GROUP_WIRE_FORMAT_VERSION_V1,
            payload: GroupPairwisePayloadV1::from(command.clone()),
        })?)
    }

    fn decode_pairwise_command(&self, payload: &[u8]) -> Result<Option<GroupPairwiseCommand>> {
        let Ok(envelope) = serde_json::from_slice::<GroupWireEnvelopeV1>(payload) else {
            return Ok(None);
        };
        if envelope.wire_format_version != GROUP_WIRE_FORMAT_VERSION_V1 {
            return Ok(None);
        }
        Ok(Some(envelope.payload.into()))
    }

    fn encode_sender_key_plaintext(&self, plaintext: &GroupSenderKeyPlaintext) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&GroupSenderKeyPlaintextV1 {
            wire_format_version: GROUP_WIRE_FORMAT_VERSION_V1,
            group_id: plaintext.group_id.clone(),
            revision: plaintext.revision,
            body: plaintext.body.clone(),
        })?)
    }

    fn decode_sender_key_plaintext(
        &self,
        payload: &[u8],
    ) -> Result<Option<GroupSenderKeyPlaintext>> {
        let Ok(plaintext) = serde_json::from_slice::<GroupSenderKeyPlaintextV1>(payload) else {
            return Ok(None);
        };
        if plaintext.wire_format_version != GROUP_WIRE_FORMAT_VERSION_V1 {
            return Ok(None);
        }
        Ok(Some(GroupSenderKeyPlaintext {
            group_id: plaintext.group_id,
            revision: plaintext.revision,
            body: plaintext.body,
        }))
    }
}

impl From<GroupPairwiseCommand> for GroupPairwisePayloadV1 {
    fn from(command: GroupPairwiseCommand) -> Self {
        match command {
            GroupPairwiseCommand::CreateGroup {
                group_id,
                protocol,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => Self::CreateGroup {
                group_id,
                protocol,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            },
            GroupPairwiseCommand::SyncGroup {
                group_id,
                protocol,
                revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => Self::SyncGroup {
                group_id,
                protocol,
                revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            },
            GroupPairwiseCommand::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            } => Self::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            },
            GroupPairwiseCommand::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => Self::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            },
            GroupPairwiseCommand::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => Self::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            },
            GroupPairwiseCommand::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => Self::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            },
            GroupPairwiseCommand::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => Self::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            },
            GroupPairwiseCommand::GroupMessage {
                group_id,
                revision,
                body,
            } => Self::GroupMessage {
                group_id,
                revision,
                body,
            },
            GroupPairwiseCommand::SenderKeyDistribution { distribution } => {
                Self::SenderKeyDistribution { distribution }
            }
        }
    }
}

impl From<GroupPairwisePayloadV1> for GroupPairwiseCommand {
    fn from(payload: GroupPairwisePayloadV1) -> Self {
        match payload {
            GroupPairwisePayloadV1::CreateGroup {
                group_id,
                protocol,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => Self::CreateGroup {
                group_id,
                protocol,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            },
            GroupPairwisePayloadV1::SyncGroup {
                group_id,
                protocol,
                revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => Self::SyncGroup {
                group_id,
                protocol,
                revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            },
            GroupPairwisePayloadV1::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            } => Self::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            },
            GroupPairwisePayloadV1::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => Self::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            },
            GroupPairwisePayloadV1::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => Self::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            },
            GroupPairwisePayloadV1::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => Self::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            },
            GroupPairwisePayloadV1::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => Self::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            },
            GroupPairwisePayloadV1::GroupMessage {
                group_id,
                revision,
                body,
            } => Self::GroupMessage {
                group_id,
                revision,
                body,
            },
            GroupPairwisePayloadV1::SenderKeyDistribution { distribution } => {
                Self::SenderKeyDistribution { distribution }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_double_ratchet::{DevicePubkey, GroupProtocol};

    fn owner(byte: u8) -> OwnerPubkey {
        OwnerPubkey::from_bytes([byte; 32])
    }

    fn device(byte: u8) -> DevicePubkey {
        DevicePubkey::from_bytes([byte; 32])
    }

    #[test]
    fn pairwise_command_roundtrips_through_versioned_json_envelope() {
        let codec = JsonGroupPayloadCodecV1;
        let command = GroupPairwiseCommand::CreateGroup {
            group_id: "group-1".to_string(),
            protocol: GroupProtocol::sender_key_v1(),
            base_revision: 0,
            new_revision: 1,
            name: "Team".to_string(),
            created_by: owner(1),
            members: vec![owner(1), owner(2)],
            admins: vec![owner(1)],
            created_at: UnixSeconds(10),
            updated_at: UnixSeconds(10),
        };

        let encoded = codec.encode_pairwise_command(&command).unwrap();

        assert!(codec.is_pairwise_payload(&encoded));
        assert_eq!(
            codec.decode_pairwise_command(&encoded).unwrap(),
            Some(command)
        );
    }

    #[test]
    fn unsupported_pairwise_version_is_not_consumed_as_group_payload() {
        let codec = JsonGroupPayloadCodecV1;
        let encoded = serde_json::to_vec(&serde_json::json!({
            "wire_format_version": 255,
            "payload": {
                "kind": "group_message",
                "group_id": "group-1",
                "revision": 1,
                "body": []
            }
        }))
        .unwrap();

        assert!(!codec.is_pairwise_payload(&encoded));
        assert_eq!(codec.decode_pairwise_command(&encoded).unwrap(), None);
    }

    #[test]
    fn sender_key_plaintext_roundtrips_through_versioned_json_envelope() {
        let codec = JsonGroupPayloadCodecV1;
        let plaintext = GroupSenderKeyPlaintext {
            group_id: "group-1".to_string(),
            revision: 3,
            body: b"hello group".to_vec(),
        };

        let encoded = codec.encode_sender_key_plaintext(&plaintext).unwrap();

        assert_eq!(
            codec.decode_sender_key_plaintext(&encoded).unwrap(),
            Some(plaintext)
        );
    }

    #[test]
    fn sender_key_distribution_command_roundtrips() {
        let codec = JsonGroupPayloadCodecV1;
        let distribution = SenderKeyDistribution {
            group_id: "group-1".to_string(),
            key_id: 7,
            sender_event_pubkey: device(3),
            chain_key: [4; 32],
            iteration: 9,
            created_at: UnixSeconds(11),
        };
        let command = GroupPairwiseCommand::SenderKeyDistribution {
            distribution: distribution.clone(),
        };

        let encoded = codec.encode_pairwise_command(&command).unwrap();

        assert_eq!(
            codec.decode_pairwise_command(&encoded).unwrap(),
            Some(command)
        );
    }
}
