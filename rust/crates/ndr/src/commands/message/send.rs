use anyhow::Result;

use nostr::Tag;
use nostr_double_ratchet::{
    FileStorageAdapter, SessionManager, SessionManagerEvent, StorageAdapter, CHAT_MESSAGE_KIND,
    EXPIRATION_TAG, MESSAGE_EVENT_KIND,
};

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
use crate::output::Output;
use crate::storage::{Storage, StoredChat, StoredMessage, StoredReaction};

use super::common::resolve_target_pubkey;
use super::resolve_target;
use super::types::{MessageInfo, MessageList, MessageSent, ReactionInfo};

#[cfg(test)]
use nostr_double_ratchet::Session;

#[cfg(test)]
pub(super) struct PreparedSendMessage {
    pub(super) encrypted_event: nostr::Event,
    pub(super) timestamp: u64,
    pub(super) stored_message: StoredMessage,
}

async fn resolve_or_join_chat(
    target: &str,
    config: &Config,
    storage: &Storage,
) -> Result<StoredChat> {
    match resolve_target(target, storage) {
        Ok(chat) => Ok(chat),
        Err(resolve_err) => {
            let target_pubkey = match resolve_target_pubkey(target, storage) {
                Ok(pubkey) => pubkey,
                Err(_) => return Err(resolve_err),
            };

            match crate::commands::public_invite::join_via_public_invite(
                &target_pubkey,
                config,
                storage,
            )
            .await
            {
                Ok(joined) => Ok(joined.chat),
                Err(err) => Err(anyhow::anyhow!(
                    "Chat not found and no public invite available for {}: {}",
                    target,
                    err
                )),
            }
        }
    }
}

fn build_session_manager(
    config: &Config,
    storage: &Storage,
) -> Result<(
    SessionManager,
    crossbeam_channel::Receiver<SessionManagerEvent>,
    nostr::Keys,
    String,
)> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: std::sync::Arc<dyn StorageAdapter> = std::sync::Arc::new(
        FileStorageAdapter::new(storage.data_dir().join("session_manager"))?,
    );

    let (sm_tx, sm_rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_pubkey,
        our_private_key,
        our_pubkey_hex,
        owner_pubkey,
        sm_tx,
        Some(session_manager_store),
        None,
    );
    manager.init()?;

    let signing_keys = nostr::Keys::new(nostr::SecretKey::from_slice(&our_private_key)?);
    Ok((manager, sm_rx, signing_keys, owner_pubkey_hex))
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

        if let Some(idx) = chats.iter().position(|chat| chat.their_pubkey == owner_hex) {
            let mut changed = false;
            let mut chat = chats[idx].clone();

            let selected_idx = chat
                .device_id
                .as_ref()
                .and_then(|current| owner_sessions.iter().position(|(d, _)| d == current))
                .unwrap_or(0);
            let (selected_device_id, selected_state) = owner_sessions[selected_idx].clone();
            let state_json = serde_json::to_string(&selected_state)?;

            if chat.device_id.as_deref() != Some(selected_device_id.as_str()) {
                chat.device_id = Some(selected_device_id);
                changed = true;
            }
            if chat.session_state != state_json {
                chat.session_state = state_json;
                changed = true;
            }

            if changed {
                storage.save_chat(&chat)?;
                chats[idx] = chat;
            }
            continue;
        }

        let (selected_device_id, selected_state) = owner_sessions[0].clone();
        let chat = StoredChat {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            their_pubkey: owner_hex,
            device_id: Some(selected_device_id),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            last_message_at: None,
            session_state: serde_json::to_string(&selected_state)?,
            message_ttl_seconds: None,
        };
        storage.save_chat(&chat)?;
        chats.push(chat);
    }

    Ok(())
}

