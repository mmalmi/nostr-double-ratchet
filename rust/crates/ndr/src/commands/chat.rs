use anyhow::Result;
use nostr_double_ratchet::{
    FileStorageAdapter, SessionManager, SessionManagerEvent, StorageAdapter,
};
use serde::Serialize;
use std::sync::Arc;

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
use crate::output::Output;
use crate::storage::Storage;

#[derive(Serialize)]
struct ChatList {
    chats: Vec<ChatInfo>,
}

#[derive(Serialize)]
struct ChatInfo {
    id: String,
    their_pubkey: String,
    created_at: u64,
    last_message_at: Option<u64>,
}

#[derive(Serialize)]
struct ChatJoinedWithEvent {
    id: String,
    their_pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_event: Option<String>,
}

/// List all chats
pub async fn list(storage: &Storage, output: &Output) -> Result<()> {
    let chats = storage.list_chats()?;

    let chat_infos: Vec<ChatInfo> = chats
        .into_iter()
        .map(|c| ChatInfo {
            id: c.id,
            their_pubkey: c.their_pubkey,
            created_at: c.created_at,
            last_message_at: c.last_message_at,
        })
        .collect();

    output.success("chat.list", ChatList { chats: chat_infos });
    Ok(())
}

/// Join a chat via invite URL
pub async fn join(url: &str, config: &Config, storage: &Storage, output: &Output) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    // 1) Legacy invite URL format (JSON in hash).
    // 2) Iris-style chat links: https://chat.iris.to/#npub1... (or #/npub1...)
    let invite = match nostr_double_ratchet::Invite::from_url(url) {
        Ok(invite) => invite,
        Err(invite_err) => {
            if let Some(pk) = crate::commands::nip19::parse_pubkey(url) {
                let their_pubkey_hex = pk.to_hex();

                // If a chat already exists, just "open" it (no new session handshake).
                if let Ok(Some(existing)) = storage.get_chat_by_pubkey(&their_pubkey_hex) {
                    output.success(
                        "chat.join",
                        ChatJoinedWithEvent {
                            id: existing.id,
                            their_pubkey: existing.their_pubkey,
                            response_event: None,
                        },
                    );
                    return Ok(());
                }

                let joined = crate::commands::public_invite::join_via_public_invite(
                    &their_pubkey_hex,
                    config,
                    storage,
                )
                .await?;

                output.success(
                    "chat.join",
                    ChatJoinedWithEvent {
                        id: joined.chat.id,
                        their_pubkey: joined.chat.their_pubkey,
                        response_event: joined
                            .response_event
                            .as_ref()
                            .map(nostr::JsonUtil::as_json),
                    },
                );
                return Ok(());
            }

            return Err(invite_err.into());
        }
    };
    if invite.purpose.as_deref() == Some("link") {
        anyhow::bail!("Link invite detected. Use 'ndr link accept <url>' instead.");
    }

    let joined = crate::commands::public_invite::join_via_invite(invite, config, storage).await?;

    output.success(
        "chat.join",
        ChatJoinedWithEvent {
            id: joined.chat.id,
            their_pubkey: joined.chat.their_pubkey,
            response_event: joined.response_event.as_ref().map(nostr::JsonUtil::as_json),
        },
    );

    Ok(())
}

/// Show chat details
pub async fn show(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    let chat = storage
        .get_chat(id)?
        .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?;

    let info = ChatInfo {
        id: chat.id,
        their_pubkey: chat.their_pubkey,
        created_at: chat.created_at,
        last_message_at: chat.last_message_at,
    };

    output.success("chat.show", info);
    Ok(())
}

/// Delete a chat
pub async fn delete(id: &str, config: &Config, storage: &Storage, output: &Output) -> Result<()> {
    let chat = storage
        .get_chat(id)?
        .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?;

    // Best-effort SessionManager cleanup to remove persisted multi-device session state.
    // Local chat file deletion below still succeeds even if this cleanup fails.
    if config.is_logged_in() {
        let _ = delete_session_manager_chat(config, storage, &chat.their_pubkey);
    }

    if storage.delete_chat(id)? {
        output.success_message("chat.delete", &format!("Deleted chat {}", id));
    } else {
        anyhow::bail!("Chat not found: {}", id);
    }
    Ok(())
}

fn delete_session_manager_chat(
    config: &Config,
    storage: &Storage,
    their_pubkey_hex: &str,
) -> Result<()> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;
    let their_pubkey = nostr::PublicKey::from_hex(their_pubkey_hex)?;

    let session_manager_store: Arc<dyn StorageAdapter> = Arc::new(FileStorageAdapter::new(
        storage.data_dir().join("session_manager"),
    )?);

    let (sm_tx, _sm_rx) = crossbeam_channel::unbounded();
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
    manager.delete_chat(their_pubkey)?;
    Ok(())
}

