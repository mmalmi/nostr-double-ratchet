use std::cmp::Ordering;

use anyhow::Result;
use nostr::UnsignedEvent;
use serde::{Deserialize, Serialize};

use crate::storage::{Storage, StoredChat, StoredGroup};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlStamp {
    pub ms: u64,
    pub event_id: String,
}

impl Ord for ControlStamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ms
            .cmp(&other.ms)
            .then_with(|| self.event_id.cmp(&other.event_id))
    }
}

impl PartialOrd for ControlStamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GroupMetadataApplyOutcome {
    Ignored,
    Rejected,
    Created(nostr_double_ratchet::group::GroupData),
    Updated {
        previous_secret: Option<String>,
        group: nostr_double_ratchet::group::GroupData,
    },
    Removed,
}

fn parse_ms_tag(tags: &nostr::Tags) -> Option<u64> {
    for tag in tags.iter() {
        let parts = tag.clone().to_vec();
        if parts.first().map(|s| s.as_str()) == Some("ms") {
            return parts.get(1).and_then(|s| s.parse::<u64>().ok());
        }
    }
    None
}

pub fn extract_control_stamp_from_unsigned(event: &UnsignedEvent) -> Option<ControlStamp> {
    let mut event = event.clone();
    event.ensure_id();
    let event_id = event.id?.to_hex();
    let ms = parse_ms_tag(&event.tags).unwrap_or_else(|| event.created_at.as_u64() * 1000);
    Some(ControlStamp { ms, event_id })
}

pub fn extract_control_stamp_from_value(
    rumor: &serde_json::Value,
    fallback_event_id: Option<&str>,
    fallback_created_at_s: u64,
) -> Option<ControlStamp> {
    let event_id = rumor
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| fallback_event_id.map(str::to_string))?;

    let ms = rumor
        .get("tags")
        .and_then(|v| v.as_array())
        .and_then(|tags| {
            tags.iter().find_map(|tag| {
                let parts = tag.as_array()?;
                if parts.first()?.as_str()? != "ms" {
                    return None;
                }
                parts.get(1)?.as_str()?.parse::<u64>().ok()
            })
        })
        .unwrap_or_else(|| {
            rumor
                .get("created_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(fallback_created_at_s)
                * 1000
        });

    Some(ControlStamp { ms, event_id })
}

pub fn select_canonical_session(
    owner_pubkey_hex: &str,
    sessions: &[(String, nostr_double_ratchet::SessionState)],
) -> Option<(String, nostr_double_ratchet::SessionState)> {
    let mut sorted: Vec<(String, nostr_double_ratchet::SessionState)> = sessions.to_vec();
    sorted.sort_by(|a, b| {
        let a_is_owner_device = a.0 == owner_pubkey_hex;
        let b_is_owner_device = b.0 == owner_pubkey_hex;
        b_is_owner_device
            .cmp(&a_is_owner_device)
            .then_with(|| a.0.cmp(&b.0))
    });
    sorted.into_iter().next()
}

pub fn should_apply_chat_settings(
    storage: &Storage,
    chat_id: &str,
    stamp: &ControlStamp,
) -> Result<bool> {
    Ok(storage
        .get_chat_settings_stamp(chat_id)?
        .is_none_or(|current| stamp > &current))
}

pub fn apply_chat_settings(
    storage: &Storage,
    chat: &mut StoredChat,
    ttl: Option<u64>,
    stamp: &ControlStamp,
) -> Result<bool> {
    if !should_apply_chat_settings(storage, &chat.id, stamp)? {
        return Ok(false);
    }

    chat.message_ttl_seconds = ttl;
    storage.save_chat_settings_stamp(&chat.id, stamp)?;
    Ok(true)
}

