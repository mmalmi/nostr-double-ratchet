use anyhow::Result;
use crossbeam_channel::{Receiver, TryRecvError};
use nostr::ToBech32;
use nostr_double_ratchet::{
    FileStorageAdapter, SenderKeyDistribution, SenderKeyState, Session, SessionManager,
    SessionManagerEvent, SharedChannel, StorageAdapter, CHAT_MESSAGE_KIND, GROUP_METADATA_KIND,
    GROUP_SENDER_KEY_DISTRIBUTION_KIND, REACTION_KIND,
};
use nostr_sdk::Client;
use serde::Serialize;
use std::sync::Arc;

use crate::config::Config;
use crate::nostr_client::send_event_or_ignore;
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

fn import_chats_into_session_manager(
    storage: &Storage,
    manager: &SessionManager,
    my_owner_pubkey_hex: &str,
) -> Result<()> {
    let known: std::collections::HashMap<(String, String), String> = manager
        .export_active_sessions()
        .into_iter()
        .filter_map(|(owner, device_id, state)| {
            serde_json::to_string(&state)
                .ok()
                .map(|json| ((owner.to_hex(), device_id), json))
        })
        .collect();

    for chat in storage.list_chats()? {
        if chat.their_pubkey == my_owner_pubkey_hex {
            continue;
        }

        let owner_pubkey = match nostr::PublicKey::from_hex(&chat.their_pubkey) {
            Ok(pk) => pk,
            Err(_) => continue,
        };
        manager.setup_user(owner_pubkey);

        let device_id = chat.device_id.clone().unwrap_or_else(|| chat.id.clone());
        if known
            .get(&(owner_pubkey.to_hex(), device_id.clone()))
            .is_some_and(|known_state| known_state == &chat.session_state)
        {
            continue;
        }

        let state: nostr_double_ratchet::SessionState =
            match serde_json::from_str(&chat.session_state) {
                Ok(state) => state,
                Err(_) => continue,
            };

        manager.import_session_state(owner_pubkey, Some(device_id), state)?;
    }

    Ok(())
}

fn sync_member_chats_from_session_manager(
    storage: &Storage,
    manager: &SessionManager,
    member_owner_pubkey_hex: &str,
) -> Result<()> {
    use std::collections::HashMap;

    let sessions_by_device: HashMap<String, nostr_double_ratchet::SessionState> = manager
        .export_active_sessions()
        .into_iter()
        .filter_map(|(owner_pubkey, device_id, state)| {
            (owner_pubkey.to_hex() == member_owner_pubkey_hex).then_some((device_id, state))
        })
        .collect();

    if sessions_by_device.is_empty() {
        return Ok(());
    }

    for mut chat in storage
        .list_chats()?
        .into_iter()
        .filter(|c| c.their_pubkey == member_owner_pubkey_hex)
    {
        let device_id = chat.device_id.clone().unwrap_or_else(|| chat.id.clone());
        let Some(state) = sessions_by_device.get(&device_id) else {
            continue;
        };
        let state_json = serde_json::to_string(state)?;

        let mut changed = false;
        if chat.device_id.as_deref() != Some(device_id.as_str()) {
            chat.device_id = Some(device_id);
            changed = true;
        }
        if chat.session_state != state_json {
            chat.session_state = state_json;
            changed = true;
        }

        if changed {
            storage.save_chat(&chat)?;
        }
    }

    Ok(())
}

