use anyhow::Result;
use nostr_double_ratchet::{Session, CHAT_MESSAGE_KIND, REACTION_KIND, GROUP_METADATA_KIND};
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredGroup, StoredGroupMessage};

#[derive(Serialize)]
struct GroupList {
    groups: Vec<GroupInfo>,
}

#[derive(Serialize)]
struct GroupInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    picture: Option<String>,
    members: Vec<String>,
    admins: Vec<String>,
    created_at: u64,
}

impl From<&nostr_double_ratchet::group::GroupData> for GroupInfo {
    fn from(g: &nostr_double_ratchet::group::GroupData) -> Self {
        GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            picture: g.picture.clone(),
            members: g.members.clone(),
            admins: g.admins.clone(),
            created_at: g.created_at,
        }
    }
}

#[derive(Serialize)]
struct GroupMessageInfo {
    id: String,
    group_id: String,
    sender_pubkey: String,
    content: String,
    timestamp: u64,
    is_outgoing: bool,
}

#[derive(Serialize)]
struct GroupMessageList {
    group_id: String,
    messages: Vec<GroupMessageInfo>,
}

/// Fan-out a rumor event to all group members via their 1:1 sessions.
/// Returns the number of members the event was sent to.
async fn fan_out(
    group: &nostr_double_ratchet::group::GroupData,
    rumor_kind: u32,
    content: &str,
    extra_tags: Vec<Vec<String>>,
    config: &Config,
    storage: &Storage,
) -> Result<usize> {
    let my_pubkey = config.public_key()?;

    // Build tags: extra_tags + ["l", group_id] + ["ms", now_ms]
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let mut tags: Vec<Vec<String>> = extra_tags;
    tags.push(vec!["l".to_string(), group.id.clone()]);
    tags.push(vec!["ms".to_string(), now_ms.to_string()]);

    let nostr_tags: Vec<nostr::Tag> = tags.iter()
        .filter_map(|t| nostr::Tag::parse(t).ok())
        .collect();

    // Build the unsigned rumor event
    let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
    let unsigned_event = nostr::EventBuilder::new(
        nostr::Kind::Custom(rumor_kind as u16),
        content,
    )
    .tags(nostr_tags)
    .build(my_pk);

    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }

    let mut sent_count = 0;
    let mut connected = false;

    for member in &group.members {
        if member == &my_pubkey {
            continue;
        }

        // Find 1:1 chat with this member
        let chat = match storage.get_chat_by_pubkey(member)? {
            Some(c) => c,
            None => continue, // No session with this member yet
        };

        let session_state: nostr_double_ratchet::SessionState = match serde_json::from_str(&chat.session_state) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut session = Session::new(session_state, chat.id.clone());

        // Encrypt the rumor via the 1:1 session
        let encrypted_event = match session.send_event(unsigned_event.clone()) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Save updated session state
        let mut updated_chat = chat;
        updated_chat.session_state = serde_json::to_string(&session.state)?;
        storage.save_chat(&updated_chat)?;

        // Publish
        if !connected {
            client.connect().await;
            connected = true;
        }
        client.send_event(encrypted_event).await?;
        sent_count += 1;
    }

    Ok(sent_count)
}