pub fn apply_group_metadata(
    storage: &Storage,
    group_id: &str,
    sender_owner_hex: &str,
    metadata: nostr_double_ratchet::group::GroupMetadata,
    stamp: ControlStamp,
    fallback_created_at_ms: u64,
    my_owner_pubkey_hex: &str,
) -> Result<GroupMetadataApplyOutcome> {
    if metadata.id != group_id {
        return Ok(GroupMetadataApplyOutcome::Rejected);
    }

    let existing = storage.get_group(group_id)?;
    match existing {
        Some(existing_group) => {
            let validation = nostr_double_ratchet::group::validate_metadata_update(
                &existing_group.data,
                &metadata,
                sender_owner_hex,
                my_owner_pubkey_hex,
            );

            match validation {
                nostr_double_ratchet::group::MetadataValidation::Reject => {
                    Ok(GroupMetadataApplyOutcome::Rejected)
                }
                nostr_double_ratchet::group::MetadataValidation::Accept => {
                    if storage
                        .get_group_control_stamp(group_id)?
                        .is_some_and(|current| stamp <= current)
                    {
                        return Ok(GroupMetadataApplyOutcome::Ignored);
                    }

                    let previous_secret = existing_group.data.secret.clone();
                    let updated = nostr_double_ratchet::group::apply_metadata_update(
                        &existing_group.data,
                        &metadata,
                    );
                    storage.save_group(&StoredGroup {
                        data: updated.clone(),
                    })?;
                    storage.save_group_control_stamp(group_id, &stamp)?;
                    let _ = storage.delete_group_tombstone(group_id)?;

                    Ok(GroupMetadataApplyOutcome::Updated {
                        previous_secret,
                        group: updated,
                    })
                }
                nostr_double_ratchet::group::MetadataValidation::Removed => {
                    if storage
                        .get_group_control_stamp(group_id)?
                        .is_some_and(|current| stamp <= current)
                    {
                        return Ok(GroupMetadataApplyOutcome::Ignored);
                    }

                    storage.delete_group(group_id)?;
                    storage.save_group_tombstone(group_id, &stamp)?;
                    Ok(GroupMetadataApplyOutcome::Removed)
                }
            }
        }
        None => {
            if storage
                .get_group_tombstone(group_id)?
                .is_some_and(|current| stamp <= current)
            {
                return Ok(GroupMetadataApplyOutcome::Ignored);
            }

            if !nostr_double_ratchet::group::validate_metadata_creation(
                &metadata,
                sender_owner_hex,
                my_owner_pubkey_hex,
            ) {
                return Ok(GroupMetadataApplyOutcome::Rejected);
            }

            let group = nostr_double_ratchet::group::GroupData {
                id: metadata.id.clone(),
                name: metadata.name.clone(),
                description: metadata.description,
                picture: metadata.picture,
                members: metadata.members.clone(),
                admins: metadata.admins.clone(),
                created_at: fallback_created_at_ms,
                secret: metadata.secret,
                accepted: None,
            };
            storage.save_group(&StoredGroup {
                data: group.clone(),
            })?;
            storage.save_group_control_stamp(group_id, &stamp)?;
            let _ = storage.delete_group_tombstone(group_id)?;
            Ok(GroupMetadataApplyOutcome::Created(group))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::storage::Storage;
    use tempfile::TempDir;

    fn rumor_with_ms(ms: u64, event_id: &str) -> UnsignedEvent {
        let mut rumor = nostr::EventBuilder::new(nostr::Kind::Custom(40), "metadata")
            .tags([nostr::Tag::parse(&["ms".to_string(), ms.to_string()]).unwrap()])
            .custom_created_at(nostr::Timestamp::from(ms / 1000))
            .build(nostr::Keys::generate().public_key());
        rumor.id = Some(nostr::EventId::parse(event_id).unwrap());
        rumor
    }

    fn test_group_data(my_owner: &str, peer_owner: &str) -> nostr_double_ratchet::group::GroupData {
        nostr_double_ratchet::group::GroupData {
            id: "group-1".to_string(),
            name: "initial".to_string(),
            description: None,
            picture: None,
            members: vec![my_owner.to_string(), peer_owner.to_string()],
            admins: vec![peer_owner.to_string()],
            created_at: 1_700_000_000_000,
            secret: Some("secret-1".to_string()),
            accepted: Some(true),
        }
    }

    fn dummy_session_state() -> nostr_double_ratchet::SessionState {
        let keys = nostr::Keys::generate();
        nostr_double_ratchet::SessionState {
            root_key: [0; 32],
            their_current_nostr_public_key: None,
            their_next_nostr_public_key: None,
            our_current_nostr_key: None,
            our_next_nostr_key: nostr_double_ratchet::SerializableKeyPair {
                public_key: keys.public_key(),
                private_key: keys.secret_key().to_secret_bytes(),
            },
            receiving_chain_key: None,
            sending_chain_key: None,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        }
    }

    #[test]
    fn extract_control_stamp_uses_ms_tag_and_event_id() {
        let event_id = "00000000000000000000000000000000000000000000000000000000000000aa";
        let rumor = rumor_with_ms(1_700_000_001_234, event_id);
        let stamp = extract_control_stamp_from_unsigned(&rumor).expect("stamp");
        assert_eq!(stamp.ms, 1_700_000_001_234);
        assert_eq!(stamp.event_id, event_id);
    }

    #[test]
    fn select_canonical_session_is_independent_of_input_order() {
        let session = dummy_session_state();
        let a = ("bbbb".to_string(), session.clone());
        let b = ("aaaa".to_string(), session);

        let first = select_canonical_session("zzzz", &[a.clone(), b.clone()]).expect("session");
        let second = select_canonical_session("zzzz", &[b, a]).expect("session");

        assert_eq!(first.0, "aaaa");
        assert_eq!(second.0, "aaaa");
    }

    #[test]
    fn select_canonical_session_prefers_owner_device() {
        let session = dummy_session_state();
        let owner = ("owner".to_string(), session.clone());
        let linked = ("linked-device".to_string(), session);

        let selected =
            select_canonical_session("owner", &[linked, owner.clone()]).expect("session");

        assert_eq!(selected.0, owner.0);
    }

    #[test]
    fn group_metadata_ignores_stale_update_after_newer_one_applies() {
        let temp = TempDir::new().unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        let me = nostr::Keys::generate().public_key().to_hex();
        let peer = nostr::Keys::generate().public_key().to_hex();
        let group = test_group_data(&me, &peer);
        storage
            .save_group(&StoredGroup {
                data: group.clone(),
            })
            .unwrap();
        storage
            .save_group_control_stamp(
                &group.id,
                &ControlStamp {
                    ms: 10,
                    event_id: "10".repeat(32),
                },
            )
            .unwrap();

        let newer = nostr_double_ratchet::group::GroupMetadata {
            id: group.id.clone(),
            name: "newer".to_string(),
            description: None,
            picture: None,
            members: group.members.clone(),
            admins: group.admins.clone(),
            secret: group.secret.clone(),
        };
        let older = nostr_double_ratchet::group::GroupMetadata {
            name: "older".to_string(),
            ..newer.clone()
        };

        let newer_outcome = apply_group_metadata(
            &storage,
            &group.id,
            &peer,
            newer,
            ControlStamp {
                ms: 20,
                event_id: "20".repeat(32),
            },
            group.created_at,
            &me,
        )
        .unwrap();
        assert!(matches!(
            newer_outcome,
            GroupMetadataApplyOutcome::Updated { .. }
        ));

        let older_outcome = apply_group_metadata(
            &storage,
            &group.id,
            &peer,
            older,
            ControlStamp {
                ms: 15,
                event_id: "15".repeat(32),
            },
            group.created_at,
            &me,
        )
        .unwrap();
        assert_eq!(older_outcome, GroupMetadataApplyOutcome::Ignored);

        let stored = storage.get_group(&group.id).unwrap().unwrap();
        assert_eq!(stored.data.name, "newer");
    }

    #[test]
    fn group_tombstone_blocks_stale_recreation() {
        let temp = TempDir::new().unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        let me = nostr::Keys::generate().public_key().to_hex();
        let peer = nostr::Keys::generate().public_key().to_hex();
        let group = test_group_data(&me, &peer);
        storage
            .save_group(&StoredGroup {
                data: group.clone(),
            })
            .unwrap();
        storage
            .save_group_control_stamp(
                &group.id,
                &ControlStamp {
                    ms: 10,
                    event_id: "10".repeat(32),
                },
            )
            .unwrap();

        let removed = nostr_double_ratchet::group::GroupMetadata {
            id: group.id.clone(),
            name: group.name.clone(),
            description: None,
            picture: None,
            members: vec![peer.clone()],
            admins: vec![peer.clone()],
            secret: None,
        };

        let outcome = apply_group_metadata(
            &storage,
            &group.id,
            &peer,
            removed,
            ControlStamp {
                ms: 30,
                event_id: "30".repeat(32),
            },
            group.created_at,
            &me,
        )
        .unwrap();
        assert_eq!(outcome, GroupMetadataApplyOutcome::Removed);
        assert!(storage.get_group(&group.id).unwrap().is_none());

        let stale_create = nostr_double_ratchet::group::GroupMetadata {
            id: group.id.clone(),
            name: "stale".to_string(),
            description: None,
            picture: None,
            members: vec![me.clone(), peer.clone()],
            admins: vec![peer.clone()],
            secret: Some("secret-0".to_string()),
        };

        let stale = apply_group_metadata(
            &storage,
            &group.id,
            &peer,
            stale_create,
            ControlStamp {
                ms: 20,
                event_id: "20".repeat(32),
            },
            group.created_at,
            &me,
        )
        .unwrap();
        assert_eq!(stale, GroupMetadataApplyOutcome::Ignored);
        assert!(storage.get_group(&group.id).unwrap().is_none());
    }

    #[test]
    fn chat_settings_ignore_stale_updates() {
        let temp = TempDir::new().unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        let mut chat = StoredChat {
            id: "chat-1".to_string(),
            their_pubkey: nostr::Keys::generate().public_key().to_hex(),
            device_id: None,
            created_at: 1,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        };

        let newer = ControlStamp {
            ms: 50,
            event_id: "50".repeat(32),
        };
        assert!(apply_chat_settings(&storage, &mut chat, Some(60), &newer).unwrap());
        assert_eq!(chat.message_ttl_seconds, Some(60));

        let older = ControlStamp {
            ms: 40,
            event_id: "40".repeat(32),
        };
        assert!(!apply_chat_settings(&storage, &mut chat, Some(10), &older).unwrap());
        assert_eq!(chat.message_ttl_seconds, Some(60));
    }
}
