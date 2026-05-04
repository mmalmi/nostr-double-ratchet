use nostr::{EventBuilder, Kind, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    Error, GroupPairwiseCommand, GroupPayloadCodec, GroupPayloadEncodeContext, GroupProtocol,
    GroupSenderKeyPlaintext, GroupSnapshot, OwnerPubkey, Result, SenderKeyDistribution,
    UnixSeconds,
};
use serde::{Deserialize, Serialize};

pub const GROUP_METADATA_KIND: u32 = 40;

const GROUP_WIRE_FORMAT_VERSION_V1: u8 = 1;
const GROUP_LABEL_TAG: &str = "l";
const MS_TAG: &str = "ms";

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
    MetadataSnapshot {
        snapshot: GroupSnapshot,
    },
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
struct MasterGroupMetadataContent {
    id: String,
    name: String,
    members: Vec<OwnerPubkey>,
    admins: Vec<OwnerPubkey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<GroupProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<u64>,
    #[serde(
        rename = "createdBy",
        alias = "created_by",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    created_by: Option<OwnerPubkey>,
    #[serde(
        rename = "createdAt",
        alias = "created_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    created_at: Option<UnixSeconds>,
    #[serde(
        rename = "updatedAt",
        alias = "updated_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    updated_at: Option<UnixSeconds>,
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
        self.decode_pairwise_command(payload)
            .map(|command| command.is_some())
            .unwrap_or(false)
    }

    fn encode_pairwise_command(
        &self,
        ctx: GroupPayloadEncodeContext,
        command: &GroupPairwiseCommand,
    ) -> Result<Vec<u8>> {
        match command {
            GroupPairwiseCommand::MetadataSnapshot { snapshot } => {
                encode_master_metadata_snapshot(ctx, snapshot)
            }
            GroupPairwiseCommand::GroupMessage {
                group_id,
                revision,
                body,
            } => encode_envelope(GroupPairwisePayloadV1::GroupMessage {
                group_id: group_id.clone(),
                revision: *revision,
                body: body.clone(),
            }),
            GroupPairwiseCommand::SenderKeyDistribution { distribution } => {
                encode_envelope(GroupPairwisePayloadV1::SenderKeyDistribution {
                    distribution: distribution.clone(),
                })
            }
        }
    }

    fn decode_pairwise_command(&self, payload: &[u8]) -> Result<Option<GroupPairwiseCommand>> {
        if let Some(command) = decode_master_metadata_snapshot(payload)? {
            return Ok(Some(command));
        }

        let Ok(envelope) = serde_json::from_slice::<GroupWireEnvelopeV1>(payload) else {
            return Ok(None);
        };
        if envelope.wire_format_version != GROUP_WIRE_FORMAT_VERSION_V1 {
            return Ok(None);
        }
        Ok(Some(command_from_v1_payload(envelope.payload)?))
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

fn encode_master_metadata_snapshot(
    ctx: GroupPayloadEncodeContext,
    snapshot: &GroupSnapshot,
) -> Result<Vec<u8>> {
    let content = serde_json::to_string(&MasterGroupMetadataContent {
        id: snapshot.group_id.clone(),
        name: snapshot.name.clone(),
        members: snapshot.members.clone(),
        admins: snapshot.admins.clone(),
        protocol: Some(snapshot.protocol),
        revision: Some(snapshot.revision),
        created_by: Some(snapshot.created_by),
        created_at: Some(snapshot.created_at),
        updated_at: Some(snapshot.updated_at),
    })?;
    let millis = ctx.created_at.get().saturating_mul(1000).to_string();
    let event = EventBuilder::new(Kind::from(GROUP_METADATA_KIND as u16), content)
        .tags(vec![
            tag([GROUP_LABEL_TAG, snapshot.group_id.as_str()])?,
            tag([MS_TAG, millis.as_str()])?,
        ])
        .custom_created_at(Timestamp::from(ctx.created_at.get()))
        .build(ctx.local_device_pubkey.to_nostr()?);
    Ok(serde_json::to_vec(&event)?)
}

fn decode_master_metadata_snapshot(payload: &[u8]) -> Result<Option<GroupPairwiseCommand>> {
    let Ok(event) = serde_json::from_slice::<UnsignedEvent>(payload) else {
        return Ok(None);
    };
    if event.kind.as_u16() as u32 != GROUP_METADATA_KIND {
        return Ok(None);
    }

    let content = serde_json::from_str::<MasterGroupMetadataContent>(&event.content)?;
    let tagged_group_id = first_tag_value(&event, GROUP_LABEL_TAG);
    let group_id = tagged_group_id.unwrap_or_else(|| content.id.clone());
    if group_id != content.id {
        return Err(Error::Parse("group metadata id/tag mismatch".to_string()));
    }

    let created_at = content
        .created_at
        .unwrap_or_else(|| UnixSeconds(event.created_at.as_secs()));
    let updated_at = content
        .updated_at
        .unwrap_or_else(|| UnixSeconds(event.created_at.as_secs()));
    let revision = content
        .revision
        .or_else(|| first_tag_value(&event, MS_TAG).and_then(|value| value.parse::<u64>().ok()))
        .unwrap_or_else(|| event.created_at.as_secs())
        .max(1);
    let created_by = content
        .created_by
        .or_else(|| content.admins.first().copied())
        .ok_or_else(|| Error::Parse("group metadata missing creator/admin".to_string()))?;

    Ok(Some(GroupPairwiseCommand::MetadataSnapshot {
        snapshot: GroupSnapshot {
            group_id,
            protocol: content
                .protocol
                .unwrap_or_else(GroupProtocol::sender_key_v1),
            name: content.name,
            created_by,
            members: content.members,
            admins: content.admins,
            revision,
            created_at,
            updated_at,
        },
    }))
}

fn encode_envelope(payload: GroupPairwisePayloadV1) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&GroupWireEnvelopeV1 {
        wire_format_version: GROUP_WIRE_FORMAT_VERSION_V1,
        payload,
    })?)
}

