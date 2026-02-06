use anyhow::Result;
use base64::Engine;
use nostr::ToBech32;
use nostr_double_ratchet::{
    SenderKeyDistribution, SenderKeyState, Session, CHAT_MESSAGE_KIND, GROUP_METADATA_KIND,
    GROUP_SENDER_KEY_DISTRIBUTION_KIND, MESSAGE_EVENT_KIND, REACTION_KIND,
};
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredGroup, StoredGroupMessage, StoredGroupSender};

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

fn hex_to_npub(hex: &str) -> String {
    nostr::PublicKey::from_hex(hex)
        .ok()
        .and_then(|pk| pk.to_bech32().ok())
        .unwrap_or_else(|| hex.to_string())
}

impl From<&nostr_double_ratchet::group::GroupData> for GroupInfo {
    fn from(g: &nostr_double_ratchet::group::GroupData) -> Self {
        GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            picture: g.picture.clone(),
            members: g.members.iter().map(|m| hex_to_npub(m)).collect(),
            admins: g.admins.iter().map(|a| hex_to_npub(a)).collect(),
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

        let session_state: nostr_double_ratchet::SessionState =
            match serde_json::from_str(&chat.session_state) {
                Ok(s) => s,
                Err(_) => continue,
            };

        let mut session = Session::new(session_state, chat.id.clone());

        // Exclude secret for removed members
        let exclude_secret = excluded_member.map(|e| e == member).unwrap_or(false);
        let metadata_content =
            nostr_double_ratchet::group::build_group_metadata_content(group, exclude_secret);

        let tags: Vec<Vec<String>> = vec![
            vec!["l".to_string(), group.id.clone()],
            vec!["ms".to_string(), now_ms.to_string()],
        ];

        let nostr_tags: Vec<nostr::Tag> = tags
            .iter()
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
            if let Ok(state) =
                serde_json::from_str::<nostr_double_ratchet::SessionState>(&chat.session_state)
            {
                let mut session = Session::new(state, chat.id.clone());
                let metadata_content =
                    nostr_double_ratchet::group::build_group_metadata_content(group, true);

                let tags: Vec<Vec<String>> = vec![
                    vec!["l".to_string(), group.id.clone()],
                    vec!["ms".to_string(), now_ms.to_string()],
                ];

                let nostr_tags: Vec<nostr::Tag> = tags
                    .iter()
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

/// Fan-out a sender-key distribution to group members over existing 1:1 Double Ratchet sessions.
///
/// This is the Signal-style approach: sender keys are distributed pairwise with forward secrecy,
/// while group messages are later published once via a per-sender outer pubkey.
async fn fan_out_sender_key_distribution(
    group: &nostr_double_ratchet::group::GroupData,
    dist_json: &str,
    key_id: u32,
    now_ms: u64,
    now_s: u64,
    config: &Config,
    storage: &Storage,
    client: &Client,
) -> Result<()> {
    let my_pubkey = config.public_key()?;

    for member in &group.members {
        if member == &my_pubkey {
            continue;
        }

        let chat = match storage.get_chat_by_pubkey(member)? {
            Some(c) => c,
            None => continue,
        };

        let session_state: nostr_double_ratchet::SessionState =
            match serde_json::from_str(&chat.session_state) {
                Ok(s) => s,
                Err(_) => continue,
            };

        let mut session = Session::new(session_state, chat.id.clone());

        let tags: Vec<Vec<String>> = vec![
            vec!["l".to_string(), group.id.clone()],
            vec!["key".to_string(), key_id.to_string()],
            vec!["ms".to_string(), now_ms.to_string()],
        ];
        let nostr_tags: Vec<nostr::Tag> = tags
            .iter()
            .filter_map(|t| nostr::Tag::parse(t).ok())
            .collect();

        let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
        let unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            dist_json,
        )
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

        let encrypted = match session.send_event(unsigned) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mut updated_chat = chat;
        updated_chat.session_state = serde_json::to_string(&session.state)?;
        storage.save_chat(&updated_chat)?;

        let _ = client.send_event(encrypted).await;
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

    let stored = StoredGroup { data: group_data };
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
    let old_secret = group.data.secret.clone();
    let updated = nostr_double_ratchet::group::add_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add member: not admin or already a member"))?;
    let secret_rotated = updated.secret != old_secret;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // If membership changes rotated the shared-channel secret, force our sender key to rotate as well
    // so new members can decrypt future messages.
    if secret_rotated {
        let _ = storage.delete_group_sender_keys(id, &my_pubkey)?;
    }

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
    let old_secret = group.data.secret.clone();
    let updated = nostr_double_ratchet::group::remove_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot remove member: not admin, not a member, or trying to remove self"
            )
        })?;
    let secret_rotated = updated.secret != old_secret;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    if secret_rotated {
        let _ = storage.delete_group_sender_keys(id, &my_pubkey)?;
    }

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
        .ok_or_else(|| {
            anyhow::anyhow!("Cannot add admin: not admin, not a member, or already an admin")
        })?;

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
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot remove admin: not admin, target not admin, or would remove last admin"
            )
        })?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // Fan-out metadata
    let _ = fan_out_metadata(&stored.data, None, config, storage).await;

    output.success("group.remove-admin", GroupInfo::from(&stored.data));
    Ok(())
}

