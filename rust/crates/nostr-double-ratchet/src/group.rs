use rand::Rng;
use serde::{Deserialize, Serialize};

pub const GROUP_METADATA_KIND: u32 = 40;
pub const GROUP_INVITE_RUMOR_KIND: u32 = 10445;
/// SharedChannel rumor kind for distributing a Signal-style sender key to the group.
pub const GROUP_SENDER_KEY_DISTRIBUTION_KIND: u32 = 10446;
/// SharedChannel rumor kind for group messages encrypted with a per-sender sender key.
pub const GROUP_SENDER_KEY_MESSAGE_KIND: u32 = 10447;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupData {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    pub members: Vec<String>,
    pub admins: Vec<String>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupMetadata {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    pub members: Vec<String>,
    pub admins: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValidation {
    Accept,
    Reject,
    Removed,
}

pub struct GroupUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub picture: Option<String>,
}

pub fn is_group_admin(group: &GroupData, pubkey: &str) -> bool {
    group.admins.iter().any(|a| a == pubkey)
}

pub fn generate_group_secret() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    hex::encode(bytes)
}

pub fn create_group_data(name: &str, creator_pubkey: &str, member_pubkeys: &[&str]) -> GroupData {
    let mut all_members = vec![creator_pubkey.to_string()];
    for pk in member_pubkeys {
        if *pk != creator_pubkey {
            all_members.push(pk.to_string());
        }
    }

    GroupData {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        description: None,
        picture: None,
        members: all_members,
        admins: vec![creator_pubkey.to_string()],
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        secret: Some(generate_group_secret()),
        accepted: Some(true),
    }
}

pub fn build_group_metadata_content(group: &GroupData, exclude_secret: bool) -> String {
    let metadata = GroupMetadata {
        id: group.id.clone(),
        name: group.name.clone(),
        members: group.members.clone(),
        admins: group.admins.clone(),
        description: group.description.clone(),
        picture: group.picture.clone(),
        secret: if exclude_secret {
            None
        } else {
            group.secret.clone()
        },
    };
    serde_json::to_string(&metadata).unwrap()
}

pub fn parse_group_metadata(content: &str) -> Option<GroupMetadata> {
    let val: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = val.as_object()?;

    let id = obj.get("id")?.as_str()?;
    let name = obj.get("name")?.as_str()?;

    let members_val = obj.get("members")?;
    let members_arr = members_val.as_array()?;
    let members: Vec<String> = members_arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    if members.len() != members_arr.len() {
        return None;
    }

    let admins_val = obj.get("admins")?;
    let admins_arr = admins_val.as_array()?;
    if admins_arr.is_empty() {
        return None;
    }
    let admins: Vec<String> = admins_arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    if admins.len() != admins_arr.len() {
        return None;
    }

    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let picture = obj
        .get("picture")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let secret = obj
        .get("secret")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(GroupMetadata {
        id: id.to_string(),
        name: name.to_string(),
        description,
        picture,
        members,
        admins,
        secret,
    })
}

pub fn validate_metadata_update(
    existing: &GroupData,
    metadata: &GroupMetadata,
    sender: &str,
    my_pubkey: &str,
) -> MetadataValidation {
    if !is_group_admin(existing, sender) {
        return MetadataValidation::Reject;
    }
    if !metadata.members.iter().any(|m| m == my_pubkey) {
        return MetadataValidation::Removed;
    }
    MetadataValidation::Accept
}

pub fn validate_metadata_creation(metadata: &GroupMetadata, sender: &str, my_pubkey: &str) -> bool {
    if !metadata.admins.iter().any(|a| a == sender) {
        return false;
    }
    if !metadata.members.iter().any(|m| m == my_pubkey) {
        return false;
    }
    true
}

pub fn apply_metadata_update(existing: &GroupData, metadata: &GroupMetadata) -> GroupData {
    GroupData {
        id: existing.id.clone(),
        name: metadata.name.clone(),
        members: metadata.members.clone(),
        admins: metadata.admins.clone(),
        description: metadata.description.clone(),
        picture: metadata.picture.clone(),
        secret: metadata.secret.clone().or_else(|| existing.secret.clone()),
        created_at: existing.created_at,
        accepted: existing.accepted,
    }
}

pub fn add_group_member(group: &GroupData, pubkey: &str, actor: &str) -> Option<GroupData> {
    if !is_group_admin(group, actor) {
        return None;
    }
    if group.members.iter().any(|m| m == pubkey) {
        return None;
    }
    let mut new_members = group.members.clone();
    new_members.push(pubkey.to_string());
    Some(GroupData {
        members: new_members,
        secret: Some(generate_group_secret()),
        ..group.clone()
    })
}