fn command_from_v1_payload(payload: GroupPairwisePayloadV1) -> Result<GroupPairwiseCommand> {
    Ok(match payload {
        GroupPairwisePayloadV1::MetadataSnapshot { snapshot } => {
            GroupPairwiseCommand::MetadataSnapshot { snapshot }
        }
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
        } => {
            if base_revision != 0 {
                return Err(Error::Parse(
                    "create group base revision must be 0".to_string(),
                ));
            }
            GroupPairwiseCommand::MetadataSnapshot {
                snapshot: GroupSnapshot {
                    group_id,
                    protocol,
                    name,
                    created_by,
                    members,
                    admins,
                    revision: new_revision,
                    created_at,
                    updated_at,
                },
            }
        }
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
        } => GroupPairwiseCommand::MetadataSnapshot {
            snapshot: GroupSnapshot {
                group_id,
                protocol,
                name,
                created_by,
                members,
                admins,
                revision,
                created_at,
                updated_at,
            },
        },
        GroupPairwisePayloadV1::GroupMessage {
            group_id,
            revision,
            body,
        } => GroupPairwiseCommand::GroupMessage {
            group_id,
            revision,
            body,
        },
        GroupPairwisePayloadV1::SenderKeyDistribution { distribution } => {
            GroupPairwiseCommand::SenderKeyDistribution { distribution }
        }
    })
}

fn first_tag_value(event: &UnsignedEvent, key: &str) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let values = tag.as_slice();
        if values.first().map(|value| value.as_str()) != Some(key) {
            return None;
        }
        values.get(1).cloned()
    })
}

