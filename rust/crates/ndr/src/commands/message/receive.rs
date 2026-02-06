use anyhow::Result;

use nostr_double_ratchet::{Session, CHAT_MESSAGE_KIND};

use crate::output::Output;
use crate::storage::{Storage, StoredGroupMessage, StoredMessage};

use super::types::IncomingMessage;

/// Receive and decrypt a message from a nostr event
pub async fn receive(event_json: &str, storage: &Storage, output: &Output) -> Result<()> {
    // Parse the nostr event
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json)
        .map_err(|e| anyhow::anyhow!("Invalid event JSON: {}", e))?;

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
                let from_pubkey_hex = chat.their_pubkey.clone();

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

                if let Some(gid) = group_id {
                    // Group message
                    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                        let msg_id = event.id.to_hex();
                        let stored = StoredGroupMessage {
                            id: msg_id.clone(),
                            group_id: gid.clone(),
                            sender_pubkey: from_pubkey_hex.clone(),
                            content: content.clone(),
                            timestamp,
                            is_outgoing: false,
                        };
                        storage.save_group_message(&stored)?;
                    }
                    storage.save_chat(&updated_chat)?;

                    output.success(
                        "receive",
                        serde_json::json!({
                            "group_id": gid,
                            "message_id": event.id.to_hex(),
                            "sender_pubkey": from_pubkey_hex,
                            "content": content,
                            "timestamp": timestamp,
                        }),
                    );
                } else {
                    // 1:1 message
                    let msg_id = event.id.to_hex();
                    let stored = StoredMessage {
                        id: msg_id.clone(),
                        chat_id: updated_chat.id.clone(),
                        from_pubkey: from_pubkey_hex.clone(),
                        content: content.clone(),
                        timestamp,
                        is_outgoing: false,
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