fn build_session_manager(
    config: &Config,
    storage: &Storage,
) -> Result<(SessionManager, Receiver<SessionManagerEvent>)> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: Arc<dyn StorageAdapter> = Arc::new(FileStorageAdapter::new(
        storage.data_dir().join("session_manager"),
    )?);

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_pubkey,
        our_private_key,
        our_pubkey_hex,
        owner_pubkey,
        tx,
        Some(session_manager_store),
        None,
    );
    manager.init()?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    // Drop any initial SessionManager events (device invite publication, subscribe requests, etc).
    // Group commands only care about publishing ratchet message events that they explicitly send.
    loop {
        match rx.try_recv() {
            Ok(_) => continue,
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
    Ok((manager, rx))
}

fn drain_session_manager_message_events(rx: &Receiver<SessionManagerEvent>) -> Vec<nostr::Event> {
    let mut message_events = Vec::new();

    loop {
        match rx.try_recv() {
            Ok(SessionManagerEvent::PublishSigned(event))
                if event.kind.as_u16() == nostr_double_ratchet::MESSAGE_EVENT_KIND as u16 =>
            {
                message_events.push(event);
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }

    message_events
}

async fn publish_events_with_retry(
    client: &Client,
    events: &[nostr::Event],
    attempts: usize,
    retry_ms: u64,
) -> bool {
    if events.is_empty() {
        return false;
    }

    for attempt in 0..attempts {
        let mut all_ok = true;
        for ev in events {
            if send_event_or_ignore(client, ev.clone()).await.is_err() {
                all_ok = false;
                break;
            }
        }

        if all_ok {
            return true;
        }

        if attempt + 1 < attempts {
            tokio::time::sleep(std::time::Duration::from_millis(retry_ms)).await;
        }
    }

    false
}

async fn publish_sender_key_distribution_shared_channel(
    group: &nostr_double_ratchet::group::GroupData,
    dist_json: &str,
    key_id: u32,
    now_ms: u64,
    now_s: u64,
    config: &Config,
    client: &Client,
) -> Result<()> {
    let Some(secret_hex) = group.secret.as_deref() else {
        return Ok(());
    };
    let Ok(secret_bytes) = hex::decode(secret_hex) else {
        return Ok(());
    };
    if secret_bytes.len() != 32 {
        return Ok(());
    }
    let mut secret_arr = [0u8; 32];
    secret_arr.copy_from_slice(&secret_bytes);
    let Ok(channel) = SharedChannel::new(&secret_arr) else {
        return Ok(());
    };

    let my_device_pubkey_hex = config.public_key()?;
    let my_owner_pubkey_hex = config.owner_public_key_hex()?;
    let my_device_private_key = config.private_key_bytes()?;
    let my_device_secret_key = nostr::SecretKey::from_slice(&my_device_private_key)?;
    let my_device_keys = nostr::Keys::new(my_device_secret_key);
    let my_device_pk = nostr::PublicKey::from_hex(&my_device_pubkey_hex)?;

    // Include owner pubkey claim so recipients can attribute linked devices correctly.
    let tags: Vec<Vec<String>> = vec![
        vec!["l".to_string(), group.id.clone()],
        vec!["key".to_string(), key_id.to_string()],
        vec!["ms".to_string(), now_ms.to_string()],
        vec!["p".to_string(), my_owner_pubkey_hex],
    ];
    let nostr_tags: Vec<nostr::Tag> = tags
        .iter()
        .filter_map(|t| nostr::Tag::parse(t).ok())
        .collect();

    let inner_unsigned = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        dist_json,
    )
    .tags(nostr_tags)
    .custom_created_at(nostr::Timestamp::from(now_s))
    .build(my_device_pk);
    let inner_signed = inner_unsigned.sign_with_keys(&my_device_keys)?;

    let outer = channel.create_event(&nostr::JsonUtil::as_json(&inner_signed))?;
    send_event_or_ignore(client, outer).await?;
    Ok(())
}

/// Fan-out group metadata to members after a mutation.
async fn fan_out_metadata(
    group: &nostr_double_ratchet::group::GroupData,
    excluded_member: Option<&str>,
    config: &Config,
    storage: &Storage,
) -> Result<()> {
    let my_owner_pubkey = config.owner_public_key_hex()?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;

    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    let (session_manager, session_manager_rx) = build_session_manager(config, storage)?;
    const MAX_DELIVERY_ATTEMPTS: usize = 20;
    const DELIVERY_RETRY_MS: u64 = 100;

    for member in &group.members {
        if member == &my_owner_pubkey {
            continue;
        }

        let exclude_secret = excluded_member.map(|e| e == member).unwrap_or(false);
        let metadata_content =
            nostr_double_ratchet::group::build_group_metadata_content(group, exclude_secret);
        let recipient_owner = match nostr::PublicKey::from_hex(member) {
            Ok(pk) => pk,
            Err(_) => continue,
        };

        let tags: Vec<Vec<String>> = vec![
            vec!["l".to_string(), group.id.clone()],
            vec!["ms".to_string(), now_ms.to_string()],
        ];
        let nostr_tags: Vec<nostr::Tag> = tags
            .iter()
            .filter_map(|t| nostr::Tag::parse(t).ok())
            .collect();

        let my_pk = nostr::PublicKey::from_hex(&my_owner_pubkey)?;
        let unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_METADATA_KIND as u16),
            &metadata_content,
        )
        .tags(nostr_tags)
        .build(my_pk);

        let mut message_events: Vec<nostr::Event> = Vec::new();
        for attempt in 0..MAX_DELIVERY_ATTEMPTS {
            import_chats_into_session_manager(storage, &session_manager, &my_owner_pubkey)?;
            let event_ids = session_manager
                .send_event_recipient_only(recipient_owner, unsigned.clone())
                .unwrap_or_default();
            let drained = drain_session_manager_message_events(&session_manager_rx);
            sync_member_chats_from_session_manager(storage, &session_manager, member)?;

            if !drained.is_empty() && !event_ids.is_empty() {
                message_events = drained;
                break;
            }

            if attempt + 1 < MAX_DELIVERY_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(DELIVERY_RETRY_MS)).await;
            }
        }

        let published = publish_events_with_retry(
            &client,
            &message_events,
            MAX_DELIVERY_ATTEMPTS,
            DELIVERY_RETRY_MS,
        )
        .await;
        if published {
            continue;
        }
    }

    // Also fan-out without secret to the removed member
    if let Some(removed) = excluded_member {
        let metadata_content =
            nostr_double_ratchet::group::build_group_metadata_content(group, true);
        let recipient_owner = match nostr::PublicKey::from_hex(removed) {
            Ok(pk) => pk,
            Err(_) => return Ok(()),
        };

        let tags: Vec<Vec<String>> = vec![
            vec!["l".to_string(), group.id.clone()],
            vec!["ms".to_string(), now_ms.to_string()],
        ];
        let nostr_tags: Vec<nostr::Tag> = tags
            .iter()
            .filter_map(|t| nostr::Tag::parse(t).ok())
            .collect();
        let my_pk = nostr::PublicKey::from_hex(&my_owner_pubkey)?;
        let unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_METADATA_KIND as u16),
            &metadata_content,
        )
        .tags(nostr_tags)
        .build(my_pk);

        let mut message_events: Vec<nostr::Event> = Vec::new();
        for attempt in 0..MAX_DELIVERY_ATTEMPTS {
            import_chats_into_session_manager(storage, &session_manager, &my_owner_pubkey)?;
            let event_ids = session_manager
                .send_event_recipient_only(recipient_owner, unsigned.clone())
                .unwrap_or_default();
            let drained = drain_session_manager_message_events(&session_manager_rx);
            sync_member_chats_from_session_manager(storage, &session_manager, removed)?;

            if !drained.is_empty() && !event_ids.is_empty() {
                message_events = drained;
                break;
            }

            if attempt + 1 < MAX_DELIVERY_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(DELIVERY_RETRY_MS)).await;
            }
        }

        let _ = publish_events_with_retry(
            &client,
            &message_events,
            MAX_DELIVERY_ATTEMPTS,
            DELIVERY_RETRY_MS,
        )
        .await;
    }

    Ok(())
}