async fn flush_session_manager_message_events(
    manager_rx: &crossbeam_channel::Receiver<SessionManagerEvent>,
    signing_keys: &nostr::Keys,
    client: &nostr_sdk::Client,
) -> Result<Vec<nostr::Event>> {
    let mut message_events = Vec::new();

    while let Ok(event) = manager_rx.try_recv() {
        let signed = match event {
            SessionManagerEvent::Publish(unsigned) => unsigned
                .sign_with_keys(signing_keys)
                .map_err(|e| anyhow::anyhow!("Failed to sign SessionManager event: {}", e))?,
            SessionManagerEvent::PublishSigned(signed) => signed,
            SessionManagerEvent::Subscribe { .. }
            | SessionManagerEvent::Unsubscribe(_)
            | SessionManagerEvent::ReceivedEvent(_)
            | SessionManagerEvent::DecryptedMessage { .. } => continue,
        };

        if signed.kind.as_u16() != MESSAGE_EVENT_KIND as u16 {
            continue;
        }
        send_event_or_ignore(client, signed.clone()).await?;
        message_events.push(signed);
    }

    Ok(message_events)
}

#[cfg(test)]
pub(super) async fn prepare_send_message(
    target: &str,
    message: &str,
    reply_to: Option<&str>,
    ttl_seconds: Option<u64>,
    expires_at_seconds: Option<u64>,
    config: &Config,
    storage: &Storage,
) -> Result<PreparedSendMessage> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    if ttl_seconds.is_some() && expires_at_seconds.is_some() {
        anyhow::bail!("Provide either --ttl or --expires-at (not both)");
    }

    let chat = resolve_or_join_chat(target, config, storage).await?;

    let chat_id = chat.id.clone();

    // Load session state
    let session_state: nostr_double_ratchet::SessionState =
        serde_json::from_str(&chat.session_state).map_err(|e| {
            anyhow::anyhow!(
                "Invalid session state: {}. Chat may not be properly initialized.",
                e
            )
        })?;

    let mut session = Session::new(session_state, chat_id.to_string());

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let now_s = now.as_secs();
    let now_ms = now.as_millis() as u64;

    let effective_ttl = ttl_seconds
        .or(chat.message_ttl_seconds)
        .filter(|ttl| *ttl > 0);

    let expires_at =
        expires_at_seconds.or_else(|| effective_ttl.and_then(|ttl| now_s.checked_add(ttl)));

    let recipient_pk = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    // Build the inner rumor as an unsigned event. This matches Iris expectations:
    // - kind 14
    // - includes "p" tag
    // - includes "ms" tag for stable ids
    // - expiration tag is on the inner rumor (encrypted)
    let mut tag_vec: Vec<Vec<String>> = Vec::new();
    tag_vec.push(vec!["p".to_string(), recipient_pk.to_hex()]);
    if let Some(reply_id) = reply_to {
        tag_vec.push(vec!["e".to_string(), reply_id.to_string()]);
    }
    tag_vec.push(vec!["ms".to_string(), now_ms.to_string()]);
    if let Some(exp) = expires_at {
        tag_vec.push(vec![EXPIRATION_TAG.to_string(), exp.to_string()]);
    }

    let mut nostr_tags: Vec<Tag> = Vec::with_capacity(tag_vec.len());
    for t in tag_vec {
        nostr_tags.push(Tag::parse(&t).map_err(|e| anyhow::anyhow!("Invalid tag: {}", e))?);
    }

    let owner_pk = nostr::PublicKey::from_hex(&config.owner_public_key_hex()?)?;
    let unsigned = nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), message)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(owner_pk);

    // Encrypt the message as a kind-1060 outer event.
    let encrypted_event = session
        .send_event(unsigned)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt message: {}", e))?;

    // Use the outer event ID as message ID (for reaction compatibility with iris-chat)
    let msg_id = encrypted_event.id.to_hex();

    let from_pubkey = config.public_key()?;

    let stored_message = StoredMessage {
        id: msg_id.clone(),
        chat_id: chat_id.to_string(),
        from_pubkey,
        content: message.to_string(),
        timestamp: now_s,
        is_outgoing: true,
        expires_at,
    };

    Ok(PreparedSendMessage {
        encrypted_event,
        timestamp: now_s,
        stored_message,
    })
}