pub fn remove_group_member(group: &GroupData, pubkey: &str, actor: &str) -> Option<GroupData> {
    if !is_group_admin(group, actor) {
        return None;
    }
    if !group.members.iter().any(|m| m == pubkey) {
        return None;
    }
    if pubkey == actor {
        return None;
    }
    Some(GroupData {
        members: group
            .members
            .iter()
            .filter(|m| *m != pubkey)
            .cloned()
            .collect(),
        admins: group
            .admins
            .iter()
            .filter(|a| *a != pubkey)
            .cloned()
            .collect(),
        secret: Some(generate_group_secret()),
        ..group.clone()
    })
}

pub fn update_group_data(
    group: &GroupData,
    updates: &GroupUpdate,
    actor: &str,
) -> Option<GroupData> {
    if !is_group_admin(group, actor) {
        return None;
    }
    let mut updated = group.clone();
    if let Some(ref name) = updates.name {
        updated.name = name.clone();
    }
    if let Some(ref description) = updates.description {
        updated.description = Some(description.clone());
    }
    if let Some(ref picture) = updates.picture {
        updated.picture = Some(picture.clone());
    }
    Some(updated)
}

pub fn add_group_admin(group: &GroupData, pubkey: &str, actor: &str) -> Option<GroupData> {
    if !is_group_admin(group, actor) {
        return None;
    }
    if !group.members.iter().any(|m| m == pubkey) {
        return None;
    }
    if group.admins.iter().any(|a| a == pubkey) {
        return None;
    }
    let mut new_admins = group.admins.clone();
    new_admins.push(pubkey.to_string());
    Some(GroupData {
        admins: new_admins,
        ..group.clone()
    })
}

