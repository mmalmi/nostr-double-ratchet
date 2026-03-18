use anyhow::{Context, Result};

use nostr_double_ratchet::{
    FileStorageAdapter, NdrRuntime, OneToManyChannel, Session, SessionManager, SessionManagerEvent,
    StorageAdapter, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, GROUP_METADATA_KIND, REACTION_KIND,
    RECEIPT_KIND, TYPING_KIND,
};

use crate::commands::owner_claim::{
    fetch_latest_app_keys, fetch_latest_app_keys_snapshot, resolve_verified_owner_pubkey,
};
use crate::config::Config;
use crate::nostr_client::{
    fetch_events_best_effort, send_event_or_ignore, subscribe_filters_best_effort,
};
use crate::output::Output;
use crate::state_sync::{
    apply_chat_settings, apply_group_metadata, extract_control_stamp_from_value,
    select_canonical_session, GroupMetadataApplyOutcome,
};
use crate::storage::{
    Storage, StoredChat, StoredGroupMessage, StoredGroupSender, StoredMessage, StoredReaction,
};

use super::common::{
    collect_chat_pubkeys, extract_e_tag, extract_e_tags, extract_expiration_tag_seconds,
    is_expired, parse_chat_settings_ttl_seconds,
};
use super::types::{IncomingMessage, IncomingReaction};

const PEER_APP_KEYS_REFRESH_INTERVAL_MS: u64 = 2_000;
const MAX_PENDING_SESSION_MANAGER_MESSAGE_EVENTS: usize = 256;
const MAX_SEEN_EVENT_IDS: usize = 20_000;

fn build_session_manager(
    config: &Config,
    storage: &Storage,
) -> Result<(
    NdrRuntime,
    String,
    nostr::PublicKey,
    String,
    nostr::PublicKey,
)> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: std::sync::Arc<dyn StorageAdapter> = std::sync::Arc::new(
        FileStorageAdapter::new(storage.data_dir().join("session_manager"))?,
    );

    let runtime = NdrRuntime::new(
        our_pubkey,
        our_private_key,
        our_pubkey_hex.clone(),
        owner_pubkey,
        Some(session_manager_store),
        None,
    );
    runtime.init()?;

    Ok((
        runtime,
        our_pubkey_hex,
        our_pubkey,
        owner_pubkey_hex,
        owner_pubkey,
    ))
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

fn sync_chats_from_session_manager(
    storage: &Storage,
    manager: &SessionManager,
    my_owner_pubkey_hex: &str,
) -> Result<()> {
    use std::collections::HashMap;

    let sessions = manager.export_active_sessions();
    if sessions.is_empty() {
        return Ok(());
    }

    let mut sessions_by_owner: HashMap<String, Vec<(String, nostr_double_ratchet::SessionState)>> =
        HashMap::new();
    for (owner_pubkey, device_id, state) in sessions {
        let owner_hex = owner_pubkey.to_hex();
        if owner_hex == my_owner_pubkey_hex {
            // Keep owner-sibling sync state in SessionManager storage only.
            continue;
        }
        sessions_by_owner
            .entry(owner_hex)
            .or_default()
            .push((device_id, state));
    }

    if sessions_by_owner.is_empty() {
        return Ok(());
    }

    let mut chats = storage.list_chats()?;

    for (owner_hex, mut owner_sessions) in sessions_by_owner {
        owner_sessions.sort_by(|a, b| a.0.cmp(&b.0));
        let Some((selected_device_id, selected_state)) =
            select_canonical_session(&owner_hex, &owner_sessions)
        else {
            continue;
        };

        if let Some(idx) = chats.iter().position(|chat| chat.their_pubkey == owner_hex) {
            let mut changed = false;
            let mut chat = chats[idx].clone();
            let state_json = serde_json::to_string(&selected_state)?;

            if chat.device_id.as_deref() != Some(selected_device_id.as_str()) {
                chat.device_id = Some(selected_device_id.clone());
                changed = true;
            }
            if chat.session_state != state_json {
                chat.session_state = state_json.clone();
                changed = true;
            }

            if changed {
                storage.save_chat(&chat)?;
                chats[idx] = chat;
            }
            continue;
        }

        let state_json = serde_json::to_string(&selected_state)?;
        let chat = crate::storage::StoredChat {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            their_pubkey: owner_hex,
            device_id: Some(selected_device_id),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            last_message_at: None,
            session_state: state_json,
            message_ttl_seconds: None,
        };
        storage.save_chat(&chat)?;
        chats.push(chat);
    }

    Ok(())
}

fn collect_chat_pubkeys_with_session_manager(
    storage: &Storage,
    session_manager: &SessionManager,
    chat_id: Option<&str>,
) -> Result<Vec<nostr::PublicKey>> {
    let chats = if let Some(id) = chat_id {
        vec![storage
            .get_chat(id)?
            .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?]
    } else {
        storage.list_chats()?
    };

    let mut owners = std::collections::HashSet::new();
    for chat in &chats {
        owners.insert(chat.their_pubkey.clone());
    }

    let mut seen = std::collections::HashSet::new();
    let mut pubkeys = Vec::new();
    for pubkey in collect_chat_pubkeys(storage, chat_id)? {
        if seen.insert(pubkey.to_hex()) {
            pubkeys.push(pubkey);
        }
    }

    for (owner, _device_id, state) in session_manager.export_active_sessions() {
        if !owners.contains(&owner.to_hex()) {
            continue;
        }

        for maybe_pubkey in [
            state.their_current_nostr_public_key,
            state.their_next_nostr_public_key,
        ] {
            if let Some(pubkey) = maybe_pubkey {
                if seen.insert(pubkey.to_hex()) {
                    pubkeys.push(pubkey);
                }
            }
        }
    }

    Ok(pubkeys)
}

async fn refresh_peer_app_keys_snapshots(
    storage: &Storage,
    session_manager: &SessionManager,
    client: &nostr_sdk::Client,
    relays: &[String],
    my_owner_pubkey_hex: &str,
    chat_id: Option<&str>,
) -> Result<()> {
    let chats = if let Some(id) = chat_id {
        vec![storage
            .get_chat(id)?
            .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?]
    } else {
        storage.list_chats()?
    };

    let mut seen_owners = std::collections::HashSet::new();
    for chat in chats {
        if chat.their_pubkey == my_owner_pubkey_hex
            || !seen_owners.insert(chat.their_pubkey.clone())
        {
            continue;
        }

        let Ok(owner_pubkey) = nostr::PublicKey::from_hex(&chat.their_pubkey) else {
            continue;
        };

        if let Some(snapshot) = fetch_latest_app_keys_snapshot(client, relays, owner_pubkey).await?
        {
            let sibling_devices: Vec<nostr::PublicKey> = snapshot
                .app_keys
                .get_all_devices()
                .into_iter()
                .map(|device| device.identity_pubkey)
                .filter(|device_pubkey| *device_pubkey != owner_pubkey)
                .collect();
            session_manager.ingest_app_keys_snapshot(
                owner_pubkey,
                snapshot.app_keys,
                snapshot.created_at,
            );

            if sibling_devices.is_empty() {
                continue;
            }

            let invite_events = fetch_events_best_effort(
                client,
                relays,
                nostr_sdk::Filter::new()
                    .kind(nostr::Kind::Custom(
                        nostr_double_ratchet::INVITE_EVENT_KIND as u16,
                    ))
                    .authors(sibling_devices.clone())
                    .limit(50),
                std::time::Duration::from_secs(3),
            )
            .await?;

            for event in invite_events {
                if !sibling_devices.contains(&event.pubkey) {
                    continue;
                }

                let expected_d = format!("double-ratchet/invites/{}", event.pubkey.to_hex());
                let matches_device_invite = event.tags.iter().any(|tag| {
                    let parts = tag.as_slice();
                    parts.first().map(|value| value.as_str()) == Some("d")
                        && parts.get(1).map(|value| value.as_str()) == Some(expected_d.as_str())
                });
                if matches_device_invite {
                    session_manager.process_received_event(event);
                }
            }
        }
    }

    Ok(())
}

async fn refresh_pending_invite_response_app_keys(
    session_manager: &SessionManager,
    client: &nostr_sdk::Client,
    relays: &[String],
) -> Result<()> {
    for owner_pubkey in session_manager.pending_invite_response_owner_pubkeys() {
        if let Some(snapshot) = fetch_latest_app_keys_snapshot(client, relays, owner_pubkey).await?
        {
            session_manager.ingest_app_keys_snapshot(
                owner_pubkey,
                snapshot.app_keys,
                snapshot.created_at,
            );
        }
    }

    Ok(())
}

async fn backfill_recent_pairwise_session_messages(
    session_manager: &SessionManager,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
    pubkeys: &std::collections::HashSet<String>,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
    owner_pubkey_hex: &str,
) -> Result<()> {
    let author_pubkeys: Vec<nostr::PublicKey> = pubkeys
        .iter()
        .filter_map(|hex| nostr::PublicKey::from_hex(hex).ok())
        .collect();
    if author_pubkeys.is_empty() {
        return Ok(());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let filter = nostr_sdk::Filter::new()
        .kind(nostr::Kind::Custom(
            nostr_double_ratchet::MESSAGE_EVENT_KIND as u16,
        ))
        .authors(author_pubkeys)
        .since(nostr::Timestamp::from(now.saturating_sub(3600)));
    let mut events =
        fetch_events_best_effort(client, relays, filter, std::time::Duration::from_secs(3)).await?;
    events.sort_by_key(|event| (event.created_at.as_u64(), event.id.to_hex()));

    let mut handled_any = false;
    for event in events {
        let current_event_id = event.id.to_hex();
        if seen_event_ids.contains(&current_event_id) {
            continue;
        }
        seen_event_ids.insert(current_event_id.clone());
        seen_event_ids_order.push_back(current_event_id.clone());
        if seen_event_ids_order.len() > MAX_SEEN_EVENT_IDS {
            if let Some(old) = seen_event_ids_order.pop_front() {
                seen_event_ids.remove(&old);
            }
        }

        let session_manager_result = process_session_manager_event(
            &event,
            runtime,
            client,
            config,
            storage,
            output,
            subscribed_manager_filters,
        )
        .await?;
        if session_manager_result.handled_any && !session_manager_result.current_event_group_routed
        {
            handled_any = true;
        }
        if session_manager_result.handled_current {
            continue;
        }

        let is_pairwise_session_message =
            event.kind.as_u16() as u32 == nostr_double_ratchet::MESSAGE_EVENT_KIND
                && event.tags.iter().any(|tag| {
                    tag.as_slice().first().map(|value| value.as_str()) == Some("header")
                });
        if is_pairwise_session_message && !session_manager_result.current_event_group_routed {
            queue_pending_session_manager_message_event(pending_events, pending_order, &event);
        }
    }

    if handled_any {
        sync_chats_from_session_manager(storage, session_manager, owner_pubkey_hex)?;
    }
    retry_pending_session_manager_message_events(
        pending_events,
        pending_order,
        session_manager,
        runtime,
        client,
        config,
        storage,
        output,
        subscribed_manager_filters,
        owner_pubkey_hex,
    )
    .await?;

    Ok(())
}

struct SessionManagerDecrypted {
    sender: nostr::PublicKey,
    sender_device: Option<nostr::PublicKey>,
    content: String,
    event_id: Option<String>,
}

struct SessionManagerProcessingResult {
    handled_any: bool,
    handled_current: bool,
    current_event_group_routed: bool,
    session_group_decrypts: Vec<SessionGroupDecrypt>,
}

type SessionGroupDecrypt = (
    nostr::PublicKey,
    Option<nostr::PublicKey>,
    String,
    Option<String>,
    u64,
);

fn extract_group_id_tag(decrypted_event: &serde_json::Value) -> Option<String> {
    decrypted_event["tags"].as_array().and_then(|tags| {
        tags.iter().find_map(|t| {
            let arr = t.as_array()?;
            if arr.first()?.as_str()? == "l" {
                arr.get(1)?.as_str().map(String::from)
            } else {
                None
            }
        })
    })
}

fn extract_peer_from_p_tag(decrypted_event: &serde_json::Value) -> Option<String> {
    let tags = decrypted_event["tags"].as_array()?;
    for t in tags {
        let arr = t.as_array()?;
        if arr.first()?.as_str()? != "p" {
            continue;
        }
        let pk_hex = arr.get(1)?.as_str()?;
        if let Ok(pk) = nostr::PublicKey::from_hex(pk_hex) {
            return Some(pk.to_hex());
        }
    }
    None
}

fn resolve_chat_for_one_to_one_decrypted(
    sender_owner_hex: &str,
    decrypted_event: &serde_json::Value,
    my_owner_pubkey_hex: &str,
    storage: &Storage,
) -> Result<Option<StoredChat>> {
    let peer_owner_hex = if sender_owner_hex == my_owner_pubkey_hex {
        match extract_peer_from_p_tag(decrypted_event) {
            Some(pk) if pk != my_owner_pubkey_hex => pk,
            _ => return Ok(None),
        }
    } else {
        sender_owner_hex.to_string()
    };

    storage.get_chat_by_pubkey(&peer_owner_hex)
}

fn fallback_event_id(event_id: Option<&str>, decrypted_event: &serde_json::Value) -> String {
    decrypted_event
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| event_id.map(str::to_string))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn decrypted_content_is_group_routed(content_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content_json)
        .ok()
        .and_then(|event| extract_group_id_tag(&event))
        .is_some()
}