fn tag<const N: usize>(parts: [&str; N]) -> Result<Tag> {
    Tag::parse(parts.map(str::to_owned)).map_err(|error| Error::Parse(error.to_string()))
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

    fn encode_context() -> GroupPayloadEncodeContext {
        GroupPayloadEncodeContext {
            local_device_pubkey: device(9),
            created_at: UnixSeconds(12),
        }
    }

    fn snapshot() -> GroupSnapshot {
        GroupSnapshot {
            group_id: "group-1".to_string(),
            protocol: GroupProtocol::sender_key_v1(),
            name: "Team".to_string(),
            created_by: owner(1),
            members: vec![owner(1), owner(2)],
            admins: vec![owner(1)],
            revision: 3,
            created_at: UnixSeconds(10),
            updated_at: UnixSeconds(11),
        }
    }

    #[test]
    fn metadata_snapshot_command_encodes_master_kind_40_rumor() {
        let codec = JsonGroupPayloadCodecV1;
        let command = GroupPairwiseCommand::MetadataSnapshot {
            snapshot: snapshot(),
        };

        let encoded = codec
            .encode_pairwise_command(encode_context(), &command)
            .unwrap();

        assert!(codec.is_pairwise_payload(&encoded));
        let event = serde_json::from_slice::<UnsignedEvent>(&encoded).unwrap();
        assert_eq!(event.kind.as_u16() as u32, GROUP_METADATA_KIND);
        assert_eq!(event.pubkey, device(9).to_nostr().unwrap());
        assert_eq!(
            first_tag_value(&event, GROUP_LABEL_TAG).as_deref(),
            Some("group-1")
        );

        let content = serde_json::from_str::<serde_json::Value>(&event.content).unwrap();
        assert_eq!(content["id"], "group-1");
        assert_eq!(content["name"], "Team");
        assert_eq!(content["members"].as_array().unwrap().len(), 2);
        assert_eq!(content["admins"].as_array().unwrap().len(), 1);
        assert_eq!(content["revision"], 3);

        assert_eq!(
            codec.decode_pairwise_command(&encoded).unwrap(),
            Some(command)
        );
    }

    #[test]
    fn transitional_create_and_sync_envelopes_decode_as_metadata_snapshots() {
        let codec = JsonGroupPayloadCodecV1;
        let create = serde_json::to_vec(&serde_json::json!({
            "wire_format_version": 1,
            "payload": {
                "kind": "create_group",
                "group_id": "group-1",
                "protocol": "sender_key_v1",
                "base_revision": 0,
                "new_revision": 1,
                "name": "Team",
                "created_by": owner(1),
                "members": [owner(1), owner(2)],
                "admins": [owner(1)],
                "created_at": 10,
                "updated_at": 10
            }
        }))
        .unwrap();

        let decoded = codec.decode_pairwise_command(&create).unwrap();
        assert!(matches!(
            decoded,
            Some(GroupPairwiseCommand::MetadataSnapshot { snapshot })
                if snapshot.revision == 1 && snapshot.name == "Team"
        ));

        let sync = encode_envelope(GroupPairwisePayloadV1::SyncGroup {
            group_id: "group-1".to_string(),
            protocol: GroupProtocol::sender_key_v1(),
            revision: 2,
            name: "Renamed".to_string(),
            created_by: owner(1),
            members: vec![owner(1), owner(2)],
            admins: vec![owner(1)],
            created_at: UnixSeconds(10),
            updated_at: UnixSeconds(11),
        })
        .unwrap();

        let decoded = codec.decode_pairwise_command(&sync).unwrap();
        assert!(matches!(
            decoded,
            Some(GroupPairwiseCommand::MetadataSnapshot { snapshot })
                if snapshot.revision == 2 && snapshot.name == "Renamed"
        ));
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

        let encoded = codec
            .encode_pairwise_command(encode_context(), &command)
            .unwrap();

        assert_eq!(
            codec.decode_pairwise_command(&encoded).unwrap(),
            Some(command)
        );
    }
}