/// Fan-out group metadata to members after a mutation.
async fn fan_out_metadata(
    group: &nostr_double_ratchet::group::GroupData,
    excluded_member: Option<&str>,
    config: &Config,
    storage: &Storage,
) -> Result<()> {
    let my_pubkey = config.public_key()?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    let mut connected = false;

    for member in &group.members {
        if member == &my_pubkey {
            continue;
        }

        let chat = match storage.get_chat_by_pubkey(member)? {
            Some(c) => c,
            None => continue,
        };

        let session_state: nostr_double_ratchet::SessionState = match serde_json::from_str(&chat.session_state) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut session = Session::new(session_state, chat.id.clone());

        // Exclude secret for removed members
        let exclude_secret = excluded_member.map(|e| e == member).unwrap_or(false);
        let metadata_content = nostr_double_ratchet::group::build_group_metadata_content(group, exclude_secret);

        let mut tags: Vec<Vec<String>> = Vec::new();
        tags.push(vec!["l".to_string(), group.id.clone()]);
        tags.push(vec!["ms".to_string(), now_ms.to_string()]);

        let nostr_tags: Vec<nostr::Tag> = tags.iter()
            .filter_map(|t| nostr::Tag::parse(t).ok())
            .collect();

        let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
        let unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_METADATA_KIND as u16),
            &metadata_content,
        )
        .tags(nostr_tags)
        .build(my_pk);

        let encrypted = match session.send_event(unsigned) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mut updated_chat = chat;
        updated_chat.session_state = serde_json::to_string(&session.state)?;
        storage.save_chat(&updated_chat)?;

        if !connected {
            client.connect().await;
            connected = true;
        }
        client.send_event(encrypted).await?;
    }

    // Also fan-out without secret to the removed member
    if let Some(removed) = excluded_member {
        if let Some(chat) = storage.get_chat_by_pubkey(removed)? {
            if let Ok(state) = serde_json::from_str::<nostr_double_ratchet::SessionState>(&chat.session_state) {
                let mut session = Session::new(state, chat.id.clone());
                let metadata_content = nostr_double_ratchet::group::build_group_metadata_content(group, true);

                let mut tags: Vec<Vec<String>> = Vec::new();
                tags.push(vec!["l".to_string(), group.id.clone()]);
                tags.push(vec!["ms".to_string(), now_ms.to_string()]);

                let nostr_tags: Vec<nostr::Tag> = tags.iter()
                    .filter_map(|t| nostr::Tag::parse(t).ok())
                    .collect();

                let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
                let unsigned = nostr::EventBuilder::new(
                    nostr::Kind::Custom(GROUP_METADATA_KIND as u16),
                    &metadata_content,
                )
                .tags(nostr_tags)
                .build(my_pk);

                if let Ok(encrypted) = session.send_event(unsigned) {
                    let mut updated_chat = chat;
                    updated_chat.session_state = serde_json::to_string(&session.state)?;
                    storage.save_chat(&updated_chat)?;

                    if !connected {
                        client.connect().await;
                    }
                    let _ = client.send_event(encrypted).await;
                }
            }
        }
    }

    Ok(())
}

pub async fn create(
    name: &str,
    members: &[String],
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let my_pubkey = config.public_key()?;
    let member_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
    let group_data = nostr_double_ratchet::group::create_group_data(name, &my_pubkey, &member_refs);

    let stored = StoredGroup {
        data: group_data,
    };
    storage.save_group(&stored)?;

    // Fan-out metadata to members
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.create", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn list(storage: &Storage, output: &Output) -> Result<()> {
    let groups = storage.list_groups()?;
    let infos: Vec<GroupInfo> = groups.iter().map(|g| GroupInfo::from(&g.data)).collect();
    output.success("group.list", GroupList { groups: infos });
    Ok(())
}

pub async fn show(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;
    output.success("group.show", GroupInfo::from(&group.data));
    Ok(())
}

pub async fn delete(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    if storage.delete_group(id)? {
        output.success_message("group.delete", &format!("Deleted group {}", id));
    } else {
        anyhow::bail!("Group not found: {}", id);
    }
    Ok(())
}