/// Send a message
#[allow(clippy::too_many_arguments)]
pub async fn send(
    target: &str,
    message: &str,
    reply_to: Option<&str>,
    ttl_seconds: Option<u64>,
    expires_at_seconds: Option<u64>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }
    if ttl_seconds.is_some() && expires_at_seconds.is_some() {
        anyhow::bail!("Provide either --ttl or --expires-at (not both)");
    }

    let mut chat = resolve_or_join_chat(target, config, storage).await?;

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
    let now_s = now.as_secs();
    let now_ms = now.as_millis() as u64;

    let effective_ttl = ttl_seconds
        .or(chat.message_ttl_seconds)
        .filter(|ttl| *ttl > 0);
    let expires_at =
        expires_at_seconds.or_else(|| effective_ttl.and_then(|ttl| now_s.checked_add(ttl)));

    let recipient_pk = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    // Build inner rumor (kind 14) and let SessionManager fan out encrypted outers.
    let mut tag_vec: Vec<Vec<String>> = vec![vec!["p".to_string(), recipient_pk.to_hex()]];
    if let Some(reply_id) = reply_to {
        tag_vec.push(vec!["e".to_string(), reply_id.to_string()]);
    }
    tag_vec.push(vec!["ms".to_string(), now_ms.to_string()]);
    if let Some(exp) = expires_at {
        tag_vec.push(vec![EXPIRATION_TAG.to_string(), exp.to_string()]);
    }

    let mut nostr_tags: Vec<Tag> = Vec::with_capacity(tag_vec.len());
    for t in tag_vec {
        nostr_tags.push(Tag::parse(&t).map_err(|e| anyhow::anyhow!("Invalid tag: {}", e))?);
    }

    let owner_pk = nostr::PublicKey::from_hex(&config.owner_public_key_hex()?)?;
    let unsigned = nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), message)
        .tags(nostr_tags)
        .custom_created_at(nostr::Timestamp::from(now_s))
        .build(owner_pk);
    let inner_id = unsigned
        .id
        .as_ref()
        .map(|id| id.to_string())
        .unwrap_or_default();

    let (manager, manager_rx, signing_keys, owner_pubkey_hex) =
        build_session_manager(config, storage)?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let event_ids = manager.send_event(recipient_pk, unsigned)?;

    let client = connect_client(config).await?;
    let published_events =
        flush_session_manager_message_events(&manager_rx, &signing_keys, &client).await?;
    sync_chats_from_session_manager(storage, &manager, &owner_pubkey_hex)?;

    if let Some(updated_chat) = storage.get_chat(&chat.id)? {
        chat = updated_chat;
    }
    chat.last_message_at = Some(now_s);
    storage.save_chat(&chat)?;

    let fallback_id = if inner_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        inner_id
    };
    let msg_id = published_events
        .first()
        .map(|evt| evt.id.to_hex())
        .or_else(|| event_ids.first().cloned())
        .unwrap_or(fallback_id);

    let stored_message = StoredMessage {
        id: msg_id.clone(),
        chat_id: chat.id.clone(),
        from_pubkey: config.public_key()?,
        content: message.to_string(),
        timestamp: now_s,
        is_outgoing: true,
        expires_at,
    };
    storage.save_message(&stored_message)?;

    output.success(
        "send",
        MessageSent {
            id: msg_id,
            chat_id: chat.id,
            content: message.to_string(),
            timestamp: now_s,
            event: published_events
                .first()
                .map(nostr::JsonUtil::as_json)
                .unwrap_or_default(),
        },
    );

    Ok(())
}

/// React to a message
pub async fn react(
    target: &str,
    message_id: &str,
    emoji: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let chat = resolve_target(target, storage)?;
    let chat_id = chat.id.clone();
    let recipient_pk = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    let (manager, manager_rx, signing_keys, owner_pubkey_hex) =
        build_session_manager(config, storage)?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let event_ids = manager.send_reaction(
        recipient_pk,
        message_id.to_string(),
        emoji.to_string(),
        None,
    )?;

    let client = connect_client(config).await?;
    let published_events =
        flush_session_manager_message_events(&manager_rx, &signing_keys, &client).await?;
    sync_chats_from_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let reaction_id = published_events
        .first()
        .map(|evt| evt.id.to_hex())
        .or_else(|| event_ids.first().cloned())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    storage.save_reaction(&StoredReaction {
        id: reaction_id.clone(),
        chat_id: chat_id.to_string(),
        message_id: message_id.to_string(),
        from_pubkey: pubkey,
        emoji: emoji.to_string(),
        timestamp,
        is_outgoing: true,
    })?;

    output.success(
        "react",
        serde_json::json!({
            "id": reaction_id,
            "chat_id": chat_id,
            "message_id": message_id,
            "emoji": emoji,
            "timestamp": timestamp,
            "event": published_events
                .first()
                .map(nostr::JsonUtil::as_json)
                .unwrap_or_default(),
        }),
    );

    Ok(())
}