async fn process_session_manager_event(
    event: &nostr::Event,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
) -> Result<SessionManagerProcessingResult> {
    let current_event_id = event.id.to_hex();
    let current_timestamp = event.created_at.as_u64();
    let session_manager = runtime.session_manager();

    session_manager.process_received_event(event.clone());
    let decrypted_events =
        flush_session_manager_events(runtime, client, config, subscribed_manager_filters).await?;
    let session_group_decrypts: Vec<SessionGroupDecrypt> = decrypted_events
        .iter()
        .filter_map(|decrypted| {
            if !decrypted_content_is_group_routed(&decrypted.content) {
                return None;
            }
            let timestamp = if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
                current_timestamp
            } else {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(current_timestamp)
            };
            Some((
                decrypted.sender,
                decrypted.sender_device,
                decrypted.content.clone(),
                decrypted.event_id.clone(),
                timestamp,
            ))
        })
        .collect();
    let current_event_group_routed = session_group_decrypts
        .iter()
        .any(|(_, _, _, event_id, _)| event_id.as_deref() == Some(current_event_id.as_str()));

    let mut handled_any = false;
    let mut handled_current = false;
    for decrypted in decrypted_events {
        let timestamp = if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
            current_timestamp
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        };

        let handled = apply_session_manager_one_to_one_decrypted(
            decrypted.sender,
            &decrypted.content,
            decrypted.event_id.as_deref(),
            timestamp,
            config,
            storage,
            output,
        )?;
        if handled {
            handled_any = true;
            if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
                handled_current = true;
            }
        }
    }

    Ok(SessionManagerProcessingResult {
        handled_any,
        handled_current,
        current_event_group_routed,
        session_group_decrypts,
    })
}

fn queue_pending_session_manager_message_event(
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    event: &nostr::Event,
) {
    let event_id = event.id.to_hex();
    if pending_events.contains_key(&event_id) {
        return;
    }

    pending_events.insert(event_id.clone(), event.clone());
    pending_order.push_back(event_id);

    if pending_order.len() > MAX_PENDING_SESSION_MANAGER_MESSAGE_EVENTS {
        if let Some(oldest_id) = pending_order.pop_front() {
            pending_events.remove(&oldest_id);
        }
    }
}

async fn retry_pending_session_manager_message_events(
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    session_manager: &SessionManager,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    owner_pubkey_hex: &str,
) -> Result<()> {
    if pending_events.is_empty() {
        return Ok(());
    }

    let pending_ids: Vec<String> = pending_order.iter().cloned().collect();
    let mut handled_any = false;
    let mut handled_ids: Vec<String> = Vec::new();

    for pending_id in pending_ids {
        let Some(event) = pending_events.get(&pending_id).cloned() else {
            continue;
        };

        let result = process_session_manager_event(
            &event,
            runtime,
            client,
            config,
            storage,
            output,
            subscribed_manager_filters,
        )
        .await?;

        if result.handled_any && !result.current_event_group_routed {
            handled_any = true;
        }
        if result.handled_current {
            handled_ids.push(pending_id);
        }
    }

    for handled_id in handled_ids {
        pending_events.remove(&handled_id);
    }
    *pending_order = pending_order
        .iter()
        .filter(|event_id| pending_events.contains_key(*event_id))
        .cloned()
        .collect();

    if handled_any {
        sync_chats_from_session_manager(storage, session_manager, owner_pubkey_hex)?;
    }

    Ok(())
}