/// Fan-out a sender-key distribution to group members over existing 1:1 Double Ratchet sessions.
///
/// This is the Signal-style approach: sender keys are distributed pairwise with forward secrecy,
/// while group messages are later published once via a per-sender outer pubkey.
#[allow(clippy::too_many_arguments)]
async fn fan_out_sender_key_distribution(
    group: &nostr_double_ratchet::group::GroupData,
    dist_json: &str,
    key_id: u32,
    now_ms: u64,
    now_s: u64,
    config: &Config,
    storage: &Storage,
    client: &Client,
) -> Result<Vec<String>> {
    let my_device_pubkey = config.public_key()?;
    let my_owner_pubkey = config.owner_public_key_hex()?;
    let (session_manager, session_manager_rx) = build_session_manager(config, storage)?;
    const MAX_DELIVERY_ATTEMPTS: usize = 20;
    const DELIVERY_RETRY_MS: u64 = 100;
    let mut delivered_members: Vec<String> = Vec::new();

    for member in &group.members {
        if member == &my_owner_pubkey {
            continue;
        }

        let tags: Vec<Vec<String>> = vec![
            vec!["l".to_string(), group.id.clone()],
            vec!["key".to_string(), key_id.to_string()],
            vec!["ms".to_string(), now_ms.to_string()],
        ];
        let nostr_tags: Vec<nostr::Tag> = tags
            .iter()
            .filter_map(|t| nostr::Tag::parse(t).ok())
            .collect();

        let my_pk = nostr::PublicKey::from_hex(&my_device_pubkey)?;
        let unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            dist_json,
        )
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

        let recipient_owner = match nostr::PublicKey::from_hex(member) {
            Ok(pk) => pk,
            Err(_) => continue,
        };

        let mut delivered = false;
        let mut message_events: Vec<nostr::Event> = Vec::new();
        for attempt in 0..MAX_DELIVERY_ATTEMPTS {
            // Pull in any chats that may have been created concurrently by `ndr message listen`.
            import_chats_into_session_manager(storage, &session_manager, &my_owner_pubkey)?;

            let event_ids = session_manager
                .send_event_recipient_only(recipient_owner, unsigned.clone())
                .unwrap_or_default();
            let drained = drain_session_manager_message_events(&session_manager_rx);
            sync_member_chats_from_session_manager(storage, &session_manager, member)?;

            if !drained.is_empty() && !event_ids.is_empty() {
                message_events = drained;
                break;
            }

            if attempt + 1 < MAX_DELIVERY_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(DELIVERY_RETRY_MS)).await;
            }
        }

        if publish_events_with_retry(
            client,
            &message_events,
            MAX_DELIVERY_ATTEMPTS,
            DELIVERY_RETRY_MS,
        )
        .await
        {
            delivered = true;
        }

        if !delivered {
            let member_chats: Vec<_> = storage
                .list_chats()?
                .into_iter()
                .filter(|c| c.their_pubkey == *member)
                .collect();

            for chat in member_chats {
                let session_state: nostr_double_ratchet::SessionState =
                    match serde_json::from_str(&chat.session_state) {
                        Ok(state) => state,
                        Err(_) => continue,
                    };

                let mut session = Session::new(session_state, chat.id.clone());
                let encrypted = match session.send_event(unsigned.clone()) {
                    Ok(event) => event,
                    Err(_) => continue,
                };

                let mut published = false;
                for _ in 0..3 {
                    if send_event_or_ignore(client, encrypted.clone())
                        .await
                        .is_ok()
                    {
                        published = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }

                if !published {
                    continue;
                }

                let mut updated_chat = chat.clone();
                updated_chat.session_state = serde_json::to_string(&session.state)?;
                storage.save_chat(&updated_chat)?;
                delivered = true;
                break;
            }
        }

        if !delivered {
            continue;
        }
        delivered_members.push(member.clone());
    }

    Ok(delivered_members)
}