fn ensure_group_sender_event_keys(
    group_id: &str,
    identity_pubkey_hex: &str,
    storage: &Storage,
) -> Result<(nostr::Keys, bool)> {
    if let Some(stored) = storage.get_group_sender(group_id, identity_pubkey_hex)? {
        if let Some(sk_hex) = stored.sender_event_secret_key {
            if let Ok(sk_bytes) = hex::decode(&sk_hex) {
                if sk_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&sk_bytes);
                    let sk = nostr::SecretKey::from_slice(&arr)?;
                    let keys = nostr::Keys::new(sk);

                    // Keep stored pubkey consistent with the secret key.
                    let derived_pk_hex = keys.public_key().to_hex();
                    if derived_pk_hex != stored.sender_event_pubkey {
                        let updated = StoredGroupSender {
                            group_id: stored.group_id,
                            identity_pubkey: stored.identity_pubkey,
                            sender_event_pubkey: derived_pk_hex,
                            sender_event_secret_key: Some(sk_hex),
                        };
                        storage.upsert_group_sender(&updated)?;
                        return Ok((keys, true));
                    }

                    return Ok((keys, false));
                }
            }
        }
    }

    // Missing/invalid secret key: rotate to a fresh sender-event keypair for this group.
    let keys = nostr::Keys::generate();
    let sk_bytes = keys.secret_key().to_secret_bytes();
    let stored = StoredGroupSender {
        group_id: group_id.to_string(),
        identity_pubkey: identity_pubkey_hex.to_string(),
        sender_event_pubkey: keys.public_key().to_hex(),
        sender_event_secret_key: Some(hex::encode(sk_bytes)),
    };
    storage.upsert_group_sender(&stored)?;
    Ok((keys, true))
}