pub(super) fn apply_session_manager_one_to_one_decrypted(
    sender_owner_pubkey: nostr::PublicKey,
    content_json: &str,
    event_id: Option<&str>,
    timestamp: u64,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<bool> {
    let decrypted_event: serde_json::Value = match serde_json::from_str(content_json) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if extract_group_id_tag(&decrypted_event).is_some() {
        // Group-routed events continue through legacy handling below.
        return Ok(false);
    }

    let my_owner_pubkey_hex = config.owner_public_key_hex()?;
    let sender_owner_hex = sender_owner_pubkey.to_hex();
    let Some(chat) = resolve_chat_for_one_to_one_decrypted(
        &sender_owner_hex,
        &decrypted_event,
        &my_owner_pubkey_hex,
        storage,
    )?
    else {
        return Ok(false);
    };

    let rumor_kind = decrypted_event["kind"]
        .as_u64()
        .unwrap_or(CHAT_MESSAGE_KIND as u64) as u32;
    let content = decrypted_event["content"]
        .as_str()
        .unwrap_or(content_json)
        .to_string();
    let is_outgoing = sender_owner_hex == my_owner_pubkey_hex;
    let from_pubkey_hex = if is_outgoing {
        my_owner_pubkey_hex.clone()
    } else {
        sender_owner_hex.clone()
    };
    let mut updated_chat = chat.clone();

    if rumor_kind == CHAT_SETTINGS_KIND {
        if let Some(ttl) = parse_chat_settings_ttl_seconds(&content) {
            if let Some(stamp) =
                extract_control_stamp_from_value(&decrypted_event, event_id, timestamp)
            {
                let _ = apply_chat_settings(storage, &mut updated_chat, ttl, &stamp)?;
            }
            storage.save_chat(&updated_chat)?;
            output.event(
                "chat_settings",
                serde_json::json!({
                    "chat_id": updated_chat.id,
                    "from_pubkey": from_pubkey_hex,
                    "message_ttl_seconds": ttl,
                    "timestamp": timestamp,
                }),
            );
            return Ok(true);
        }
        return Ok(false);
    }

    if rumor_kind == RECEIPT_KIND {
        let message_ids: Vec<String> = extract_e_tags(&decrypted_event);
        storage.save_chat(&updated_chat)?;
        output.event(
            "receipt",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "type": content,
                "message_ids": message_ids,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    if rumor_kind == REACTION_KIND {
        let message_id = extract_e_tag(&decrypted_event);
        let reaction_id = fallback_event_id(event_id, &decrypted_event);
        let stored = StoredReaction {
            id: reaction_id,
            chat_id: chat.id.clone(),
            message_id: message_id.clone(),
            from_pubkey: from_pubkey_hex.clone(),
            emoji: content.clone(),
            timestamp,
            is_outgoing,
        };
        storage.save_reaction(&stored)?;
        storage.save_chat(&updated_chat)?;
        output.event(
            "reaction",
            IncomingReaction {
                chat_id: updated_chat.id.clone(),
                from_pubkey: from_pubkey_hex,
                message_id,
                emoji: content,
                timestamp,
            },
        );
        return Ok(true);
    }

    if rumor_kind == TYPING_KIND {
        storage.save_chat(&updated_chat)?;
        output.event(
            "typing",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let expires_at = extract_expiration_tag_seconds(&decrypted_event);
    let msg_id = fallback_event_id(event_id, &decrypted_event);

    if !is_expired(expires_at, now_seconds) {
        let stored = StoredMessage {
            id: msg_id.clone(),
            chat_id: chat.id.clone(),
            from_pubkey: from_pubkey_hex.clone(),
            content: content.clone(),
            timestamp,
            is_outgoing,
            expires_at,
        };
        storage.save_message(&stored)?;
        updated_chat.last_message_at = Some(timestamp);
        storage.save_chat(&updated_chat)?;
        output.event(
            "message",
            IncomingMessage {
                chat_id: updated_chat.id.clone(),
                message_id: msg_id,
                from_pubkey: from_pubkey_hex,
                content,
                timestamp,
            },
        );
        return Ok(true);
    }

    storage.save_chat(&updated_chat)?;
    Ok(true)
}

async fn flush_session_manager_events(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
) -> Result<Vec<SessionManagerDecrypted>> {
    let mut decrypted = Vec::new();

    for event in runtime.drain_events() {
        match event {
            SessionManagerEvent::Publish(unsigned) => {
                let sk = nostr::SecretKey::from_slice(&config.private_key_bytes()?)?;
                let keys = nostr::Keys::new(sk);
                let signed = unsigned
                    .sign_with_keys(&keys)
                    .map_err(|e| anyhow::anyhow!("Failed to sign SessionManager event: {}", e))?;
                send_event_or_ignore(client, signed).await?;
            }
            SessionManagerEvent::PublishSigned(signed) => {
                send_event_or_ignore(client, signed).await?;
            }
            SessionManagerEvent::Subscribe { filter_json, .. } => {
                if !subscribed_manager_filters.insert(filter_json.clone()) {
                    continue;
                }
                let filter: nostr_sdk::Filter =
                    serde_json::from_str(&filter_json).with_context(|| {
                        format!("Failed to parse SessionManager filter: {}", filter_json)
                    })?;
                let relays = config.resolved_relays();
                subscribe_filters_best_effort(client, &relays, vec![filter]).await?;
            }
            SessionManagerEvent::Unsubscribe(_) => {
                // nostr-sdk Client API does not expose stable per-sub-id unsubscribe in this path.
            }
            SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                event_id,
            } => {
                decrypted.push(SessionManagerDecrypted {
                    sender,
                    sender_device,
                    content,
                    event_id,
                });
            }
            SessionManagerEvent::ReceivedEvent(_) => {}
        }
    }

    Ok(decrypted)
}

/// Listen for new messages and invite responses
pub async fn listen(
    chat_id: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    use nostr_double_ratchet::{
        GROUP_INVITE_RUMOR_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND, INVITE_RESPONSE_KIND,
        MESSAGE_EVENT_KIND, SHARED_CHANNEL_KIND,
    };
    use nostr_sdk::{Client, Filter, RelayPoolNotification};
    use notify::{Event as NotifyEvent, EventKind, RecursiveMode, Watcher};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let mut config = config.clone();
    let chat_id_owned = chat_id.map(|s| s.to_string());
    let (runtime, my_pubkey, my_pubkey_key, owner_pubkey_hex, owner_pubkey) =
        build_session_manager(&config, storage)?;
    let session_manager = runtime.session_manager();
    // Clean up stale discovery queue entries (older than 24 hours).
    let _ = session_manager.cleanup_discovery_queue(24 * 60 * 60 * 1000);

    import_chats_into_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
    sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
    let our_private_key = config.private_key_bytes()?;

    // Prepare client (don't connect until we have something to subscribe to)
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    let mut connected = false;

    let scope = chat_id
        .map(|id| format!("chat {}", id))
        .unwrap_or_else(|| "all chats".to_string());

    type FilterState = (
        Vec<Filter>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
    );

    // Helper to build SharedChannel map from groups
    let build_channel_map = |storage: &Storage| -> Result<
        HashMap<String, (nostr_double_ratchet::SharedChannel, String)>,
    > {
        let mut channels = HashMap::new();
        for group in storage.list_groups()? {
            if group.data.accepted != Some(true) {
                continue;
            }
            if let Some(ref secret_hex) = group.data.secret {
                if let Ok(secret_bytes) = hex::decode(secret_hex) {
                    if secret_bytes.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&secret_bytes);
                        if let Ok(channel) = nostr_double_ratchet::SharedChannel::new(&arr) {
                            let pk_hex = channel.public_key().to_hex();
                            channels.insert(pk_hex, (channel, group.data.id.clone()));
                        }
                    }
                }
            }
        }
        Ok(channels)
    };

    // sender_event_pubkey -> (group_id, sender_owner_pubkey, sender_device_pubkey)
    type GroupSenderMap = HashMap<String, (String, String, String)>;

    // Helper to build per-sender outer pubkey map from groups
    let build_group_sender_map = |storage: &Storage| -> Result<GroupSenderMap> {
        let mut map = HashMap::new();
        for group in storage.list_groups()? {
            if group.data.accepted != Some(true) {
                continue;
            }

            // Only accept mappings for current members (prevents removed members from continuing to send).
            let members = &group.data.members;
            for sender in storage.list_group_senders(&group.data.id)? {
                if sender.group_id != group.data.id {
                    continue;
                }
                let owner_pubkey_hex = sender
                    .owner_pubkey
                    .as_deref()
                    .unwrap_or(sender.identity_pubkey.as_str());
                if !members.contains(&owner_pubkey_hex.to_string()) {
                    continue;
                }
                if nostr::PublicKey::from_hex(&sender.sender_event_pubkey).is_err() {
                    continue;
                }
                map.insert(
                    sender.sender_event_pubkey.clone(),
                    (
                        group.data.id.clone(),
                        owner_pubkey_hex.to_string(),
                        sender.identity_pubkey.clone(),
                    ),
                );
            }
        }
        Ok(map)
    };

    // Helper to build filters from current state
    // Limit relay backfill to recent events to avoid replaying entire history on restart.
    let since_timestamp = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        nostr::Timestamp::from(now.saturating_sub(3600))
    };

    let build_filters =
        |storage: &Storage,
         chat_id: Option<&str>,
         channel_map: &HashMap<String, (nostr_double_ratchet::SharedChannel, String)>,
         group_sender_map: &GroupSenderMap,
         since: nostr::Timestamp|
         -> Result<FilterState> {
            let pubkeys_to_watch =
                collect_chat_pubkeys_with_session_manager(storage, &session_manager, chat_id)?;
            let subscribed_pubkeys: HashSet<String> =
                pubkeys_to_watch.iter().map(|pk| pk.to_hex()).collect();

            let mut filters = Vec::new();

            if !pubkeys_to_watch.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                        .authors(pubkeys_to_watch)
                        .since(since),
                );
            }

            let group_sender_pubkeys: Vec<nostr::PublicKey> = group_sender_map
                .keys()
                .filter_map(|hex| nostr::PublicKey::from_hex(hex).ok())
                .collect();
            let group_sender_pubkeys_hex: HashSet<String> =
                group_sender_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !group_sender_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                        .authors(group_sender_pubkeys)
                        .since(since),
                );
            }

            let peer_owner_pubkeys: Vec<nostr::PublicKey> = storage
                .list_chats()?
                .into_iter()
                .filter(|chat| {
                    chat_id
                        .map(|selected_chat_id| chat.id == selected_chat_id)
                        .unwrap_or(true)
                })
                .filter_map(|chat| nostr::PublicKey::from_hex(&chat.their_pubkey).ok())
                .collect();
            let peer_app_keys_pubkeys: HashSet<String> =
                peer_owner_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !peer_owner_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(
                            nostr_double_ratchet::APP_KEYS_EVENT_KIND as u16,
                        ))
                        .authors(peer_owner_pubkeys)
                        .since(since),
                );
            }

            let stored_invites = storage.list_invites()?;
            let ephemeral_pubkeys: Vec<nostr::PublicKey> = stored_invites
                .iter()
                .filter_map(|stored| {
                    nostr_double_ratchet::Invite::deserialize(&stored.serialized)
                        .ok()
                        .map(|invite| invite.inviter_ephemeral_public_key)
                })
                .collect();
            let invite_pubkeys: HashSet<String> =
                ephemeral_pubkeys.iter().map(|pk| pk.to_hex()).collect();

            if !ephemeral_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16))
                        .pubkeys(ephemeral_pubkeys), // Invite response events intentionally randomize `created_at` over a wide
                                                     // window for metadata resistance, so applying a recent `since` cutoff can
                                                     // drop valid responses and break session bootstrap.
                );
            }

            // Add SharedChannel filters for accepted groups
            let channel_pubkeys: Vec<nostr::PublicKey> = channel_map
                .values()
                .map(|(ch, _)| ch.public_key())
                .collect();
            let channel_pubkeys_hex: HashSet<String> =
                channel_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !channel_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(SHARED_CHANNEL_KIND as u16))
                        .authors(channel_pubkeys)
                        .since(since),
                );
            }

            Ok((
                filters,
                subscribed_pubkeys,
                invite_pubkeys,
                channel_pubkeys_hex,
                group_sender_pubkeys_hex,
                peer_app_keys_pubkeys,
            ))
        };

    // Build initial filters
    let one_to_many = OneToManyChannel::default();
    let mut channel_map = build_channel_map(storage)?;
    let mut group_sender_map = build_group_sender_map(storage)?;
    let (
        mut filters,
        mut subscribed_pubkeys,
        mut subscribed_invite_pubkeys,
        mut subscribed_channel_pubkeys,
        mut subscribed_group_sender_pubkeys,
        mut subscribed_peer_app_keys_pubkeys,
    ) = build_filters(
        storage,
        chat_id,
        &channel_map,
        &group_sender_map,
        since_timestamp,
    )?;
    let mut last_refresh = Instant::now();
    let mut last_peer_app_keys_refresh =
        Instant::now() - Duration::from_millis(PEER_APP_KEYS_REFRESH_INTERVAL_MS);
    let mut subscribed_manager_filters: HashSet<String> = HashSet::new();

    // Deduplicate events across relays and overlapping subscriptions.
    // Without this, the same event can be processed multiple times (CPU heavy) and
    // can contribute to unbounded notification backlog (memory heavy).
    let mut seen_event_ids: HashSet<String> = HashSet::new();
    let mut seen_event_ids_order: VecDeque<String> = VecDeque::new();

    // If we receive sender-key messages before the sender-key distribution, keep them here and retry
    // when the distribution arrives.
    let mut pending_sender_key_messages: HashMap<(String, String, u32), Vec<nostr::Event>> =
        HashMap::new();

    // If we receive per-sender published group messages before the sender-key distribution, keep
    // them here and retry when the distribution arrives.
    let mut pending_group_sender_events: HashMap<(String, u32), Vec<nostr::Event>> = HashMap::new();
    // If a newly linked device sends its first message before we have processed that device's
    // invite response, keep the outer event here and retry it after session bootstrap lands.
    let mut pending_session_manager_message_events: HashMap<String, nostr::Event> = HashMap::new();
    let mut pending_session_manager_message_event_order: VecDeque<String> = VecDeque::new();

    output.success_message(
        "listen",
        &format!(
            "Listening for messages and invite responses on {}... (Ctrl+C to stop)",
            scope
        ),
    );

    // Create notifications receiver before any subscribe() call to avoid missing
    // backfilled events that relays may send immediately upon subscription.
    let mut notifications = client.notifications();

    // Set up filesystem watcher for invites and chats directories
    let (fs_tx, fs_rx) = mpsc::channel();
    let mut _watcher =
        notify::recommended_watcher(move |res: Result<NotifyEvent, notify::Error>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = fs_tx.send(());
                }
            }
        })?;

    // Watch storage directories
    let invites_dir = storage.data_dir().join("invites");
    let chats_dir = storage.data_dir().join("chats");
    let groups_dir = storage.data_dir().join("groups");
    let group_senders_dir = storage.data_dir().join("group_senders");
    if invites_dir.exists() {
        _watcher.watch(&invites_dir, RecursiveMode::NonRecursive)?;
    }
    if chats_dir.exists() {
        _watcher.watch(&chats_dir, RecursiveMode::NonRecursive)?;
    }
    if groups_dir.exists() {
        _watcher.watch(&groups_dir, RecursiveMode::NonRecursive)?;
    }
    if group_senders_dir.exists() {
        // `group_senders/<group_id>/<identity>.json`
        _watcher.watch(&group_senders_dir, RecursiveMode::Recursive)?;
    }

    // Subscribe only if we have filters
    let mut has_subscription = !filters.is_empty();
    if has_subscription {
        if !connected {
            client.connect().await;
            connected = true;
        }
        subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
        refresh_peer_app_keys_snapshots(
            storage,
            &session_manager,
            &client,
            &relays,
            &owner_pubkey_hex,
            chat_id_owned.as_deref(),
        )
        .await?;
        refresh_pending_invite_response_app_keys(&session_manager, &client, &relays).await?;
        last_peer_app_keys_refresh = Instant::now();
        let _ = flush_session_manager_events(
            &runtime,
            &client,
            &config,
            &mut subscribed_manager_filters,
        )
        .await?;
        sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
        retry_pending_session_manager_message_events(
            &mut pending_session_manager_message_events,
            &mut pending_session_manager_message_event_order,
            &session_manager,
            &runtime,
            &client,
            &config,
            storage,
            output,
            &mut subscribed_manager_filters,
            &owner_pubkey_hex,
        )
        .await?;
        backfill_recent_pairwise_session_messages(
            &session_manager,
            &runtime,
            &client,
            &relays,
            &subscribed_pubkeys,
            &config,
            storage,
            output,
            &mut subscribed_manager_filters,
            &mut pending_session_manager_message_events,
            &mut pending_session_manager_message_event_order,
            &mut seen_event_ids,
            &mut seen_event_ids_order,
            &owner_pubkey_hex,
        )
        .await?;
    }

    // Wait for invites/chats if we have nothing to subscribe to yet.
    // Poll storage even without filesystem events so brand-new dirs/files created by
    // sibling `ndr` processes are discovered quickly.
    while !has_subscription {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Drain fs events to avoid unbounded queue growth.
        while let Ok(()) = fs_rx.try_recv() {}

        import_chats_into_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
        sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
        channel_map = build_channel_map(storage)?;
        group_sender_map = build_group_sender_map(storage)?;
        let (
            new_filters,
            new_pubkeys,
            new_invite_pubkeys,
            new_channel_pubkeys,
            new_group_sender_pubkeys,
            new_peer_app_keys_pubkeys,
        ) = build_filters(
            storage,
            chat_id_owned.as_deref(),
            &channel_map,
            &group_sender_map,
            since_timestamp,
        )?;
        if !new_filters.is_empty() {
            filters = new_filters;
            subscribed_pubkeys = new_pubkeys;
            subscribed_invite_pubkeys = new_invite_pubkeys;
            subscribed_channel_pubkeys = new_channel_pubkeys;
            subscribed_group_sender_pubkeys = new_group_sender_pubkeys;
            subscribed_peer_app_keys_pubkeys = new_peer_app_keys_pubkeys;
            if !connected {
                client.connect().await;
                connected = true;
            }
            subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
            refresh_peer_app_keys_snapshots(
                storage,
                &session_manager,
                &client,
                &relays,
                &owner_pubkey_hex,
                chat_id_owned.as_deref(),
            )
            .await?;
            refresh_pending_invite_response_app_keys(&session_manager, &client, &relays).await?;
            last_peer_app_keys_refresh = Instant::now();
            let _ = flush_session_manager_events(
                &runtime,
                &client,
                &config,
                &mut subscribed_manager_filters,
            )
            .await?;
            retry_pending_session_manager_message_events(
                &mut pending_session_manager_message_events,
                &mut pending_session_manager_message_event_order,
                &session_manager,
                &runtime,
                &client,
                &config,
                storage,
                output,
                &mut subscribed_manager_filters,
                &owner_pubkey_hex,
            )
            .await?;
            backfill_recent_pairwise_session_messages(
                &session_manager,
                &runtime,
                &client,
                &relays,
                &subscribed_pubkeys,
                &config,
                storage,
                output,
                &mut subscribed_manager_filters,
                &mut pending_session_manager_message_events,
                &mut pending_session_manager_message_event_order,
                &mut seen_event_ids,
                &mut seen_event_ids_order,
                &owner_pubkey_hex,
            )
            .await?;
            has_subscription = true;
        }
    }

    // Handle incoming events - only start after we have a subscription
    loop {
        // Check for filesystem changes (new invites/chats created by other processes)
        let mut should_refresh = false;
        while let Ok(()) = fs_rx.try_recv() {
            should_refresh = true;
        }
        if should_refresh || last_refresh.elapsed() >= Duration::from_millis(100) {
            import_chats_into_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
            sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
            if connected {
                if last_peer_app_keys_refresh.elapsed()
                    >= Duration::from_millis(PEER_APP_KEYS_REFRESH_INTERVAL_MS)
                {
                    refresh_peer_app_keys_snapshots(
                        storage,
                        &session_manager,
                        &client,
                        &relays,
                        &owner_pubkey_hex,
                        chat_id_owned.as_deref(),
                    )
                    .await?;
                    refresh_pending_invite_response_app_keys(&session_manager, &client, &relays)
                        .await?;
                    last_peer_app_keys_refresh = Instant::now();
                }
                let _ = flush_session_manager_events(
                    &runtime,
                    &client,
                    &config,
                    &mut subscribed_manager_filters,
                )
                .await?;
                retry_pending_session_manager_message_events(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &session_manager,
                    &runtime,
                    &client,
                    &config,
                    storage,
                    output,
                    &mut subscribed_manager_filters,
                    &owner_pubkey_hex,
                )
                .await?;
            }
            channel_map = build_channel_map(storage)?;
            group_sender_map = build_group_sender_map(storage)?;
            let (
                new_filters,
                new_pubkeys,
                new_invite_pubkeys,
                new_channel_pubkeys,
                new_group_sender_pubkeys,
                new_peer_app_keys_pubkeys,
            ) = build_filters(
                storage,
                chat_id_owned.as_deref(),
                &channel_map,
                &group_sender_map,
                since_timestamp,
            )?;
            if !new_filters.is_empty()
                && (new_filters.len() != filters.len()
                    || new_pubkeys != subscribed_pubkeys
                    || new_invite_pubkeys != subscribed_invite_pubkeys
                    || new_channel_pubkeys != subscribed_channel_pubkeys
                    || new_group_sender_pubkeys != subscribed_group_sender_pubkeys
                    || new_peer_app_keys_pubkeys != subscribed_peer_app_keys_pubkeys)
            {
                filters = new_filters;
                subscribed_pubkeys = new_pubkeys;
                subscribed_invite_pubkeys = new_invite_pubkeys;
                subscribed_channel_pubkeys = new_channel_pubkeys;
                subscribed_group_sender_pubkeys = new_group_sender_pubkeys;
                subscribed_peer_app_keys_pubkeys = new_peer_app_keys_pubkeys;
                subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
                backfill_recent_pairwise_session_messages(
                    &session_manager,
                    &runtime,
                    &client,
                    &relays,
                    &subscribed_pubkeys,
                    &config,
                    storage,
                    output,
                    &mut subscribed_manager_filters,
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                    &owner_pubkey_hex,
                )
                .await?;
            }
            last_refresh = Instant::now();
        }

        // Wait for relay notification with timeout to allow fs check
        let notification = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            notifications.recv(),
        )
        .await;

        let notification = match notification {
            Ok(Ok(n)) => n,
            Ok(Err(_)) => break, // Channel closed
            Err(_) => continue,  // Timeout, loop to check fs
        };

        if let RelayPoolNotification::Event { event, .. } = notification {
            let current_event_id = event.id.to_hex();
            if seen_event_ids.contains(&current_event_id) {
                continue;
            }
            seen_event_ids.insert(current_event_id.clone());
            seen_event_ids_order.push_back(current_event_id.clone());
            if seen_event_ids_order.len() > MAX_SEEN_EVENT_IDS {
                if let Some(old) = seen_event_ids_order.pop_front() {
                    seen_event_ids.remove(&old);
                }
            }
            let session_manager_result = process_session_manager_event(
                &event,
                &runtime,
                &client,
                &config,
                storage,
                output,
                &mut subscribed_manager_filters,
            )
            .await?;
            if event.kind.as_u16() as u32 == INVITE_RESPONSE_KIND {
                refresh_pending_invite_response_app_keys(&session_manager, &client, &relays)
                    .await?;
                let _ = flush_session_manager_events(
                    &runtime,
                    &client,
                    &config,
                    &mut subscribed_manager_filters,
                )
                .await?;
                sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
                retry_pending_session_manager_message_events(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &session_manager,
                    &runtime,
                    &client,
                    &config,
                    storage,
                    output,
                    &mut subscribed_manager_filters,
                    &owner_pubkey_hex,
                )
                .await?;
            }
            let session_group_decrypts = session_manager_result.session_group_decrypts;
            let current_event_group_routed = session_manager_result.current_event_group_routed;

            if session_manager_result.handled_any && !current_event_group_routed {
                sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
            }

            if session_manager_result.handled_current {
                continue;
            }

            let is_pairwise_session_message = event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND
                && event.tags.iter().any(|tag| {
                    tag.as_slice().first().map(|value| value.as_str()) == Some("header")
                });
            if is_pairwise_session_message && !current_event_group_routed {
                queue_pending_session_manager_message_event(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &event,
                );
            }

            let event_kind = event.kind.as_u16() as u32;
            let mut used_group_routed_fallback = false;

            // Handle invite responses
            if event_kind == INVITE_RESPONSE_KIND {
                for stored_invite in storage.list_invites()? {
                    let invite = match nostr_double_ratchet::Invite::deserialize(
                        &stored_invite.serialized,
                    ) {
                        Ok(i) => i,
                        Err(_) => continue,
                    };

                    match invite.process_invite_response(&event, our_private_key) {
                        Ok(Some(response)) => {
                            let resolved_owner = response.resolved_owner_pubkey();
                            let their_pubkey = match resolve_verified_owner_pubkey(
                                Some(&client),
                                &relays,
                                &response,
                            )
                            .await
                            {
                                Ok(Some(pubkey)) => pubkey,
                                Ok(None) => {
                                    output.event(
                                        "invite_rejected",
                                        serde_json::json!({
                                            "invite_id": stored_invite.id,
                                            "owner_pubkey": resolved_owner.to_hex(),
                                            "device_pubkey": response.invitee_identity.to_hex(),
                                            "reason": "unverified_owner_claim",
                                        }),
                                    );
                                    continue;
                                }
                                Err(err) => {
                                    output.event(
                                        "invite_rejected",
                                        serde_json::json!({
                                            "invite_id": stored_invite.id,
                                            "owner_pubkey": resolved_owner.to_hex(),
                                            "device_pubkey": response.invitee_identity.to_hex(),
                                            "reason": format!("owner_verification_error: {}", err),
                                        }),
                                    );
                                    continue;
                                }
                            };

                            if invite.purpose.as_deref() == Some("link") {
                                let owner_pubkey_hex = their_pubkey.to_hex();

                                config.set_linked_owner(&owner_pubkey_hex)?;
                                storage.delete_invite(&stored_invite.id)?;

                                output.event(
                                    "link_accepted",
                                    serde_json::json!({
                                        "invite_id": stored_invite.id,
                                        "owner_pubkey": owner_pubkey_hex,
                                        "device_pubkey": invite.inviter.to_hex(),
                                    }),
                                );
                                continue;
                            }

                            if their_pubkey != response.invitee_identity {
                                if let Some(snapshot) =
                                    fetch_latest_app_keys_snapshot(&client, &relays, their_pubkey)
                                        .await?
                                {
                                    session_manager.ingest_app_keys_snapshot(
                                        their_pubkey,
                                        snapshot.app_keys,
                                        snapshot.created_at,
                                    );
                                }
                            }

                            let peer_device_id = response
                                .device_id
                                .clone()
                                .or_else(|| Some(response.invitee_identity.to_hex()));
                            let session = response.session;
                            let response_state = session.state.clone();
                            let their_pubkey_hex = hex::encode(their_pubkey.to_bytes());
                            let mut import_state = response_state.clone();

                            let chat = if let Some(mut existing_chat) =
                                storage.get_chat_by_pubkey(&their_pubkey_hex)?
                            {
                                let selected_state = match serde_json::from_str::<
                                    nostr_double_ratchet::SessionState,
                                >(
                                    &existing_chat.session_state
                                ) {
                                    Ok(existing_state) => {
                                        let existing_can_send = Session::new(
                                            existing_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        let response_can_send = Session::new(
                                            response_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        if existing_can_send && !response_can_send {
                                            existing_state
                                        } else {
                                            response_state.clone()
                                        }
                                    }
                                    Err(_) => response_state.clone(),
                                };

                                import_state = selected_state.clone();
                                if peer_device_id.is_some() {
                                    existing_chat.device_id = peer_device_id.clone();
                                }
                                existing_chat.session_state =
                                    serde_json::to_string(&selected_state)?;
                                storage.save_chat(&existing_chat)?;
                                existing_chat
                            } else {
                                let new_chat = crate::storage::StoredChat {
                                    id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                                    their_pubkey: their_pubkey_hex.clone(),
                                    device_id: peer_device_id,
                                    created_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs(),
                                    last_message_at: None,
                                    session_state: serde_json::to_string(&response_state)?,
                                    message_ttl_seconds: None,
                                };
                                storage.save_chat(&new_chat)?;
                                new_chat
                            };
                            session_manager.import_session_state(
                                their_pubkey,
                                chat.device_id.clone(),
                                import_state,
                            )?;
                            session_manager.setup_user(their_pubkey);
                            let _ = flush_session_manager_events(
                                &runtime,
                                &client,
                                &config,
                                &mut subscribed_manager_filters,
                            )
                            .await?;
                            sync_chats_from_session_manager(
                                storage,
                                &session_manager,
                                &owner_pubkey_hex,
                            )?;
                            retry_pending_session_manager_message_events(
                                &mut pending_session_manager_message_events,
                                &mut pending_session_manager_message_event_order,
                                &session_manager,
                                &runtime,
                                &client,
                                &config,
                                storage,
                                output,
                                &mut subscribed_manager_filters,
                                &owner_pubkey_hex,
                            )
                            .await?;
                            storage.delete_invite(&stored_invite.id)?;

                            // Update subscription for new chat's ephemeral keys
                            let new_pubkeys = collect_chat_pubkeys_with_session_manager(
                                storage,
                                &session_manager,
                                chat_id_owned.as_deref(),
                            )?;
                            if !new_pubkeys.is_empty() {
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone())
                                    .since(since_timestamp);
                                subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                    .await?;
                                subscribed_pubkeys =
                                    new_pubkeys.iter().map(|pk| pk.to_hex()).collect();
                            }

                            output.event(
                                "session_created",
                                serde_json::json!({
                                    "invite_id": stored_invite.id,
                                    "chat_id": chat.id,
                                    "their_pubkey": their_pubkey_hex,
                                }),
                            );

                            break;
                        }
                        Ok(None) => continue,
                        Err(_) => continue,
                    }
                }
                continue;
            }

            // Shared-channel is only used for signed group-invite announcements that bootstrap
            // 1:1 sessions between members without an existing channel.
            if event_kind == SHARED_CHANNEL_KIND {
                let sender_hex = event.pubkey.to_hex();
                if let Some((channel, group_id)) = channel_map.get(&sender_hex) {
                    let Ok(inner_json) = channel.decrypt_event(&event) else {
                        continue;
                    };
                    let Ok(inner_event) = (|| {
                        let ev: nostr::Event = nostr::JsonUtil::from_json(inner_json.as_str())?;
                        Ok::<nostr::Event, nostr::event::Error>(ev)
                    })() else {
                        // Reject unsigned/non-event payloads.
                        continue;
                    };

                    // Inner payload must be an authentic signed event.
                    if inner_event.verify().is_err() {
                        continue;
                    }
                    if inner_event.kind.as_u16() as u32 != GROUP_INVITE_RUMOR_KIND {
                        continue;
                    }

                    // Skip our own device announcements.
                    let signer_device_pubkey = inner_event.pubkey;
                    if signer_device_pubkey.to_hex() == my_pubkey {
                        continue;
                    }

                    // Bind inner payload to the decrypted shared-channel group.
                    let tag_group_id = inner_event.tags.iter().find_map(|t| {
                        let v = t.clone().to_vec();
                        if v.first().map(|s| s.as_str()) == Some("l") {
                            v.get(1).cloned()
                        } else {
                            None
                        }
                    });
                    if let Some(tag_gid) = tag_group_id {
                        if tag_gid != *group_id {
                            continue;
                        }
                    }

                    let invite_data: serde_json::Value =
                        match serde_json::from_str(&inner_event.content) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                    let invite_url = invite_data
                        .get("inviteUrl")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if invite_url.is_empty() {
                        continue;
                    }
                    if invite_data.get("groupId").and_then(|v| v.as_str())
                        != Some(group_id.as_str())
                    {
                        continue;
                    }

                    let signer_device_hex = signer_device_pubkey.to_hex();
                    let claimed_owner_hex = invite_data
                        .get("ownerPubkey")
                        .and_then(|v| v.as_str())
                        .unwrap_or(signer_device_hex.as_str())
                        .to_string();
                    let Ok(claimed_owner_pubkey) = nostr::PublicKey::from_hex(&claimed_owner_hex)
                    else {
                        continue;
                    };

                    let Some(group) = storage.get_group(group_id)? else {
                        continue;
                    };
                    if !group.data.members.contains(&claimed_owner_hex) {
                        continue;
                    }

                    // The signer must either be the owner itself or an authorized delegate device.
                    let signer_owner_pubkey = if claimed_owner_pubkey == signer_device_pubkey {
                        claimed_owner_pubkey
                    } else {
                        let app_keys =
                            fetch_latest_app_keys(&client, &relays, claimed_owner_pubkey).await?;
                        let authorized = app_keys
                            .as_ref()
                            .and_then(|keys| keys.get_device(&signer_device_pubkey))
                            .is_some();
                        if !authorized {
                            continue;
                        }
                        claimed_owner_pubkey
                    };
                    let signer_owner_hex = signer_owner_pubkey.to_hex();

                    // Skip if it's our own owner identity.
                    if signer_owner_hex == owner_pubkey_hex {
                        continue;
                    }

                    // Deterministic tie-breaker:
                    // when two members publish group invites concurrently, only
                    // the lexicographically-smaller owner pubkey auto-accepts.
                    if owner_pubkey_hex >= signer_owner_hex {
                        continue;
                    }

                    // Parse invite and bind it to the signed announcer identity.
                    let Ok(invite) = nostr_double_ratchet::Invite::from_url(invite_url) else {
                        continue;
                    };
                    let invite_owner_pubkey = invite.owner_public_key.unwrap_or(invite.inviter);
                    if invite_owner_pubkey != signer_owner_pubkey {
                        continue;
                    }
                    if invite.inviter != signer_device_pubkey {
                        continue;
                    }

                    // If we already have a *sendable* session, skip auto-accept.
                    // For concurrent invites, an existing inviter-side session can be
                    // present but still non-sendable; in that case we must still
                    // accept to avoid deadlock.
                    let peer_chats: Vec<_> = storage
                        .list_chats()?
                        .into_iter()
                        .filter(|chat| chat.their_pubkey == signer_owner_hex)
                        .collect();
                    if peer_chats.iter().any(|existing_chat| {
                        let existing_can_send =
                            serde_json::from_str::<nostr_double_ratchet::SessionState>(
                                &existing_chat.session_state,
                            )
                            .map(|state| Session::new(state, existing_chat.id.clone()).can_send())
                            .unwrap_or(false);
                        existing_can_send || existing_chat.last_message_at.is_some()
                    }) {
                        continue;
                    }

                    if let Ok((mut accept_session, response_event)) = invite.accept_with_owner(
                        my_pubkey_key,
                        our_private_key,
                        None,
                        Some(owner_pubkey),
                    ) {
                        // Merge into existing chat if present; otherwise create one.
                        let mut chat = if let Some(mut existing_chat) =
                            storage.get_chat_by_pubkey(&signer_owner_hex)?
                        {
                            let selected_state =
                                match serde_json::from_str::<nostr_double_ratchet::SessionState>(
                                    &existing_chat.session_state,
                                ) {
                                    Ok(existing_state) => {
                                        let existing_can_send = Session::new(
                                            existing_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        let accepted_can_send = Session::new(
                                            accept_session.state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        if existing_can_send && !accepted_can_send {
                                            existing_state
                                        } else {
                                            accept_session.state.clone()
                                        }
                                    }
                                    Err(_) => accept_session.state.clone(),
                                };

                            accept_session.state = selected_state.clone();
                            existing_chat.device_id = Some(invite.inviter.to_hex());
                            existing_chat.session_state = serde_json::to_string(&selected_state)?;
                            storage.save_chat(&existing_chat)?;
                            existing_chat
                        } else {
                            let new_chat = crate::storage::StoredChat {
                                id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                                their_pubkey: signer_owner_hex.clone(),
                                device_id: Some(invite.inviter.to_hex()),
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)?
                                    .as_secs(),
                                last_message_at: None,
                                session_state: serde_json::to_string(&accept_session.state)?,
                                message_ttl_seconds: None,
                            };
                            storage.save_chat(&new_chat)?;
                            new_chat
                        };

                        // Publish the response event.
                        send_event_or_ignore(&client, response_event).await?;

                        // Kick off the session by sending a lightweight typing event.
                        // The inviter is a non-initiator (see Invite::process_invite_response),
                        // so they must receive at least one message before they can send.
                        if let Ok(typing_event) = accept_session.send_typing() {
                            // Persist ratcheted state before network I/O.
                            chat.session_state = serde_json::to_string(&accept_session.state)?;
                            storage.save_chat(&chat)?;

                            // Best-effort; group functionality still works if this fails.
                            let _ = client.send_event(typing_event).await;
                        }

                        // Update subscription for new chat's keys.
                        let new_pubkeys = collect_chat_pubkeys_with_session_manager(
                            storage,
                            &session_manager,
                            chat_id_owned.as_deref(),
                        )?;
                        if !new_pubkeys.is_empty() {
                            let new_filter = Filter::new()
                                .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                .authors(new_pubkeys.clone())
                                .since(since_timestamp);
                            subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                .await?;
                            subscribed_pubkeys = new_pubkeys.iter().map(|pk| pk.to_hex()).collect();
                        }

                        output.event(
                            "group_invite_accepted",
                            serde_json::json!({
                                "group_id": group_id,
                                "member_pubkey": signer_owner_hex,
                                "chat_id": chat.id,
                                "sender_device_pubkey": signer_device_hex,
                            }),
                        );
                    }
                }
                continue;
            }

            // Handle messages
            if event_kind == MESSAGE_EVENT_KIND {
                // === Per-sender published group message ===
                let sender_event_pubkey_hex = event.pubkey.to_hex();
                if let Some((group_id, sender_owner_pubkey_hex, sender_device_pubkey_hex)) =
                    group_sender_map.get(&sender_event_pubkey_hex).cloned()
                {
                    if event.verify().is_err() {
                        continue;
                    }

                    let parsed = match one_to_many.parse_outer_content(&event.content) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let key_id = parsed.key_id;
                    let n = parsed.message_number;
                    let ciphertext = parsed.ciphertext;

                    if ciphertext.is_empty() {
                        continue;
                    };

                    let Some(group) = storage.get_group(&group_id)? else {
                        continue;
                    };
                    if group.data.accepted != Some(true) {
                        continue;
                    }
                    if !group.data.members.contains(&sender_owner_pubkey_hex) {
                        continue;
                    }

                    let Some(mut st) = storage.get_group_sender_key_state(
                        &group_id,
                        &sender_device_pubkey_hex,
                        key_id,
                    )?
                    else {
                        pending_group_sender_events
                            .entry((sender_event_pubkey_hex.clone(), key_id))
                            .or_default()
                            .push(*event);
                        continue;
                    };

                    let plaintext_json = match st.decrypt_from_bytes(n, &ciphertext) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    storage.upsert_group_sender_key_state(
                        &group_id,
                        &sender_device_pubkey_hex,
                        &st,
                    )?;

                    let decrypted_event: serde_json::Value =
                        match serde_json::from_str(&plaintext_json) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                    let rumor_kind = decrypted_event["kind"]
                        .as_u64()
                        .unwrap_or(CHAT_MESSAGE_KIND as u64)
                        as u32;
                    let content = decrypted_event["content"]
                        .as_str()
                        .unwrap_or(&plaintext_json)
                        .to_string();

                    let timestamp = event.created_at.as_u64();
                    let now_seconds = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    let expires_at = extract_expiration_tag_seconds(&decrypted_event);

                    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                        if is_expired(expires_at, now_seconds) {
                            continue;
                        }
                        let msg_id = event.id.to_hex();
                        let stored = StoredGroupMessage {
                            id: msg_id.clone(),
                            group_id: group_id.clone(),
                            sender_pubkey: sender_owner_pubkey_hex.clone(),
                            content: content.clone(),
                            timestamp,
                            is_outgoing: false,
                            expires_at,
                        };
                        storage.save_group_message(&stored)?;

                        output.event(
                            "group_message",
                            serde_json::json!({
                                "group_id": group_id,
                                "message_id": msg_id,
                                "sender_pubkey": sender_owner_pubkey_hex,
                                "sender_device_pubkey": sender_device_pubkey_hex,
                                "content": content,
                                "timestamp": timestamp,
                            }),
                        );
                    } else if rumor_kind == REACTION_KIND {
                        let message_id = extract_e_tag(&decrypted_event);
                        output.event(
                            "group_reaction",
                            serde_json::json!({
                                "group_id": group_id,
                                "sender_pubkey": sender_owner_pubkey_hex,
                                "sender_device_pubkey": sender_device_pubkey_hex,
                                "message_id": message_id,
                                "emoji": content,
                                "timestamp": timestamp,
                            }),
                        );
                    } else if rumor_kind == TYPING_KIND {
                        output.event(
                            "group_typing",
                            serde_json::json!({
                                "group_id": group_id,
                                "sender_pubkey": sender_owner_pubkey_hex,
                                "sender_device_pubkey": sender_device_pubkey_hex,
                                "timestamp": timestamp,
                            }),
                        );
                    }

                    continue;
                }

                let maybe_unmapped_group_outer = if event.verify().is_ok() {
                    one_to_many
                        .parse_outer_content(&event.content)
                        .ok()
                        .and_then(|parsed| {
                            (!parsed.ciphertext.is_empty()).then_some((
                                sender_event_pubkey_hex.clone(),
                                parsed.key_id,
                                (*event).clone(),
                            ))
                        })
                } else {
                    None
                };

                let mut decrypted_current_event = false;
                for chat in storage.list_chats()? {
                    let session_state: nostr_double_ratchet::SessionState =
                        match serde_json::from_str(&chat.session_state) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };

                    let mut session = Session::new(session_state, chat.id.clone());

                    match session.receive(&event) {
                        Ok(Some(decrypted_event_json)) => {
                            decrypted_current_event = true;
                            let decrypted_event: serde_json::Value =
                                serde_json::from_str(&decrypted_event_json)?;
                            let rumor_kind = decrypted_event["kind"]
                                .as_u64()
                                .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                as u32;
                            let content = decrypted_event["content"]
                                .as_str()
                                .unwrap_or(&decrypted_event_json)
                                .to_string();

                            let timestamp = event.created_at.as_u64();

                            // Check for group routing tag ["l", group_id]
                            let group_id = decrypted_event["tags"].as_array().and_then(|tags| {
                                tags.iter().find_map(|t| {
                                    let arr = t.as_array()?;
                                    if arr.first()?.as_str()? == "l" {
                                        arr.get(1)?.as_str().map(String::from)
                                    } else {
                                        None
                                    }
                                })
                            });

                            let from_pubkey_hex = chat.their_pubkey.clone();

                            // Always update session state first
                            let mut updated_chat = chat.clone();
                            updated_chat.session_state = serde_json::to_string(&session.state)?;

                            if let Some(ref gid) = group_id {
                                // === Group-routed event ===
                                if rumor_kind == GROUP_METADATA_KIND {
                                    // Group metadata update
                                    if let Some(metadata) =
                                        nostr_double_ratchet::group::parse_group_metadata(&content)
                                    {
                                        let my_device_pubkey = config.public_key()?;
                                        let my_owner_pubkey = config.owner_public_key_hex()?;
                                        let stamp = extract_control_stamp_from_value(
                                            &decrypted_event,
                                            None,
                                            timestamp,
                                        );

                                        if let Some(stamp) = stamp {
                                            match apply_group_metadata(
                                                storage,
                                                gid,
                                                &from_pubkey_hex,
                                                metadata,
                                                stamp,
                                                timestamp * 1000,
                                                &my_owner_pubkey,
                                            )? {
                                                GroupMetadataApplyOutcome::Updated {
                                                    previous_secret,
                                                    group: updated,
                                                } => {
                                                    // If the group's shared-channel secret rotated (e.g. membership changed),
                                                    // force *our* sender key to rotate too so newly-added members can decrypt.
                                                    if updated.secret != previous_secret {
                                                        let _ = storage.delete_group_sender_keys(
                                                            gid,
                                                            &my_device_pubkey,
                                                        )?;
                                                    }

                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "updated",
                                                            "sender_pubkey": from_pubkey_hex,
                                                            "name": updated.name,
                                                            "members": updated.members,
                                                            "admins": updated.admins,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Removed => {
                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "removed",
                                                            "sender_pubkey": from_pubkey_hex,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Created(group_data) => {
                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "created",
                                                            "sender_pubkey": from_pubkey_hex,
                                                            "name": group_data.name,
                                                            "members": group_data.members,
                                                            "admins": group_data.admins,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Ignored
                                                | GroupMetadataApplyOutcome::Rejected => {
                                                    storage.save_chat(&updated_chat)?;
                                                }
                                            }
                                        }
                                    }
                                } else if rumor_kind == GROUP_SENDER_KEY_DISTRIBUTION_KIND {
                                    // Sender-key distribution over the 1:1 Double Ratchet session (forward secrecy).
                                    let dist = match serde_json::from_str::<
                                        nostr_double_ratchet::SenderKeyDistribution,
                                    >(&content)
                                    {
                                        Ok(d) => d,
                                        Err(_) => continue,
                                    };

                                    if dist.group_id != *gid {
                                        continue;
                                    }

                                    let Some(group) = storage.get_group(gid)? else {
                                        continue;
                                    };
                                    if !group.data.members.contains(&from_pubkey_hex) {
                                        continue;
                                    }

                                    let sender_device_pubkey_hex = chat
                                        .device_id
                                        .clone()
                                        .filter(|d| nostr::PublicKey::from_hex(d).is_ok())
                                        .unwrap_or_else(|| from_pubkey_hex.clone());

                                    // Learn/update the sender's per-group outer pubkey mapping.
                                    if let Some(ref sender_event_pubkey) = dist.sender_event_pubkey
                                    {
                                        if let Ok(sender_event_pk) =
                                            nostr::PublicKey::from_hex(sender_event_pubkey)
                                        {
                                            storage.upsert_group_sender(&StoredGroupSender {
                                                group_id: gid.clone(),
                                                identity_pubkey: sender_device_pubkey_hex.clone(),
                                                owner_pubkey: Some(from_pubkey_hex.clone()),
                                                sender_event_pubkey: sender_event_pubkey.clone(),
                                                sender_event_secret_key: None,
                                            })?;

                                            group_sender_map.insert(
                                                sender_event_pubkey.clone(),
                                                (
                                                    gid.clone(),
                                                    from_pubkey_hex.clone(),
                                                    sender_device_pubkey_hex.clone(),
                                                ),
                                            );

                                            let sender_event_hex = sender_event_pk.to_hex();
                                            if !subscribed_group_sender_pubkeys
                                                .contains(&sender_event_hex)
                                            {
                                                let sender_filter = Filter::new()
                                                    .kind(nostr::Kind::Custom(
                                                        MESSAGE_EVENT_KIND as u16,
                                                    ))
                                                    .authors(vec![sender_event_pk]);
                                                subscribe_filters_best_effort(
                                                    &client,
                                                    &relays,
                                                    vec![sender_filter],
                                                )
                                                .await?;
                                                subscribed_group_sender_pubkeys
                                                    .insert(sender_event_hex);
                                            }
                                        }
                                    }

                                    // Don't overwrite an existing progressed state (would break decrypt).
                                    if storage
                                        .get_group_sender_key_state(
                                            gid,
                                            &sender_device_pubkey_hex,
                                            dist.key_id,
                                        )?
                                        .is_none()
                                    {
                                        let state = nostr_double_ratchet::SenderKeyState::new(
                                            dist.key_id,
                                            dist.chain_key,
                                            dist.iteration,
                                        );
                                        storage.upsert_group_sender_key_state(
                                            gid,
                                            &sender_device_pubkey_hex,
                                            &state,
                                        )?;
                                    }

                                    // Retry any pending SharedChannel messages for this (group,sender,key_id).
                                    let pending_key = (
                                        gid.clone(),
                                        sender_device_pubkey_hex.clone(),
                                        dist.key_id,
                                    );
                                    if let Some(mut pending) =
                                        pending_sender_key_messages.remove(&pending_key)
                                    {
                                        // Best-effort: process in message-number order.
                                        pending.sort_by_key(|e| {
                                            e.tags
                                                .iter()
                                                .find_map(|t| {
                                                    let v = t.clone().to_vec();
                                                    if v.first().map(|s| s.as_str()) == Some("n") {
                                                        v.get(1)?.parse::<u64>().ok()
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .unwrap_or(0)
                                        });

                                        for env in pending {
                                            let key_id = dist.key_id;
                                            let n = env
                                                .tags
                                                .iter()
                                                .find_map(|t| {
                                                    let v = t.clone().to_vec();
                                                    if v.first().map(|s| s.as_str()) == Some("n") {
                                                        v.get(1)?.parse::<u32>().ok()
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .unwrap_or(0);

                                            if let Some(mut st) = storage
                                                .get_group_sender_key_state(
                                                    gid,
                                                    &sender_device_pubkey_hex,
                                                    key_id,
                                                )?
                                            {
                                                if let Ok(plaintext_json) =
                                                    st.decrypt(n, &env.content)
                                                {
                                                    storage.upsert_group_sender_key_state(
                                                        gid,
                                                        &sender_device_pubkey_hex,
                                                        &st,
                                                    )?;

                                                    if let Ok(decrypted_event) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            &plaintext_json,
                                                        )
                                                    {
                                                        let rumor_kind = decrypted_event["kind"]
                                                            .as_u64()
                                                            .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                                            as u32;
                                                        let content = decrypted_event["content"]
                                                            .as_str()
                                                            .unwrap_or(&plaintext_json)
                                                            .to_string();

                                                        let timestamp = env.created_at.as_u64();
                                                        let now_seconds =
                                                            std::time::SystemTime::now()
                                                                .duration_since(
                                                                    std::time::UNIX_EPOCH,
                                                                )?
                                                                .as_secs();
                                                        let expires_at =
                                                            extract_expiration_tag_seconds(
                                                                &decrypted_event,
                                                            );

                                                        if rumor_kind == CHAT_MESSAGE_KIND
                                                            || rumor_kind == 14
                                                        {
                                                            if is_expired(expires_at, now_seconds) {
                                                                continue;
                                                            }
                                                            let msg_id = env.id.to_hex();
                                                            let stored = StoredGroupMessage {
                                                                id: msg_id.clone(),
                                                                group_id: gid.clone(),
                                                                sender_pubkey: from_pubkey_hex
                                                                    .clone(),
                                                                content: content.clone(),
                                                                timestamp,
                                                                is_outgoing: false,
                                                                expires_at,
                                                            };
                                                            storage.save_group_message(&stored)?;
                                                            output.event(
                                                                "group_message",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "message_id": msg_id,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "content": content,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        } else if rumor_kind == REACTION_KIND {
                                                            let message_id =
                                                                extract_e_tag(&decrypted_event);
                                                            output.event(
                                                                "group_reaction",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "message_id": message_id,
                                                                    "emoji": content,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        } else if rumor_kind == TYPING_KIND {
                                                            output.event(
                                                                "group_typing",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Retry any pending per-sender published messages for this (sender_event_pubkey,key_id).
                                    if let Some(ref sender_event_pubkey) = dist.sender_event_pubkey
                                    {
                                        let pending_key =
                                            (sender_event_pubkey.clone(), dist.key_id);
                                        if let Some(mut pending) =
                                            pending_group_sender_events.remove(&pending_key)
                                        {
                                            // Best-effort: process in message-number order.
                                            pending.sort_by_key(|e| {
                                                one_to_many
                                                    .parse_outer_content(&e.content)
                                                    .map(|p| p.message_number as u64)
                                                    .unwrap_or(0)
                                            });

                                            for outer in pending {
                                                if outer.verify().is_err() {
                                                    continue;
                                                }

                                                let parsed = match one_to_many
                                                    .parse_outer_content(&outer.content)
                                                {
                                                    Ok(p) => p,
                                                    Err(_) => continue,
                                                };
                                                let key_id = parsed.key_id;
                                                let n = parsed.message_number;
                                                let ciphertext = parsed.ciphertext;

                                                if ciphertext.is_empty() {
                                                    continue;
                                                }
                                                if key_id != dist.key_id {
                                                    continue;
                                                }

                                                if let Some(mut st) = storage
                                                    .get_group_sender_key_state(
                                                        gid,
                                                        &sender_device_pubkey_hex,
                                                        key_id,
                                                    )?
                                                {
                                                    if let Ok(plaintext_json) =
                                                        st.decrypt_from_bytes(n, &ciphertext)
                                                    {
                                                        storage.upsert_group_sender_key_state(
                                                            gid,
                                                            &sender_device_pubkey_hex,
                                                            &st,
                                                        )?;

                                                        if let Ok(decrypted_event) =
                                                            serde_json::from_str::<serde_json::Value>(
                                                                &plaintext_json,
                                                            )
                                                        {
                                                            let rumor_kind = decrypted_event["kind"]
                                                                .as_u64()
                                                                .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                                                as u32;
                                                            let content = decrypted_event
                                                                ["content"]
                                                                .as_str()
                                                                .unwrap_or(&plaintext_json)
                                                                .to_string();

                                                            let timestamp =
                                                                outer.created_at.as_u64();
                                                            let now_seconds =
                                                                std::time::SystemTime::now()
                                                                    .duration_since(
                                                                        std::time::UNIX_EPOCH,
                                                                    )?
                                                                    .as_secs();
                                                            let expires_at =
                                                                extract_expiration_tag_seconds(
                                                                    &decrypted_event,
                                                                );

                                                            if rumor_kind == CHAT_MESSAGE_KIND
                                                                || rumor_kind == 14
                                                            {
                                                                if is_expired(
                                                                    expires_at,
                                                                    now_seconds,
                                                                ) {
                                                                    continue;
                                                                }
                                                                let msg_id = outer.id.to_hex();
                                                                let stored = StoredGroupMessage {
                                                                    id: msg_id.clone(),
                                                                    group_id: gid.clone(),
                                                                    sender_pubkey: from_pubkey_hex
                                                                        .clone(),
                                                                    content: content.clone(),
                                                                    timestamp,
                                                                    is_outgoing: false,
                                                                    expires_at,
                                                                };
                                                                storage
                                                                    .save_group_message(&stored)?;
                                                                output.event(
                                                                    "group_message",
                                                                    serde_json::json!({
                                                                        "group_id": gid,
                                                                        "message_id": msg_id,
                                                                        "sender_pubkey": from_pubkey_hex,
                                                                        "sender_device_pubkey": sender_device_pubkey_hex,
                                                                        "content": content,
                                                                        "timestamp": timestamp,
                                                                    }),
                                                                );
                                                            } else if rumor_kind == REACTION_KIND {
                                                                let message_id =
                                                                    extract_e_tag(&decrypted_event);
                                                                output.event(
                                                                    "group_reaction",
                                                                    serde_json::json!({
                                                                        "group_id": gid,
                                                                        "sender_pubkey": from_pubkey_hex,
                                                                        "sender_device_pubkey": sender_device_pubkey_hex,
                                                                        "message_id": message_id,
                                                                        "emoji": content,
                                                                        "timestamp": timestamp,
                                                                    }),
                                                                );
                                                            } else if rumor_kind == TYPING_KIND {
                                                                output.event(
                                                                    "group_typing",
                                                                    serde_json::json!({
                                                                        "group_id": gid,
                                                                        "sender_pubkey": from_pubkey_hex,
                                                                        "sender_device_pubkey": sender_device_pubkey_hex,
                                                                        "timestamp": timestamp,
                                                                    }),
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_sender_key",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "sender_device_pubkey": sender_device_pubkey_hex,
                                            "key_id": dist.key_id,
                                            "iteration": dist.iteration,
                                            "sender_event_pubkey": dist.sender_event_pubkey,
                                            "transport": "session",
                                        }),
                                    );
                                } else if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                                    // Group chat message
                                    let now_seconds = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs();
                                    let expires_at =
                                        extract_expiration_tag_seconds(&decrypted_event);

                                    let msg_id = event.id.to_hex();
                                    if !is_expired(expires_at, now_seconds) {
                                        let stored = StoredGroupMessage {
                                            id: msg_id.clone(),
                                            group_id: gid.clone(),
                                            sender_pubkey: from_pubkey_hex.clone(),
                                            content: content.clone(),
                                            timestamp,
                                            is_outgoing: false,
                                            expires_at,
                                        };
                                        storage.save_group_message(&stored)?;

                                        output.event(
                                            "group_message",
                                            serde_json::json!({
                                                "group_id": gid,
                                                "message_id": msg_id,
                                                "sender_pubkey": from_pubkey_hex,
                                                "content": content,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    }

                                    storage.save_chat(&updated_chat)?;
                                } else if rumor_kind == REACTION_KIND {
                                    let message_id = extract_e_tag(&decrypted_event);
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_reaction",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "message_id": message_id,
                                            "emoji": content,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else if rumor_kind == TYPING_KIND {
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_typing",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                }
                            } else {
                                // === 1:1 event (no group tag) ===
                                if rumor_kind == CHAT_SETTINGS_KIND {
                                    if let Some(ttl) = parse_chat_settings_ttl_seconds(&content) {
                                        updated_chat.message_ttl_seconds = ttl;
                                        storage.save_chat(&updated_chat)?;
                                        output.event(
                                            "chat_settings",
                                            serde_json::json!({
                                                "chat_id": updated_chat.id,
                                                "from_pubkey": from_pubkey_hex,
                                                "message_ttl_seconds": ttl,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    }
                                } else if rumor_kind == RECEIPT_KIND {
                                    let receipt_type = content.clone();
                                    let message_ids: Vec<String> = extract_e_tags(&decrypted_event);

                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "receipt",
                                        serde_json::json!({
                                            "chat_id": updated_chat.id,
                                            "from_pubkey": from_pubkey_hex,
                                            "type": receipt_type,
                                            "message_ids": message_ids,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else if rumor_kind == REACTION_KIND {
                                    let message_id = extract_e_tag(&decrypted_event);
                                    let reaction_id = event.id.to_hex();
                                    let stored = StoredReaction {
                                        id: reaction_id,
                                        chat_id: chat.id.clone(),
                                        message_id: message_id.clone(),
                                        from_pubkey: from_pubkey_hex.clone(),
                                        emoji: content.clone(),
                                        timestamp,
                                        is_outgoing: false,
                                    };
                                    storage.save_reaction(&stored)?;
                                    storage.save_chat(&updated_chat)?;

                                    output.event(
                                        "reaction",
                                        IncomingReaction {
                                            chat_id: updated_chat.id.clone(),
                                            from_pubkey: from_pubkey_hex,
                                            message_id,
                                            emoji: content,
                                            timestamp,
                                        },
                                    );
                                } else if rumor_kind == TYPING_KIND {
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "typing",
                                        serde_json::json!({
                                            "chat_id": updated_chat.id,
                                            "from_pubkey": from_pubkey_hex,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else {
                                    // Chat message (kind 14 or default)
                                    let now_seconds = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs();
                                    let expires_at =
                                        extract_expiration_tag_seconds(&decrypted_event);

                                    let msg_id = event.id.to_hex();
                                    if !is_expired(expires_at, now_seconds) {
                                        let stored = StoredMessage {
                                            id: msg_id.clone(),
                                            chat_id: chat.id.clone(),
                                            from_pubkey: from_pubkey_hex.clone(),
                                            content: content.clone(),
                                            timestamp,
                                            is_outgoing: false,
                                            expires_at,
                                        };
                                        storage.save_message(&stored)?;
                                        updated_chat.last_message_at = Some(timestamp);

                                        storage.save_chat(&updated_chat)?;
                                        output.event(
                                            "message",
                                            IncomingMessage {
                                                chat_id: updated_chat.id.clone(),
                                                message_id: msg_id,
                                                from_pubkey: from_pubkey_hex,
                                                content,
                                                timestamp,
                                            },
                                        );
                                    }
                                }
                            }

                            storage.save_chat(&updated_chat)?;

                            // KEY FIX: Update subscription after receiving a message
                            // because the ratchet may have rotated ephemeral keys
                            let new_pubkeys = collect_chat_pubkeys_with_session_manager(
                                storage,
                                &session_manager,
                                chat_id_owned.as_deref(),
                            )?;
                            let new_pubkey_set: HashSet<String> =
                                new_pubkeys.iter().map(|pk| pk.to_hex()).collect();

                            if new_pubkey_set != subscribed_pubkeys {
                                // Keys changed, resubscribe
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone())
                                    .since(since_timestamp);
                                subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                    .await?;
                                subscribed_pubkeys = new_pubkey_set;
                            }

                            break;
                        }
                        _ => continue,
                    }
                }

                // If this looks like a per-sender published group outer event but we don't yet
                // have the sender_event_pubkey mapping (i.e. sender key distribution not processed
                // yet), stash it so we can decrypt it once we learn (sender_event_pubkey,key_id).
                if let Some((sender_event_pubkey, key_id, outer_event)) =
                    maybe_unmapped_group_outer.filter(|_| !decrypted_current_event)
                {
                    pending_group_sender_events
                        .entry((sender_event_pubkey, key_id))
                        .or_default()
                        .push(outer_event);
                }

                if !session_group_decrypts.is_empty() {
                    for (
                        sender_owner_pubkey,
                        sender_device_pubkey,
                        content_json,
                        group_event_id,
                        group_timestamp,
                    ) in &session_group_decrypts
                    {
                        if decrypted_current_event
                            && group_event_id.as_deref() == Some(current_event_id.as_str())
                        {
                            continue;
                        }
                        let decrypted_event: serde_json::Value =
                            match serde_json::from_str(content_json) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                        let Some(gid) = extract_group_id_tag(&decrypted_event) else {
                            continue;
                        };
                        let rumor_kind = decrypted_event["kind"]
                            .as_u64()
                            .unwrap_or(CHAT_MESSAGE_KIND as u64)
                            as u32;
                        let content = decrypted_event["content"]
                            .as_str()
                            .unwrap_or(content_json)
                            .to_string();
                        let from_pubkey_hex = sender_owner_pubkey.to_hex();
                        let timestamp = *group_timestamp;

                        if rumor_kind == GROUP_METADATA_KIND {
                            if let Some(metadata) =
                                nostr_double_ratchet::group::parse_group_metadata(&content)
                            {
                                let my_owner_pubkey = config.owner_public_key_hex()?;
                                if let Some(stamp) = extract_control_stamp_from_value(
                                    &decrypted_event,
                                    None,
                                    timestamp,
                                ) {
                                    match apply_group_metadata(
                                        storage,
                                        &gid,
                                        &from_pubkey_hex,
                                        metadata,
                                        stamp,
                                        timestamp * 1000,
                                        &my_owner_pubkey,
                                    )? {
                                        GroupMetadataApplyOutcome::Updated {
                                            group: updated,
                                            ..
                                        } => {
                                            output.event(
                                                "group_metadata",
                                                serde_json::json!({
                                                    "group_id": gid,
                                                    "action": "updated",
                                                    "sender_pubkey": from_pubkey_hex,
                                                    "name": updated.name,
                                                    "members": updated.members,
                                                    "admins": updated.admins,
                                                }),
                                            );
                                        }
                                        GroupMetadataApplyOutcome::Removed => {
                                            output.event(
                                                "group_metadata",
                                                serde_json::json!({
                                                    "group_id": gid,
                                                    "action": "removed",
                                                    "sender_pubkey": from_pubkey_hex,
                                                }),
                                            );
                                        }
                                        GroupMetadataApplyOutcome::Created(group_data) => {
                                            output.event(
                                                "group_metadata",
                                                serde_json::json!({
                                                    "group_id": gid,
                                                    "action": "created",
                                                    "sender_pubkey": from_pubkey_hex,
                                                    "name": group_data.name,
                                                    "members": group_data.members,
                                                    "admins": group_data.admins,
                                                }),
                                            );
                                        }
                                        GroupMetadataApplyOutcome::Ignored
                                        | GroupMetadataApplyOutcome::Rejected => {}
                                    }
                                }
                            }
                            used_group_routed_fallback = true;
                        } else if rumor_kind == GROUP_SENDER_KEY_DISTRIBUTION_KIND {
                            let dist = match serde_json::from_str::<
                                nostr_double_ratchet::SenderKeyDistribution,
                            >(&content)
                            {
                                Ok(d) => d,
                                Err(_) => continue,
                            };
                            if dist.group_id != gid {
                                continue;
                            }

                            let Some(group) = storage.get_group(&gid)? else {
                                continue;
                            };
                            if !group.data.members.contains(&from_pubkey_hex) {
                                continue;
                            }

                            let sender_device_pubkey_hex = sender_device_pubkey
                                .as_ref()
                                .map(|pk| pk.to_hex())
                                .unwrap_or_else(|| from_pubkey_hex.clone());

                            if let Some(ref sender_event_pubkey) = dist.sender_event_pubkey {
                                if let Ok(sender_event_pk) =
                                    nostr::PublicKey::from_hex(sender_event_pubkey)
                                {
                                    storage.upsert_group_sender(&StoredGroupSender {
                                        group_id: gid.clone(),
                                        identity_pubkey: sender_device_pubkey_hex.clone(),
                                        owner_pubkey: Some(from_pubkey_hex.clone()),
                                        sender_event_pubkey: sender_event_pubkey.clone(),
                                        sender_event_secret_key: None,
                                    })?;

                                    group_sender_map.insert(
                                        sender_event_pubkey.clone(),
                                        (
                                            gid.clone(),
                                            from_pubkey_hex.clone(),
                                            sender_device_pubkey_hex.clone(),
                                        ),
                                    );

                                    let sender_event_hex = sender_event_pk.to_hex();
                                    if !subscribed_group_sender_pubkeys.contains(&sender_event_hex)
                                    {
                                        let sender_filter = Filter::new()
                                            .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                            .authors(vec![sender_event_pk]);
                                        subscribe_filters_best_effort(
                                            &client,
                                            &relays,
                                            vec![sender_filter],
                                        )
                                        .await?;
                                        subscribed_group_sender_pubkeys.insert(sender_event_hex);
                                    }
                                }
                            }

                            if storage
                                .get_group_sender_key_state(
                                    &gid,
                                    &sender_device_pubkey_hex,
                                    dist.key_id,
                                )?
                                .is_none()
                            {
                                let state = nostr_double_ratchet::SenderKeyState::new(
                                    dist.key_id,
                                    dist.chain_key,
                                    dist.iteration,
                                );
                                storage.upsert_group_sender_key_state(
                                    &gid,
                                    &sender_device_pubkey_hex,
                                    &state,
                                )?;
                            }

                            if let Some(ref sender_event_pubkey) = dist.sender_event_pubkey {
                                let pending_key = (sender_event_pubkey.clone(), dist.key_id);
                                if let Some(mut pending) =
                                    pending_group_sender_events.remove(&pending_key)
                                {
                                    pending.sort_by_key(|e| {
                                        one_to_many
                                            .parse_outer_content(&e.content)
                                            .map(|p| p.message_number as u64)
                                            .unwrap_or(0)
                                    });

                                    for outer in pending {
                                        if outer.verify().is_err() {
                                            continue;
                                        }
                                        let parsed =
                                            match one_to_many.parse_outer_content(&outer.content) {
                                                Ok(p) => p,
                                                Err(_) => continue,
                                            };
                                        if parsed.key_id != dist.key_id
                                            || parsed.ciphertext.is_empty()
                                        {
                                            continue;
                                        }

                                        if let Some(mut st) = storage.get_group_sender_key_state(
                                            &gid,
                                            &sender_device_pubkey_hex,
                                            parsed.key_id,
                                        )? {
                                            if let Ok(plaintext_json) = st.decrypt_from_bytes(
                                                parsed.message_number,
                                                &parsed.ciphertext,
                                            ) {
                                                storage.upsert_group_sender_key_state(
                                                    &gid,
                                                    &sender_device_pubkey_hex,
                                                    &st,
                                                )?;
                                                if let Ok(inner_event) =
                                                    serde_json::from_str::<serde_json::Value>(
                                                        &plaintext_json,
                                                    )
                                                {
                                                    let inner_kind = inner_event["kind"]
                                                        .as_u64()
                                                        .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                                        as u32;
                                                    let inner_content = inner_event["content"]
                                                        .as_str()
                                                        .unwrap_or(&plaintext_json)
                                                        .to_string();
                                                    if inner_kind == CHAT_MESSAGE_KIND
                                                        || inner_kind == 14
                                                    {
                                                        let now_seconds =
                                                            std::time::SystemTime::now()
                                                                .duration_since(
                                                                    std::time::UNIX_EPOCH,
                                                                )?
                                                                .as_secs();
                                                        let expires_at =
                                                            extract_expiration_tag_seconds(
                                                                &inner_event,
                                                            );
                                                        if is_expired(expires_at, now_seconds) {
                                                            continue;
                                                        }
                                                        let msg_id = outer.id.to_hex();
                                                        storage.save_group_message(
                                                            &StoredGroupMessage {
                                                                id: msg_id.clone(),
                                                                group_id: gid.clone(),
                                                                sender_pubkey: from_pubkey_hex
                                                                    .clone(),
                                                                content: inner_content.clone(),
                                                                timestamp: outer
                                                                    .created_at
                                                                    .as_u64(),
                                                                is_outgoing: false,
                                                                expires_at,
                                                            },
                                                        )?;
                                                        output.event(
                                                            "group_message",
                                                            serde_json::json!({
                                                                "group_id": gid,
                                                                "message_id": msg_id,
                                                                "sender_pubkey": from_pubkey_hex,
                                                                "sender_device_pubkey": sender_device_pubkey_hex,
                                                                "content": inner_content,
                                                                "timestamp": outer.created_at.as_u64(),
                                                            }),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            output.event(
                                "group_sender_key",
                                serde_json::json!({
                                    "group_id": gid,
                                    "sender_pubkey": from_pubkey_hex,
                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                    "key_id": dist.key_id,
                                    "iteration": dist.iteration,
                                    "sender_event_pubkey": dist.sender_event_pubkey,
                                    "transport": "session",
                                }),
                            );
                            used_group_routed_fallback = true;
                        } else if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                            let now_seconds = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)?
                                .as_secs();
                            let expires_at = extract_expiration_tag_seconds(&decrypted_event);
                            if is_expired(expires_at, now_seconds) {
                                continue;
                            }
                            let msg_id =
                                fallback_event_id(group_event_id.as_deref(), &decrypted_event);
                            storage.save_group_message(&StoredGroupMessage {
                                id: msg_id.clone(),
                                group_id: gid.clone(),
                                sender_pubkey: from_pubkey_hex.clone(),
                                content: content.clone(),
                                timestamp,
                                is_outgoing: false,
                                expires_at,
                            })?;
                            output.event(
                                "group_message",
                                serde_json::json!({
                                    "group_id": gid,
                                    "message_id": msg_id,
                                    "sender_pubkey": from_pubkey_hex,
                                    "content": content,
                                    "timestamp": timestamp,
                                }),
                            );
                            used_group_routed_fallback = true;
                        } else if rumor_kind == REACTION_KIND {
                            output.event(
                                "group_reaction",
                                serde_json::json!({
                                    "group_id": gid,
                                    "sender_pubkey": from_pubkey_hex,
                                    "message_id": extract_e_tag(&decrypted_event),
                                    "emoji": content,
                                    "timestamp": timestamp,
                                }),
                            );
                            used_group_routed_fallback = true;
                        } else if rumor_kind == TYPING_KIND {
                            output.event(
                                "group_typing",
                                serde_json::json!({
                                    "group_id": gid,
                                    "sender_pubkey": from_pubkey_hex,
                                    "timestamp": timestamp,
                                }),
                            );
                            used_group_routed_fallback = true;
                        }
                    }
                }
            }

            if used_group_routed_fallback {
                sync_chats_from_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
            } else if current_event_group_routed {
                import_chats_into_session_manager(storage, &session_manager, &owner_pubkey_hex)?;
            }
        }
    }

    Ok(())
}