/// Send a delivery/read receipt
pub async fn receipt(
    target: &str,
    receipt_type: &str,
    message_ids: &[&str],
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    if receipt_type != "delivered" && receipt_type != "seen" {
        anyhow::bail!("Receipt type must be 'delivered' or 'seen'");
    }

    if message_ids.is_empty() {
        anyhow::bail!("At least one message ID is required");
    }

    let chat = resolve_target(target, storage)?;
    let chat_id = chat.id.clone();
    let recipient_pk = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    let (manager, manager_rx, signing_keys, owner_pubkey_hex) =
        build_session_manager(config, storage)?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let message_ids_vec = message_ids.iter().map(|s| (*s).to_string()).collect();
    let _event_ids = manager.send_receipt(recipient_pk, receipt_type, message_ids_vec, None)?;

    let client = connect_client(config).await?;
    let _published_events =
        flush_session_manager_message_events(&manager_rx, &signing_keys, &client).await?;
    sync_chats_from_session_manager(storage, &manager, &owner_pubkey_hex)?;

    output.success(
        "receipt",
        serde_json::json!({
            "chat_id": chat_id,
            "type": receipt_type,
            "message_ids": message_ids,
        }),
    );

    Ok(())
}

/// Send a typing indicator
pub async fn typing(
    target: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let chat = resolve_target(target, storage)?;
    let chat_id = chat.id.clone();
    let recipient_pk = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    let (manager, manager_rx, signing_keys, owner_pubkey_hex) =
        build_session_manager(config, storage)?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let _event_ids = manager.send_typing(recipient_pk, None)?;

    let client = connect_client(config).await?;
    let published_events =
        flush_session_manager_message_events(&manager_rx, &signing_keys, &client).await?;
    sync_chats_from_session_manager(storage, &manager, &owner_pubkey_hex)?;

    output.success(
        "typing",
        serde_json::json!({
            "chat_id": chat_id,
            "event": published_events
                .first()
                .map(nostr::JsonUtil::as_json)
                .unwrap_or_default(),
        }),
    );

    Ok(())
}

/// Read messages from a chat
pub async fn read(target: &str, limit: usize, storage: &Storage, output: &Output) -> Result<()> {
    let chat = resolve_target(target, storage)?;
    let chat_id = chat.id.clone();

    // Best-effort purge so disappearing messages actually disappear from local history.
    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let _ = storage.purge_expired_messages(&chat_id, now_seconds);

    let messages = storage.get_messages(&chat_id, limit)?;
    let reactions = storage.get_reactions(&chat_id, limit)?;

    let message_infos: Vec<MessageInfo> = messages
        .into_iter()
        .map(|m| MessageInfo {
            id: m.id,
            from_pubkey: m.from_pubkey,
            content: m.content,
            timestamp: m.timestamp,
            is_outgoing: m.is_outgoing,
        })
        .collect();

    let reaction_infos: Vec<ReactionInfo> = reactions
        .into_iter()
        .map(|r| ReactionInfo {
            id: r.id,
            message_id: r.message_id,
            from_pubkey: r.from_pubkey,
            emoji: r.emoji,
            timestamp: r.timestamp,
            is_outgoing: r.is_outgoing,
        })
        .collect();

    output.success(
        "read",
        MessageList {
            chat_id: chat_id.to_string(),
            messages: message_infos,
            reactions: reaction_infos,
        },
    );

    Ok(())
}