pub async fn update(
    id: &str,
    name: Option<&str>,
    description: Option<&str>,
    picture: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updates = nostr_double_ratchet::group::GroupUpdate {
        name: name.map(|s| s.to_string()),
        description: description.map(|s| s.to_string()),
        picture: picture.map(|s| s.to_string()),
    };

    let updated = nostr_double_ratchet::group::update_group_data(&group.data, &updates, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Permission denied: not an admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out metadata
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.update", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn add_member(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::add_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add member: not admin or already a member"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out metadata to all members including new one
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.add-member", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn remove_member(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::remove_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot remove member: not admin, not a member, or trying to remove self"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out with secret to remaining, without secret to removed
    let _ = fan_out_metadata(&stored.data, Some(pubkey), config, storage).await;

    output.success("group.remove-member", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn add_admin(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::add_group_admin(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add admin: not admin, not a member, or already an admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out metadata
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.add-admin", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn remove_admin(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::remove_group_admin(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot remove admin: not admin, target not admin, or would remove last admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out metadata
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.remove-admin", GroupInfo::from(&stored.data));
    Ok(())
}

/// Send a message to all group members (fan-out)
pub async fn send_message(
    id: &str,
    message: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let msg_id = uuid::Uuid::new_v4().to_string();

    let sent_count = fan_out(&group.data, CHAT_MESSAGE_KIND, message, vec![], config, storage).await?;

    // Store outgoing message
    let stored_msg = StoredGroupMessage {
        id: msg_id.clone(),
        group_id: id.to_string(),
        sender_pubkey: my_pubkey,
        content: message.to_string(),
        timestamp,
        is_outgoing: true,
    };
    storage.save_group_message(&stored_msg)?;

    output.success("group.send", serde_json::json!({
        "id": msg_id,
        "group_id": id,
        "content": message,
        "timestamp": timestamp,
        "sent_to": sent_count,
    }));

    Ok(())
}

/// React to a group message (fan-out)
pub async fn react(
    id: &str,
    message_id: &str,
    emoji: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let extra_tags = vec![vec!["e".to_string(), message_id.to_string()]];

    let sent_count = fan_out(&group.data, REACTION_KIND, emoji, extra_tags, config, storage).await?;

    output.success("group.react", serde_json::json!({
        "group_id": id,
        "message_id": message_id,
        "emoji": emoji,
        "sent_to": sent_count,
    }));

    Ok(())
}

/// Accept a group invitation
pub async fn accept(
    id: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    if group.data.accepted == Some(true) {
        anyhow::bail!("Group already accepted");
    }

    let mut updated = group.data.clone();
    updated.accepted = Some(true);

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // If the group has a secret, publish our invite on the shared channel
    if let Some(ref secret_hex) = stored.data.secret {
        if let Ok(secret_bytes) = hex::decode(secret_hex) {
            if secret_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&secret_bytes);

                if let Ok(channel) = nostr_double_ratchet::SharedChannel::new(&arr) {
                    let my_pubkey = config.public_key()?;

                    // Create an invite for group members to establish 1:1 sessions
                    let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
                    let invite = nostr_double_ratchet::Invite::create_new(my_pk, None, None)?;
                    let invite_url = invite.get_url("https://iris.to")?;

                    let rumor_json = serde_json::json!({
                        "id": uuid::Uuid::new_v4().to_string(),
                        "pubkey": my_pubkey,
                        "created_at": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        "kind": nostr_double_ratchet::GROUP_INVITE_RUMOR_KIND,
                        "tags": [],
                        "content": serde_json::json!({
                            "inviteUrl": invite_url,
                            "groupId": id,
                        }).to_string()
                    }).to_string();

                    if let Ok(event) = channel.create_event(&rumor_json) {
                        let client = Client::default();
                        let relays = config.resolved_relays();
                        for relay in &relays {
                            client.add_relay(relay).await?;
                        }
                        client.connect().await;
                        let _ = client.send_event(event).await;
                    }

                    // Save the invite so we can process responses
                    let stored_invite = crate::storage::StoredInvite {
                        id: uuid::Uuid::new_v4().to_string(),
                        label: Some(format!("group:{}", id)),
                        url: invite_url,
                        created_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs(),
                        serialized: invite.serialize()?,
                    };
                    storage.save_invite(&stored_invite)?;
                }
            }
        }
    }

    output.success("group.accept", GroupInfo::from(&stored.data));
    Ok(())
}

/// Read group messages
pub async fn messages(
    id: &str,
    limit: usize,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    // Verify group exists
    storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let msgs = storage.get_group_messages(id, limit)?;

    let message_infos: Vec<GroupMessageInfo> = msgs
        .into_iter()
        .map(|m| GroupMessageInfo {
            id: m.id,
            group_id: m.group_id,
            sender_pubkey: m.sender_pubkey,
            content: m.content,
            timestamp: m.timestamp,
            is_outgoing: m.is_outgoing,
        })
        .collect();

    output.success("group.messages", GroupMessageList {
        group_id: id.to_string(),
        messages: message_infos,
    });

    Ok(())
}