pub async fn create(
    name: &str,
    members: &[String],
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    // Group membership is expressed in *owner pubkeys* so it works with linked-device mode.
    let my_pubkey = config.owner_public_key_hex()?;
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

    let my_pubkey = config.owner_public_key_hex()?;
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

    let my_pubkey = config.owner_public_key_hex()?;
    let old_secret = group.data.secret.clone();
    let updated = nostr_double_ratchet::group::add_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add member: not admin or already a member"))?;
    let secret_rotated = updated.secret != old_secret;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    // If membership changes rotated the shared-channel secret, force our sender key to rotate as well
    // so new members can decrypt future messages.
    if secret_rotated {
        let my_device_pubkey = config.public_key()?;
        let _ = storage.delete_group_sender_keys(id, &my_device_pubkey)?;
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

    let my_pubkey = config.owner_public_key_hex()?;
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
        let my_device_pubkey = config.public_key()?;
        let _ = storage.delete_group_sender_keys(id, &my_device_pubkey)?;
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

    let my_pubkey = config.owner_public_key_hex()?;
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

    let my_pubkey = config.owner_public_key_hex()?;
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
    device_pubkey_hex: &str,
    owner_pubkey_hex: &str,
    storage: &Storage,
) -> Result<(nostr::Keys, bool)> {
    if let Some(stored) = storage.get_group_sender(group_id, device_pubkey_hex)? {
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
                            owner_pubkey: Some(owner_pubkey_hex.to_string()),
                            sender_event_pubkey: derived_pk_hex,
                            sender_event_secret_key: Some(sk_hex),
                        };
                        storage.upsert_group_sender(&updated)?;
                        return Ok((keys, true));
                    }

                    // Backfill/migrate owner pubkey if missing.
                    if stored.owner_pubkey.as_deref() != Some(owner_pubkey_hex) {
                        let updated = StoredGroupSender {
                            group_id: stored.group_id,
                            identity_pubkey: stored.identity_pubkey,
                            owner_pubkey: Some(owner_pubkey_hex.to_string()),
                            sender_event_pubkey: stored.sender_event_pubkey,
                            sender_event_secret_key: Some(sk_hex),
                        };
                        storage.upsert_group_sender(&updated)?;
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
        identity_pubkey: device_pubkey_hex.to_string(),
        owner_pubkey: Some(owner_pubkey_hex.to_string()),
        sender_event_pubkey: keys.public_key().to_hex(),
        sender_event_secret_key: Some(hex::encode(sk_bytes)),
    };
    storage.upsert_group_sender(&stored)?;
    Ok((keys, true))
}

