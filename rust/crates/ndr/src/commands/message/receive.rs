use anyhow::Result;

use nostr_double_ratchet::{
    SessionManagerEvent, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, REACTION_KIND, RECEIPT_KIND,
    TYPING_KIND,
};

use crate::config::Config;
use crate::output::Output;
use crate::state_sync::{apply_chat_settings, extract_control_stamp_from_value};
use crate::storage::{Storage, StoredMessage, StoredReaction};

use super::common::{
    extract_e_tag, extract_e_tags, extract_expiration_tag_seconds, is_expired,
    parse_chat_settings_ttl_seconds,
};
use super::types::{IncomingMessage, IncomingReaction};

/// Receive and decrypt a message from a nostr event.
///
/// This hidden command is used by CLI tests and tooling that already have a raw event. It uses the
/// same persisted SessionManager state as send/listen instead of the historical per-chat state copy.
pub async fn receive(
    event_json: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json)
        .map_err(|e| anyhow::anyhow!("Invalid event JSON: {}", e))?;
    let current_event_id = event.id.to_hex();
    let current_timestamp = event.created_at.as_u64();

    let (runtime, _signing_keys, owner_pubkey_hex) = super::send::build_runtime(config, storage)?;
    let manager = runtime.session_manager();
    manager.process_received_event(event);

    for manager_event in runtime.drain_events() {
        let SessionManagerEvent::DecryptedMessage {
            sender,
            content,
            event_id,
            ..
        } = manager_event
        else {
            continue;
        };

        let timestamp = if event_id.as_deref() == Some(current_event_id.as_str()) {
            current_timestamp
        } else {
            now_seconds()?
        };

        if apply_session_manager_decrypted(
            sender,
            &content,
            event_id.as_deref(),
            timestamp,
            config,
            storage,
            output,
        )? {
            super::send::sync_chats_from_session_manager(storage, manager, &owner_pubkey_hex)?;
            return Ok(());
        }
    }

    anyhow::bail!("Could not decrypt message - no matching session found");
}

fn apply_session_manager_decrypted(
    sender_owner_pubkey: nostr::PublicKey,
    content_json: &str,
    event_id: Option<&str>,
    timestamp: u64,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<bool> {
    let decrypted_event: serde_json::Value = serde_json::from_str(content_json)
        .map_err(|e| anyhow::anyhow!("Failed to parse decrypted event: {}", e))?;

    if extract_group_id_tag(&decrypted_event).is_some() {
        return Ok(false);
    }

    let my_owner_pubkey_hex = config.owner_public_key_hex()?;
    let sender_owner_hex = sender_owner_pubkey.to_hex();
    let peer_owner_hex = if sender_owner_hex == my_owner_pubkey_hex {
        match extract_peer_from_p_tag(&decrypted_event) {
            Some(pk) if pk != my_owner_pubkey_hex => pk,
            _ => return Ok(false),
        }
    } else {
        sender_owner_hex.clone()
    };

    let Some(chat) = storage.get_chat_by_pubkey(&peer_owner_hex)? else {
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
            output.success(
                "receive",
                serde_json::json!({
                    "chat_id": updated_chat.id,
                    "from_pubkey": from_pubkey_hex,
                    "kind": CHAT_SETTINGS_KIND,
                    "message_ttl_seconds": ttl,
                    "timestamp": timestamp,
                }),
            );
            return Ok(true);
        }
        return Ok(false);
    }

    if rumor_kind == RECEIPT_KIND {
        let message_ids = extract_e_tags(&decrypted_event);
        storage.save_chat(&updated_chat)?;
        output.success(
            "receive",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "kind": RECEIPT_KIND,
                "type": content,
                "message_ids": message_ids,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    if rumor_kind == REACTION_KIND {
        let message_id = extract_e_tag(&decrypted_event);
        let stored = StoredReaction {
            id: fallback_event_id(event_id, &decrypted_event),
            chat_id: updated_chat.id.clone(),
            message_id: message_id.clone(),
            from_pubkey: from_pubkey_hex.clone(),
            emoji: content.clone(),
            timestamp,
            is_outgoing,
        };
        storage.save_reaction(&stored)?;
        storage.save_chat(&updated_chat)?;
        output.success(
            "receive",
            IncomingReaction {
                chat_id: updated_chat.id,
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
        output.success(
            "receive",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "kind": TYPING_KIND,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    let expires_at = extract_expiration_tag_seconds(&decrypted_event);
    let msg_id = fallback_event_id(event_id, &decrypted_event);
    let now = now_seconds()?;

    if is_expired(expires_at, now) {
        storage.save_chat(&updated_chat)?;
        output.success(
            "receive",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "message_id": msg_id,
                "from_pubkey": from_pubkey_hex,
                "timestamp": timestamp,
                "expires_at": expires_at,
                "expired": true,
            }),
        );
        return Ok(true);
    }

    let stored = StoredMessage {
        id: msg_id.clone(),
        chat_id: updated_chat.id.clone(),
        from_pubkey: from_pubkey_hex.clone(),
        content: content.clone(),
        timestamp,
        is_outgoing,
        expires_at,
    };
    storage.save_message(&stored)?;
    updated_chat.last_message_at = Some(timestamp);
    storage.save_chat(&updated_chat)?;

    output.success(
        "receive",
        IncomingMessage {
            chat_id: updated_chat.id,
            message_id: msg_id,
            from_pubkey: from_pubkey_hex,
            content,
            timestamp,
        },
    );
    Ok(true)
}

fn extract_group_id_tag(decrypted_event: &serde_json::Value) -> Option<String> {
    let tags = decrypted_event["tags"].as_array()?;
    tags.iter().find_map(|t| {
        let arr = t.as_array()?;
        if arr.first()?.as_str()? == "l" {
            arr.get(1)?.as_str().map(String::from)
        } else {
            None
        }
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

fn fallback_event_id(event_id: Option<&str>, decrypted_event: &serde_json::Value) -> String {
    decrypted_event
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| event_id.map(str::to_string))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn now_seconds() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}
