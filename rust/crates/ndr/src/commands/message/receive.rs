use anyhow::Result;

use nostr_double_ratchet::{
    Session, CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, REACTION_KIND, RECEIPT_KIND, TYPING_KIND,
};

use crate::output::Output;
use crate::storage::{Storage, StoredGroupMessage, StoredMessage, StoredReaction};

use super::common::{
    extract_e_tag, extract_e_tags, extract_expiration_tag_seconds, is_expired,
    parse_chat_settings_ttl_seconds,
};
use super::types::{IncomingMessage, IncomingReaction};

/// Receive and decrypt a message from a nostr event
pub async fn receive(event_json: &str, storage: &Storage, output: &Output) -> Result<()> {
    // Parse the nostr event
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json)
        .map_err(|e| anyhow::anyhow!("Invalid event JSON: {}", e))?;

    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Try to find a matching chat and decrypt
    let chats = storage.list_chats()?;

    for chat in chats {
        let session_state: nostr_double_ratchet::SessionState =
            match serde_json::from_str(&chat.session_state) {
                Ok(s) => s,
                Err(_) => continue,
            };

        let mut session = Session::new(session_state, chat.id.clone());

        // Try to decrypt with this session
        match session.receive(&event) {
            Ok(Some(decrypted_event_json)) => {
                let decrypted_event: serde_json::Value =
                    serde_json::from_str(&decrypted_event_json)
                        .map_err(|e| anyhow::anyhow!("Failed to parse decrypted event: {}", e))?;

                let content = decrypted_event["content"]
                    .as_str()
                    .unwrap_or(&decrypted_event_json)
                    .to_string();

                let rumor_kind = decrypted_event["kind"]
                    .as_u64()
                    .unwrap_or(CHAT_MESSAGE_KIND as u64) as u32;

                let timestamp = event.created_at.as_u64();
                // Prefer the decrypted (inner/rumor) id when available; outer event ids vary per-device.
                let msg_id = decrypted_event["id"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| event.id.to_hex());
                let from_pubkey_hex = chat.their_pubkey.clone();
                let expires_at = extract_expiration_tag_seconds(&decrypted_event);

                // Check for group routing tag
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

                // Update session state
                let mut updated_chat = chat;
                updated_chat.session_state = serde_json::to_string(&session.state)?;

                // Encrypted 1:1 chat settings (disappearing-message signaling).
                if rumor_kind == CHAT_SETTINGS_KIND {
                    if let Some(ttl) = parse_chat_settings_ttl_seconds(&content) {
                        updated_chat.message_ttl_seconds = ttl;
                        storage.save_chat(&updated_chat)?;
                        output.success(
                            "receive",
                            serde_json::json!({
                                "chat_id": updated_chat.id,
                                "from_pubkey": from_pubkey_hex,
                                "kind": CHAT_SETTINGS_KIND,
                                "message_ttl_seconds": ttl,
                            }),
                        );
                        return Ok(());
                    }
                }

                // Receipt / reaction / typing indicators should not be stored as messages.
                if rumor_kind == RECEIPT_KIND {
                    let message_ids = extract_e_tags(&decrypted_event);
                    storage.save_chat(&updated_chat)?;
                    if let Some(gid) = group_id {
                        output.success(
                            "receive",
                            serde_json::json!({
                                "group_id": gid,
                                "from_pubkey": from_pubkey_hex,
                                "kind": RECEIPT_KIND,
                                "type": content,
                                "message_ids": message_ids,
                                "timestamp": timestamp,
                            }),
                        );
                    } else {
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
                    }
                    return Ok(());
                }

                if rumor_kind == REACTION_KIND {
                    let message_id = extract_e_tag(&decrypted_event);
                    if let Some(gid) = group_id {
                        storage.save_chat(&updated_chat)?;
                        output.success(
                            "receive",
                            serde_json::json!({
                                "group_id": gid,
                                "sender_pubkey": from_pubkey_hex,
                                "kind": REACTION_KIND,
                                "message_id": message_id,
                                "emoji": content,
                                "timestamp": timestamp,
                            }),
                        );
                    } else {
                        let stored = StoredReaction {
                            id: msg_id.clone(),
                            chat_id: updated_chat.id.clone(),
                            message_id: message_id.clone(),
                            from_pubkey: from_pubkey_hex.clone(),
                            emoji: content.clone(),
                            timestamp,
                            is_outgoing: false,
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
                    }
                    return Ok(());
                }

                if rumor_kind == TYPING_KIND {
                    storage.save_chat(&updated_chat)?;
                    if let Some(gid) = group_id {
                        output.success(
                            "receive",
                            serde_json::json!({
                                "group_id": gid,
                                "sender_pubkey": from_pubkey_hex,
                                "kind": TYPING_KIND,
                                "timestamp": timestamp,
                            }),
                        );
                    } else {
                        output.success(
                            "receive",
                            serde_json::json!({
                                "chat_id": updated_chat.id,
                                "from_pubkey": from_pubkey_hex,
                                "kind": TYPING_KIND,
                                "timestamp": timestamp,
                            }),
                        );
                    }
                    return Ok(());
                }

                if let Some(gid) = group_id {
                    // Group message
                    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                        if is_expired(expires_at, now_seconds) {
                            storage.save_chat(&updated_chat)?;
                            output.success(
                                "receive",
                                serde_json::json!({
                                    "group_id": gid,
                                    "message_id": msg_id,
                                    "sender_pubkey": from_pubkey_hex,
                                    "timestamp": timestamp,
                                    "expires_at": expires_at,
                                    "expired": true,
                                }),
                            );
                            return Ok(());
                        }
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
                    }
                    storage.save_chat(&updated_chat)?;

                    output.success(
                        "receive",
                        serde_json::json!({
                            "group_id": gid,
                            "message_id": msg_id,
                            "sender_pubkey": from_pubkey_hex,
                            "content": content,
                            "timestamp": timestamp,
                        }),
                    );
                } else {
                    // 1:1 message
                    if is_expired(expires_at, now_seconds) {
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
                        return Ok(());
                    }
                    let stored = StoredMessage {
                        id: msg_id.clone(),
                        chat_id: updated_chat.id.clone(),
                        from_pubkey: from_pubkey_hex.clone(),
                        content: content.clone(),
                        timestamp,
                        is_outgoing: false,
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
                }

                return Ok(());
            }
            Ok(None) => continue,
            Err(_) => continue,
        }
    }

    anyhow::bail!("Could not decrypt message - no matching session found");
}