#[allow(clippy::too_many_arguments)]
async fn ensure_group_sender_key(
    group: &nostr_double_ratchet::group::GroupData,
    group_id: &str,
    my_pubkey: &str,
    sender_event_pubkey_hex: &str,
    sender_event_keys_changed: bool,
    now_ms: u64,
    now_s: u64,
    config: &Config,
    storage: &Storage,
    client: &Client,
) -> Result<SenderKeyState> {
    // Ensure we have an active sender key; distribute it to members over 1:1 sessions if we just
    // created one.
    let mut created_sender_key = false;
    let sender_key = match storage.get_latest_group_sender_key_state(group_id, my_pubkey)? {
        Some(s) => s,
        None => {
            created_sender_key = true;

            let key_id: u32 = rand::random();
            let chain_key: [u8; 32] = rand::random();
            let state = SenderKeyState::new(key_id, chain_key, 0);
            storage.upsert_group_sender_key_state(group_id, my_pubkey, &state)?;

            let mut dist = SenderKeyDistribution::new(group_id.to_string(), key_id, chain_key, 0);
            dist.sender_event_pubkey = Some(sender_event_pubkey_hex.to_string());
            let dist_json = serde_json::to_string(&dist)?;

            // Reliability: also publish the distribution on the group's SharedChannel so members
            // can learn our sender-event pubkey and sender key even if 1:1 delivery is delayed.
            let _ = publish_sender_key_distribution_shared_channel(
                group, &dist_json, key_id, now_ms, now_s, config, client,
            )
            .await;
            let _ = fan_out_sender_key_distribution(
                group, &dist_json, key_id, now_ms, now_s, config, storage, client,
            )
            .await;

            state
        }
    };

    // If our sender-event pubkey changed (e.g. state loss), re-announce it to the group over
    // forward-secure 1:1 sessions, even if our sender key already existed.
    if sender_event_keys_changed && !created_sender_key {
        let mut dist = SenderKeyDistribution::new(
            group_id.to_string(),
            sender_key.key_id,
            sender_key.chain_key(),
            sender_key.iteration(),
        );
        dist.sender_event_pubkey = Some(sender_event_pubkey_hex.to_string());
        let dist_json = serde_json::to_string(&dist)?;
        let _ = publish_sender_key_distribution_shared_channel(
            group,
            &dist_json,
            sender_key.key_id,
            now_ms,
            now_s,
            config,
            client,
        )
        .await;
        let _ = fan_out_sender_key_distribution(
            group,
            &dist_json,
            sender_key.key_id,
            now_ms,
            now_s,
            config,
            storage,
            client,
        )
        .await;
    }

    Ok(sender_key)
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

    let my_device_pubkey = config.public_key()?;
    let my_owner_pubkey = config.owner_public_key_hex()?;
    let (sender_event_keys, sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_device_pubkey, &my_owner_pubkey, storage)?;
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

    let mut sender_key = ensure_group_sender_key(
        &group.data,
        id,
        &my_device_pubkey,
        &sender_event_pubkey_hex,
        sender_event_keys_changed,
        now_ms,
        now_s,
        config,
        storage,
        &client,
    )
    .await?;

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

    let my_pk = nostr::PublicKey::from_hex(&my_device_pubkey)?;
    let inner = nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), message)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

    let inner_json = serde_json::to_string(&inner)?;
    let channel = nostr_double_ratchet::OneToManyChannel::default();
    let outer = channel
        .encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_key,
            &inner_json,
            nostr::Timestamp::from(now_s),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create group outer event: {}", e))?;
    storage.upsert_group_sender_key_state(id, &my_device_pubkey, &sender_key)?;
    send_event_or_ignore(&client, outer.clone()).await?;

    // Store outgoing message using the outer event ID (stable across all recipients).
    let msg_id = outer.id.to_hex();
    let stored_msg = StoredGroupMessage {
        id: msg_id.clone(),
        group_id: id.to_string(),
        sender_pubkey: my_owner_pubkey.clone(),
        content: message.to_string(),
        timestamp: now_s,
        is_outgoing: true,
        expires_at: None,
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

    let my_device_pubkey = config.public_key()?;
    let my_owner_pubkey = config.owner_public_key_hex()?;
    let (sender_event_keys, sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_device_pubkey, &my_owner_pubkey, storage)?;
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

    let mut sender_key = ensure_group_sender_key(
        &group.data,
        id,
        &my_device_pubkey,
        &sender_event_pubkey_hex,
        sender_event_keys_changed,
        now_ms,
        now_s,
        config,
        storage,
        &client,
    )
    .await?;

    let tags: Vec<Vec<String>> = vec![
        vec!["e".to_string(), message_id.to_string()],
        vec!["l".to_string(), id.to_string()],
        vec!["ms".to_string(), now_ms.to_string()],
    ];
    let nostr_tags: Vec<nostr::Tag> = tags
        .iter()
        .filter_map(|t| nostr::Tag::parse(t).ok())
        .collect();

    let my_pk = nostr::PublicKey::from_hex(&my_device_pubkey)?;
    let inner = nostr::EventBuilder::new(nostr::Kind::Custom(REACTION_KIND as u16), emoji)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(my_pk);

    let inner_json = serde_json::to_string(&inner)?;
    let channel = nostr_double_ratchet::OneToManyChannel::default();
    let outer = channel
        .encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_key,
            &inner_json,
            nostr::Timestamp::from(now_s),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create group outer event: {}", e))?;
    storage.upsert_group_sender_key_state(id, &my_device_pubkey, &sender_key)?;
    send_event_or_ignore(&client, outer).await?;

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

    let my_device_pubkey = config.public_key()?;
    let my_owner_pubkey = config.owner_public_key_hex()?;
    let (sender_event_keys, _sender_event_keys_changed) =
        ensure_group_sender_event_keys(id, &my_device_pubkey, &my_owner_pubkey, storage)?;
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
    storage.upsert_group_sender_key_state(id, &my_device_pubkey, &state)?;

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
                    let my_device_pubkey = config.public_key()?;
                    let my_owner_pubkey_hex = config.owner_public_key_hex()?;
                    let my_owner_pubkey = nostr::PublicKey::from_hex(&my_owner_pubkey_hex)?;
                    let my_device_private_key = config.private_key_bytes()?;
                    let my_device_secret_key =
                        nostr::SecretKey::from_slice(&my_device_private_key)?;
                    let my_device_keys = nostr::Keys::new(my_device_secret_key);

                    // Create an invite for group members to establish 1:1 sessions
                    let my_device_pk = nostr::PublicKey::from_hex(&my_device_pubkey)?;
                    let mut invite =
                        nostr_double_ratchet::Invite::create_new(my_device_pk, None, None)?;
                    // Help invitees attribute this device to an owner identity.
                    invite.owner_public_key = Some(my_owner_pubkey);
                    let invite_url = invite.get_url("https://chat.iris.to")?;

                    let inner_content = serde_json::json!({
                        "inviteUrl": invite_url,
                        "groupId": id,
                        "ownerPubkey": my_owner_pubkey_hex,
                    })
                    .to_string();
                    let inner_unsigned = nostr::EventBuilder::new(
                        nostr::Kind::Custom(nostr_double_ratchet::GROUP_INVITE_RUMOR_KIND as u16),
                        inner_content,
                    )
                    .tag(
                        nostr::Tag::parse(&["l".to_string(), id.to_string()])
                            .map_err(|e| anyhow::anyhow!("Invalid group tag: {}", e))?,
                    )
                    .build(my_device_pk);
                    let inner_signed = inner_unsigned.sign_with_keys(&my_device_keys)?;

                    if let Ok(event) =
                        channel.create_event(&nostr::JsonUtil::as_json(&inner_signed))
                    {
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

    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let _ = storage.purge_expired_group_messages(id, now_seconds);

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