/// Send a group message (published once under our per-group sender-event pubkey, encrypted with our sender key).
pub async fn send_message(
    id: &str,
    message: &str,
    reply_to: Option<&str>,
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

    if group.data.accepted != Some(true) {
        anyhow::bail!("Group not accepted. Run: ndr group accept {}", id);
    }

    let my_pubkey = config.public_key()?;
    let (sender_event_keys, sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_pubkey, storage)?;
    let sender_event_pubkey_hex = sender_event_keys.public_key().to_hex();

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let now_s = now.as_secs();
    let now_ms = now.as_millis() as u64;

    // Prepare relay client once.
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    // Ensure we have an active sender key; distribute it to members over 1:1 sessions if we just created one.
    let mut created_sender_key = false;
    let mut sender_key = match storage.get_latest_group_sender_key_state(id, &my_pubkey)? {
        Some(s) => s,
        None => {
            created_sender_key = true;
            let key_id: u32 = rand::random();
            let chain_key: [u8; 32] = rand::random();
            let state = SenderKeyState::new(key_id, chain_key, 0);
            storage.upsert_group_sender_key_state(id, &my_pubkey, &state)?;

            let mut dist = SenderKeyDistribution::new(id.to_string(), key_id, chain_key, 0);
            dist.sender_event_pubkey = Some(sender_event_pubkey_hex.clone());
            let dist_json = serde_json::to_string(&dist)?;

            let _ = fan_out_sender_key_distribution(
                &group.data,
                &dist_json,
                key_id,
                now_ms,
                now_s,
                config,
                storage,
                &client,
            )
            .await;

            state
        }
    };

    // If our sender-event pubkey changed (e.g. state loss), re-announce it to the group over
    // forward-secure 1:1 sessions, even if our sender key already existed.
    if sender_event_keys_changed && !created_sender_key {
        let mut dist = SenderKeyDistribution::new(
            id.to_string(),
            sender_key.key_id,
            sender_key.chain_key(),
            sender_key.iteration(),
        );
        dist.sender_event_pubkey = Some(sender_event_pubkey_hex.clone());
        let dist_json = serde_json::to_string(&dist)?;
        let _ = fan_out_sender_key_distribution(
            &group.data,
            &dist_json,
            sender_key.key_id,
            now_ms,
            now_s,
            config,
            storage,
            &client,
        )
        .await;
    }

    // Build the plaintext group event (unsigned), then encrypt it with the sender key.
    let mut tags: Vec<Vec<String>> = Vec::new();
    if let Some(reply_id) = reply_to {
        tags.push(vec!["e".to_string(), reply_id.to_string()]);
    }
    tags.push(vec!["l".to_string(), id.to_string()]);
    tags.push(vec!["ms".to_string(), now_ms.to_string()]);

    let nostr_tags: Vec<nostr::Tag> = tags
        .iter()
        .filter_map(|t| nostr::Tag::parse(t).ok())
        .collect();

    let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
    let inner = nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), message)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

    let inner_json = serde_json::to_string(&inner)?;
    let (n, ciphertext_bytes) = sender_key.encrypt_to_bytes(&inner_json)?;
    storage.upsert_group_sender_key_state(id, &my_pubkey, &sender_key)?;

    // Publish once, authored by our per-group sender-event pubkey.
    //
    // Outer content format: base64(key_id||n||nip44_ciphertext_bytes). No tags that reveal group id.
    let mut payload: Vec<u8> = Vec::with_capacity(8 + ciphertext_bytes.len());
    payload.extend_from_slice(&sender_key.key_id.to_be_bytes());
    payload.extend_from_slice(&n.to_be_bytes());
    payload.extend_from_slice(&ciphertext_bytes);
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload);

    let outer_unsigned =
        nostr::EventBuilder::new(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16), &payload_b64)
            .custom_created_at(nostr::Timestamp::from(now_s))
            .build(sender_event_keys.public_key());
    let outer = outer_unsigned
        .sign_with_keys(&sender_event_keys)
        .map_err(|e| anyhow::anyhow!("Failed to sign group outer event: {}", e))?;
    client.send_event(outer.clone()).await?;

    // Store outgoing message using the outer event ID (stable across all recipients).
    let msg_id = outer.id.to_hex();
    let stored_msg = StoredGroupMessage {
        id: msg_id.clone(),
        group_id: id.to_string(),
        sender_pubkey: my_pubkey.clone(),
        content: message.to_string(),
        timestamp: now_s,
        is_outgoing: true,
    };
    storage.save_group_message(&stored_msg)?;

    output.success(
        "group.send",
        serde_json::json!({
            "id": msg_id,
            "group_id": id,
            "content": message,
            "timestamp": now_s,
            "published": true,
        }),
    );

    Ok(())
}