/// Set per-chat disappearing-message TTL (seconds) and optionally notify the peer via an encrypted
/// chat-settings rumor (kind 10448).
pub async fn ttl(
    target: &str,
    ttl: &str,
    local_only: bool,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let mut chat = crate::commands::message::resolve_target(target, storage)?;

    let ttl_trimmed = ttl.trim();
    let ttl_seconds: Option<u64> = if ttl_trimmed.eq_ignore_ascii_case("off")
        || ttl_trimmed.eq_ignore_ascii_case("null")
        || ttl_trimmed == "0"
    {
        None
    } else {
        let parsed = ttl_trimmed.parse::<u64>().map_err(|_| {
            anyhow::anyhow!("Invalid ttl (expected integer seconds or 'off'): {}", ttl)
        })?;
        if parsed == 0 {
            None
        } else {
            Some(parsed)
        }
    };

    chat.message_ttl_seconds = ttl_seconds;
    storage.save_chat(&chat)?;

    if local_only {
        output.success(
            "chat.ttl",
            serde_json::json!({
                "chat_id": chat.id,
                "their_pubkey": chat.their_pubkey,
                "message_ttl_seconds": ttl_seconds,
                "local_only": true,
            }),
        );
        return Ok(());
    }

    // Send an encrypted settings rumor so the peer can auto-adopt.
    let recipient = nostr::PublicKey::from_hex(&chat.their_pubkey)
        .map_err(|_| anyhow::anyhow!("Chat has invalid their_pubkey: {}", chat.their_pubkey))?;

    let ttl_to_send = ttl_seconds.unwrap_or(0);

    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: Arc<dyn StorageAdapter> = Arc::new(FileStorageAdapter::new(
        storage.data_dir().join("session_manager"),
    )?);
    let (sm_tx, sm_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
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

    // Import the current selected session so we can send via SessionManager.
    let device_id = chat.device_id.clone().unwrap_or_else(|| chat.id.clone());
    let state: nostr_double_ratchet::SessionState = serde_json::from_str(&chat.session_state)
        .map_err(|e| {
            anyhow::anyhow!(
                "Invalid session state: {}. Chat may not be properly initialized.",
                e
            )
        })?;
    manager.import_session_state(recipient, Some(device_id), state)?;

    let event_ids = manager.send_chat_settings(recipient, ttl_to_send)?;

    let client = connect_client(config).await?;
    let signing_keys = nostr::Keys::new(nostr::SecretKey::from_slice(&our_private_key)?);

    // Drain and publish only message events (kind 1060), skipping any invite/device housekeeping.
    let mut published_events = Vec::new();
    while let Ok(ev) = sm_rx.try_recv() {
        let signed = match ev {
            SessionManagerEvent::Publish(unsigned) => unsigned.sign_with_keys(&signing_keys)?,
            SessionManagerEvent::PublishSigned(signed) => signed,
            SessionManagerEvent::Subscribe { .. }
            | SessionManagerEvent::Unsubscribe(_)
            | SessionManagerEvent::ReceivedEvent(_)
            | SessionManagerEvent::DecryptedMessage { .. } => continue,
        };
        if signed.kind.as_u16() != nostr_double_ratchet::MESSAGE_EVENT_KIND as u16 {
            continue;
        }
        send_event_or_ignore(&client, signed.clone()).await?;
        published_events.push(signed);
    }

    // Update the stored selected session state from SessionManager.
    let mut sessions: Vec<(String, nostr_double_ratchet::SessionState)> = manager
        .export_active_sessions()
        .into_iter()
        .filter(|(owner, _, _)| *owner == recipient)
        .map(|(_, device_id, state)| (device_id, state))
        .collect();
    sessions.sort_by(|a, b| a.0.cmp(&b.0));
    if !sessions.is_empty() {
        let selected_idx = chat
            .device_id
            .as_ref()
            .and_then(|current| sessions.iter().position(|(d, _)| d == current))
            .unwrap_or(0);
        let (selected_device_id, selected_state) = sessions[selected_idx].clone();
        chat.device_id = Some(selected_device_id);
        chat.session_state = serde_json::to_string(&selected_state)?;
        storage.save_chat(&chat)?;
    }

    output.success(
        "chat.ttl",
        serde_json::json!({
            "chat_id": chat.id,
            "their_pubkey": chat.their_pubkey,
            "message_ttl_seconds": ttl_seconds,
            "local_only": false,
            "event": published_events
                .first()
                .map(nostr::JsonUtil::as_json)
                .unwrap_or_else(|| event_ids.first().cloned().unwrap_or_default()),
        }),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoredChat;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        config
            .set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        (temp, config, storage)
    }

    #[tokio::test]
    async fn test_list_chats_empty() {
        let (_temp, _config, storage) = setup();
        let output = Output::new(true);

        list(&storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_chat_crud() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        // Add a chat manually
        storage
            .save_chat(&StoredChat {
                id: "test-chat".to_string(),
                their_pubkey: "abc123".to_string(),
                device_id: None,
                created_at: 1234567890,
                last_message_at: None,
                session_state: "{}".to_string(),
                message_ttl_seconds: None,
            })
            .unwrap();

        // List
        list(&storage, &output).await.unwrap();

        // Show
        show("test-chat", &storage, &output).await.unwrap();

        // Delete
        delete("test-chat", &config, &storage, &output)
            .await
            .unwrap();

        assert!(storage.list_chats().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_delete_cleans_session_manager_user_record() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);
        let their_pubkey = nostr::Keys::generate().public_key().to_hex();

        storage
            .save_chat(&StoredChat {
                id: "delete-sync-chat".to_string(),
                their_pubkey: their_pubkey.clone(),
                device_id: None,
                created_at: 1234567890,
                last_message_at: None,
                session_state: "{}".to_string(),
                message_ttl_seconds: None,
            })
            .unwrap();

        let session_manager_dir = storage.data_dir().join("session_manager");
        std::fs::create_dir_all(&session_manager_dir).unwrap();
        let user_record_file = session_manager_dir.join(format!("user_{}.json", their_pubkey));
        std::fs::write(&user_record_file, "{}").unwrap();

        delete("delete-sync-chat", &config, &storage, &output)
            .await
            .unwrap();

        assert!(storage.get_chat("delete-sync-chat").unwrap().is_none());
        assert!(
            !user_record_file.exists(),
            "expected SessionManager user record to be removed"
        );
    }
}