pub fn remove_group_admin(group: &GroupData, pubkey: &str, actor: &str) -> Option<GroupData> {
    if !is_group_admin(group, actor) {
        return None;
    }
    if !group.admins.iter().any(|a| a == pubkey) {
        return None;
    }
    if group.admins.len() <= 1 {
        return None;
    }
    Some(GroupData {
        admins: group
            .admins
            .iter()
            .filter(|a| *a != pubkey)
            .cloned()
            .collect(),
        ..group.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CAROL: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const DAVE: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn make_group(overrides: Option<GroupData>) -> GroupData {
        let base = GroupData {
            id: "test-group".to_string(),
            name: "Test".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![ALICE.to_string()],
            created_at: 1700000000000,
            secret: Some("a".repeat(64)),
            accepted: Some(true),
        };
        match overrides {
            Some(o) => o,
            None => base,
        }
    }

    fn make_group_with(f: impl FnOnce(&mut GroupData)) -> GroupData {
        let mut g = make_group(None);
        f(&mut g);
        g
    }

    // === Group constants ===

    #[test]
    fn group_metadata_kind_is_40() {
        assert_eq!(GROUP_METADATA_KIND, 40);
    }

    #[test]
    fn group_invite_rumor_kind_is_10445() {
        assert_eq!(GROUP_INVITE_RUMOR_KIND, 10445);
    }

    #[test]
    fn group_sender_key_distribution_kind_is_10446() {
        assert_eq!(GROUP_SENDER_KEY_DISTRIBUTION_KIND, 10446);
    }

    #[test]
    fn group_sender_key_message_kind_is_10447() {
        assert_eq!(GROUP_SENDER_KEY_MESSAGE_KIND, 10447);
    }

    // === isGroupAdmin ===

    #[test]
    fn is_group_admin_returns_true_for_admin() {
        assert!(is_group_admin(&make_group(None), ALICE));
    }

    #[test]
    fn is_group_admin_returns_false_for_non_admin_member() {
        assert!(!is_group_admin(&make_group(None), BOB));
    }

    #[test]
    fn is_group_admin_returns_false_for_non_member() {
        assert!(!is_group_admin(&make_group(None), DAVE));
    }

    // === generateGroupSecret ===

    #[test]
    fn generate_group_secret_returns_64_char_hex() {
        let secret = generate_group_secret();
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_group_secret_unique() {
        let a = generate_group_secret();
        let b = generate_group_secret();
        assert_ne!(a, b);
    }

    // === createGroupData ===

    #[test]
    fn create_group_data_creator_is_first_member_and_sole_admin() {
        let group = create_group_data("My Group", ALICE, &[BOB, CAROL]);
        assert_eq!(group.name, "My Group");
        assert_eq!(group.members, vec![ALICE, BOB, CAROL]);
        assert_eq!(group.admins, vec![ALICE]);
        assert_eq!(group.accepted, Some(true));
        assert!(group.secret.as_ref().unwrap().len() == 64);
        assert!(!group.id.is_empty());
    }

    #[test]
    fn create_group_data_deduplicates_creator() {
        let group = create_group_data("Dedup", ALICE, &[ALICE, BOB]);
        let alice_count = group.members.iter().filter(|m| *m == ALICE).count();
        assert_eq!(alice_count, 1);
    }

    // === buildGroupMetadataContent ===

    #[test]
    fn build_group_metadata_content_serializes_to_json() {
        let group = make_group_with(|g| {
            g.description = Some("desc".to_string());
            g.picture = Some("pic.jpg".to_string());
        });
        let json = build_group_metadata_content(&group, false);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["id"], group.id);
        assert_eq!(parsed["name"], group.name);
        assert_eq!(parsed["members"], serde_json::json!([ALICE, BOB]));
        assert_eq!(parsed["admins"], serde_json::json!([ALICE]));
        assert_eq!(parsed["description"], "desc");
        assert_eq!(parsed["picture"], "pic.jpg");
        assert_eq!(parsed["secret"], "a".repeat(64));
    }

    #[test]
    fn build_group_metadata_content_excludes_secret() {
        let group = make_group(None);
        let json = build_group_metadata_content(&group, true);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("secret").is_none());
    }

    #[test]
    fn build_group_metadata_content_omits_empty_optional_fields() {
        let group = make_group(None);
        let json = build_group_metadata_content(&group, false);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("description").is_none());
        assert!(parsed.get("picture").is_none());
    }

    // === parseGroupMetadata ===

    #[test]
    fn parse_group_metadata_parses_valid() {
        let meta = GroupMetadata {
            id: "g1".to_string(),
            name: "G".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string()],
            admins: vec![ALICE.to_string()],
            secret: Some("x".repeat(64)),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let result = parse_group_metadata(&json).unwrap();
        assert_eq!(result, meta);
    }

    #[test]
    fn parse_group_metadata_returns_none_for_missing_id() {
        let json = serde_json::json!({
            "name": "G", "members": [ALICE], "admins": [ALICE]
        });
        assert!(parse_group_metadata(&json.to_string()).is_none());
    }

    #[test]
    fn parse_group_metadata_returns_none_for_empty_admins() {
        let json = serde_json::json!({
            "id": "g1", "name": "G", "members": [ALICE], "admins": []
        });
        assert!(parse_group_metadata(&json.to_string()).is_none());
    }

    #[test]
    fn parse_group_metadata_returns_none_for_invalid_json() {
        assert!(parse_group_metadata("not json").is_none());
    }

    #[test]
    fn parse_group_metadata_returns_none_for_non_array_members() {
        let json = serde_json::json!({
            "id": "g1", "name": "G", "members": "bad", "admins": [ALICE]
        });
        assert!(parse_group_metadata(&json.to_string()).is_none());
    }

    // === validateMetadataUpdate ===

    #[test]
    fn validate_metadata_update_accepts_from_admin() {
        let group = make_group(None);
        let metadata = GroupMetadata {
            id: group.id.clone(),
            name: "New".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        assert_eq!(
            validate_metadata_update(&group, &metadata, ALICE, BOB),
            MetadataValidation::Accept
        );
    }

    #[test]
    fn validate_metadata_update_rejects_from_non_admin() {
        let group = make_group(None);
        let metadata = GroupMetadata {
            id: group.id.clone(),
            name: "Hack".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![BOB.to_string()],
            secret: None,
        };
        assert_eq!(
            validate_metadata_update(&group, &metadata, BOB, ALICE),
            MetadataValidation::Reject
        );
    }

    #[test]
    fn validate_metadata_update_returns_removed_when_not_in_members() {
        let group = make_group(None);
        let metadata = GroupMetadata {
            id: group.id.clone(),
            name: "Kicked".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        assert_eq!(
            validate_metadata_update(&group, &metadata, ALICE, BOB),
            MetadataValidation::Removed
        );
    }

    // === validateMetadataCreation ===

    #[test]
    fn validate_metadata_creation_accepts_valid() {
        let meta = GroupMetadata {
            id: "g1".to_string(),
            name: "G".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        assert!(validate_metadata_creation(&meta, ALICE, BOB));
    }

    #[test]
    fn validate_metadata_creation_rejects_sender_not_in_admins() {
        let meta = GroupMetadata {
            id: "g1".to_string(),
            name: "G".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        assert!(!validate_metadata_creation(&meta, BOB, BOB));
    }

    #[test]
    fn validate_metadata_creation_rejects_my_pubkey_not_in_members() {
        let meta = GroupMetadata {
            id: "g1".to_string(),
            name: "G".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        assert!(!validate_metadata_creation(&meta, ALICE, BOB));
    }

    // === applyMetadataUpdate ===

    #[test]
    fn apply_metadata_update_updates_fields_preserving_accepted() {
        let group = make_group_with(|g| g.accepted = Some(true));
        let meta = GroupMetadata {
            id: group.id.clone(),
            name: "Updated".to_string(),
            description: Some("new desc".to_string()),
            picture: None,
            members: vec![ALICE.to_string(), BOB.to_string(), CAROL.to_string()],
            admins: vec![ALICE.to_string()],
            secret: Some("b".repeat(64)),
        };
        let updated = apply_metadata_update(&group, &meta);
        assert_eq!(updated.name, "Updated");
        assert_eq!(updated.members, vec![ALICE, BOB, CAROL]);
        assert_eq!(updated.description, Some("new desc".to_string()));
        assert_eq!(updated.secret, Some("b".repeat(64)));
        assert_eq!(updated.accepted, Some(true));
    }

    #[test]
    fn apply_metadata_update_keeps_existing_secret_when_metadata_has_none() {
        let original_secret = format!("original{}", "0".repeat(56));
        let group = make_group_with(|g| g.secret = Some(original_secret.clone()));
        let meta = GroupMetadata {
            id: group.id.clone(),
            name: "X".to_string(),
            description: None,
            picture: None,
            members: vec![ALICE.to_string()],
            admins: vec![ALICE.to_string()],
            secret: None,
        };
        let updated = apply_metadata_update(&group, &meta);
        assert_eq!(updated.secret, Some(original_secret));
    }

    // === addGroupMember ===

    #[test]
    fn add_group_member_admin_can_add_and_secret_rotates() {
        let group = make_group(None);
        let result = add_group_member(&group, CAROL, ALICE).unwrap();
        assert!(result.members.contains(&CAROL.to_string()));
        assert_ne!(result.secret, group.secret);
    }

    #[test]
    fn add_group_member_returns_none_if_not_admin() {
        assert!(add_group_member(&make_group(None), CAROL, BOB).is_none());
    }

    #[test]
    fn add_group_member_returns_none_if_already_member() {
        assert!(add_group_member(&make_group(None), BOB, ALICE).is_none());
    }

    // === removeGroupMember ===

    #[test]
    fn remove_group_member_admin_can_remove_and_secret_rotates() {
        let group = make_group_with(|g| {
            g.members = vec![ALICE.to_string(), BOB.to_string(), CAROL.to_string()];
        });
        let result = remove_group_member(&group, CAROL, ALICE).unwrap();
        assert!(!result.members.contains(&CAROL.to_string()));
        assert_ne!(result.secret, group.secret);
    }

    #[test]
    fn remove_group_member_also_strips_admin_status() {
        let group = make_group_with(|g| {
            g.admins = vec![ALICE.to_string(), BOB.to_string()];
        });
        let result = remove_group_member(&group, BOB, ALICE).unwrap();
        assert!(!result.admins.contains(&BOB.to_string()));
    }

    #[test]
    fn remove_group_member_returns_none_if_not_admin() {
        let group = make_group_with(|g| {
            g.members = vec![ALICE.to_string(), BOB.to_string(), CAROL.to_string()];
        });
        assert!(remove_group_member(&group, CAROL, BOB).is_none());
    }

    #[test]
    fn remove_group_member_returns_none_if_not_in_group() {
        assert!(remove_group_member(&make_group(None), DAVE, ALICE).is_none());
    }

    #[test]
    fn remove_group_member_returns_none_if_self_remove() {
        assert!(remove_group_member(&make_group(None), ALICE, ALICE).is_none());
    }

    // === updateGroupData ===

    #[test]
    fn update_group_data_admin_can_update_name() {
        let result = update_group_data(
            &make_group(None),
            &GroupUpdate {
                name: Some("New Name".to_string()),
                description: None,
                picture: None,
            },
            ALICE,
        )
        .unwrap();
        assert_eq!(result.name, "New Name");
    }

    #[test]
    fn update_group_data_admin_can_update_description() {
        let result = update_group_data(
            &make_group(None),
            &GroupUpdate {
                name: None,
                description: Some("new desc".to_string()),
                picture: None,
            },
            ALICE,
        )
        .unwrap();
        assert_eq!(result.description, Some("new desc".to_string()));
    }

    #[test]
    fn update_group_data_admin_can_update_picture() {
        let result = update_group_data(
            &make_group(None),
            &GroupUpdate {
                name: None,
                description: None,
                picture: Some("pic.jpg".to_string()),
            },
            ALICE,
        )
        .unwrap();
        assert_eq!(result.picture, Some("pic.jpg".to_string()));
    }

    #[test]
    fn update_group_data_returns_none_if_not_admin() {
        assert!(update_group_data(
            &make_group(None),
            &GroupUpdate {
                name: Some("Hack".to_string()),
                description: None,
                picture: None,
            },
            BOB,
        )
        .is_none());
    }

    // === addGroupAdmin ===

    #[test]
    fn add_group_admin_can_promote_member() {
        let result = add_group_admin(&make_group(None), BOB, ALICE).unwrap();
        assert!(result.admins.contains(&BOB.to_string()));
    }

    #[test]
    fn add_group_admin_returns_none_if_not_admin() {
        assert!(add_group_admin(&make_group(None), CAROL, BOB).is_none());
    }

    #[test]
    fn add_group_admin_returns_none_if_not_member() {
        assert!(add_group_admin(&make_group(None), DAVE, ALICE).is_none());
    }

    #[test]
    fn add_group_admin_returns_none_if_already_admin() {
        assert!(add_group_admin(&make_group(None), ALICE, ALICE).is_none());
    }

    // === removeGroupAdmin ===

    #[test]
    fn remove_group_admin_can_demote() {
        let group = make_group_with(|g| {
            g.admins = vec![ALICE.to_string(), BOB.to_string()];
        });
        let result = remove_group_admin(&group, BOB, ALICE).unwrap();
        assert!(!result.admins.contains(&BOB.to_string()));
    }

    #[test]
    fn remove_group_admin_returns_none_if_not_admin() {
        let group = make_group_with(|g| {
            g.admins = vec![ALICE.to_string(), BOB.to_string()];
        });
        assert!(remove_group_admin(&group, ALICE, CAROL).is_none());
    }

    #[test]
    fn remove_group_admin_returns_none_if_target_not_admin() {
        assert!(remove_group_admin(&make_group(None), BOB, ALICE).is_none());
    }

    #[test]
    fn remove_group_admin_returns_none_if_would_remove_last() {
        assert!(remove_group_admin(&make_group(None), ALICE, ALICE).is_none());
    }

    // === JSON interop: camelCase field names ===

    #[test]
    fn group_data_serializes_with_camel_case() {
        let group = make_group(None);
        let json = serde_json::to_string(&group).unwrap();
        assert!(json.contains("\"createdAt\""));
        assert!(!json.contains("\"created_at\""));
    }

    #[test]
    fn group_data_deserializes_from_camel_case() {
        let json = r#"{"id":"g1","name":"Test","members":["a"],"admins":["a"],"createdAt":123}"#;
        let group: GroupData = serde_json::from_str(json).unwrap();
        assert_eq!(group.created_at, 123);
    }

    #[test]
    fn group_metadata_roundtrip_json() {
        let meta = GroupMetadata {
            id: "g1".to_string(),
            name: "Test".to_string(),
            description: Some("desc".to_string()),
            picture: Some("pic.jpg".to_string()),
            members: vec![ALICE.to_string(), BOB.to_string()],
            admins: vec![ALICE.to_string()],
            secret: Some("x".repeat(64)),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed = parse_group_metadata(&json).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn build_then_parse_roundtrip() {
        let group = make_group_with(|g| {
            g.description = Some("desc".to_string());
            g.picture = Some("pic.jpg".to_string());
        });
        let json = build_group_metadata_content(&group, false);
        let parsed = parse_group_metadata(&json).unwrap();
        assert_eq!(parsed.id, group.id);
        assert_eq!(parsed.name, group.name);
        assert_eq!(parsed.members, group.members);
        assert_eq!(parsed.admins, group.admins);
        assert_eq!(parsed.description, group.description);
        assert_eq!(parsed.picture, group.picture);
        assert_eq!(parsed.secret, group.secret);
    }
}