/// React to a group message (published once under our per-group sender-event pubkey, encrypted with our sender key).
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

    if group.data.accepted != Some(true) {
        anyhow::bail!("Group not accepted. Run: ndr group accept {}", id);
    }

    let my_pubkey = config.public_key()?;
    let (sender_event_keys, sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_pubkey, storage)?;
    let sender_event_pubkey_hex = sender_event_keys.public_key().to_hex();

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let now_s = now.as_secs();
    let now_ms = now.as_millis() as u64;

    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    let mut created_sender_key = false;
    let mut sender_key = match storage.get_latest_group_sender_key_state(id, &my_pubkey)? {
        Some(s) => s,
        None => {
            created_sender_key = true;
            let key_id: u32 = rand::random();
            let chain_key: [u8; 32] = rand::random();
            let state = SenderKeyState::new(key_id, chain_key, 0);
            storage.upsert_group_sender_key_state(id, &my_pubkey, &state)?;

            let mut dist = SenderKeyDistribution::new(id.to_string(), key_id, chain_key, 0);
            dist.sender_event_pubkey = Some(sender_event_pubkey_hex.clone());
            let dist_json = serde_json::to_string(&dist)?;

            let _ = fan_out_sender_key_distribution(
                &group.data,
                &dist_json,
                key_id,
                now_ms,
                now_s,
                config,
                storage,
                &client,
            )
            .await;

            state
        }
    };

    if sender_event_keys_changed && !created_sender_key {
        let mut dist = SenderKeyDistribution::new(
            id.to_string(),
            sender_key.key_id,
            sender_key.chain_key(),
            sender_key.iteration(),
        );
        dist.sender_event_pubkey = Some(sender_event_pubkey_hex.clone());
        let dist_json = serde_json::to_string(&dist)?;
        let _ = fan_out_sender_key_distribution(
            &group.data,
            &dist_json,
            sender_key.key_id,
            now_ms,
            now_s,
            config,
            storage,
            &client,
        )
        .await;
    }

    let mut tags: Vec<Vec<String>> = Vec::new();
    tags.push(vec!["e".to_string(), message_id.to_string()]);
    tags.push(vec!["l".to_string(), id.to_string()]);
    tags.push(vec!["ms".to_string(), now_ms.to_string()]);
    let nostr_tags: Vec<nostr::Tag> = tags
        .iter()
        .filter_map(|t| nostr::Tag::parse(t).ok())
        .collect();

    let my_pk = nostr::PublicKey::from_hex(&my_pubkey)?;
    let inner = nostr::EventBuilder::new(nostr::Kind::Custom(REACTION_KIND as u16), emoji)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

    let inner_json = serde_json::to_string(&inner)?;
    let (n, ciphertext_bytes) = sender_key.encrypt_to_bytes(&inner_json)?;
    storage.upsert_group_sender_key_state(id, &my_pubkey, &sender_key)?;

    let mut payload: Vec<u8> = Vec::with_capacity(8 + ciphertext_bytes.len());
    payload.extend_from_slice(&sender_key.key_id.to_be_bytes());
    payload.extend_from_slice(&n.to_be_bytes());
    payload.extend_from_slice(&ciphertext_bytes);
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload);

    let outer_unsigned =
        nostr::EventBuilder::new(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16), &payload_b64)
            .custom_created_at(nostr::Timestamp::from(now_s))
            .build(sender_event_keys.public_key());
    let outer = outer_unsigned
        .sign_with_keys(&sender_event_keys)
        .map_err(|e| anyhow::anyhow!("Failed to sign group outer event: {}", e))?;
    client.send_event(outer).await?;

    output.success(
        "group.react",
        serde_json::json!({
            "group_id": id,
            "message_id": message_id,
            "emoji": emoji,
            "published": true,
        }),
    );

    Ok(())
}

/// Rotate our sender key for a group and fan-out a fresh distribution over 1:1 Double Ratchet sessions.
pub async fn rotate_sender_key(
    id: &str,
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

    if group.data.accepted != Some(true) {
        anyhow::bail!("Group not accepted. Run: ndr group accept {}", id);
    }

    let my_pubkey = config.public_key()?;
    let (sender_event_keys, _sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_pubkey, storage)?;
    let sender_event_pubkey_hex = sender_event_keys.public_key().to_hex();

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let now_s = now.as_secs();
    let now_ms = now.as_millis() as u64;

    // Prepare relay client once.
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    // Create and store a new sender key state.
    let key_id: u32 = rand::random();
    let chain_key: [u8; 32] = rand::random();
    let state = SenderKeyState::new(key_id, chain_key, 0);
    storage.upsert_group_sender_key_state(id, &my_pubkey, &state)?;

    // Distribute the sender key to the group via 1:1 Double Ratchet sessions (forward secrecy).
    let mut dist = SenderKeyDistribution::new(id.to_string(), key_id, chain_key, 0);
    dist.sender_event_pubkey = Some(sender_event_pubkey_hex);
    let dist_json = serde_json::to_string(&dist)?;
    let _ = fan_out_sender_key_distribution(
        &group.data,
        &dist_json,
        key_id,
        now_ms,
        now_s,
        config,
        storage,
        &client,
    )
    .await;

    output.success(
        "group.rotate-sender-key",
        serde_json::json!({
            "group_id": id,
            "key_id": key_id,
            "published": true,
        }),
    );

    Ok(())
}

/// Accept a group invitation
pub async fn accept(id: &str, config: &Config, storage: &Storage, output: &Output) -> Result<()> {
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
                    let invite_url = invite.get_url("https://chat.iris.to")?;

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
                    })
                    .to_string();

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
pub async fn messages(id: &str, limit: usize, storage: &Storage, output: &Output) -> Result<()> {
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

    output.success(
        "group.messages",
        GroupMessageList {
            group_id: id.to_string(),
            messages: message_infos,
        },
    );

    Ok(())
}
