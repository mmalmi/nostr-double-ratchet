use anyhow::Result;
use nostr_double_ratchet::{FileStorageAdapter, NdrRuntime, StorageAdapter};

use crate::config::Config;
use crate::state_sync::select_canonical_session;
use crate::storage::{Storage, StoredChat};

pub(crate) fn build_runtime(
    config: &Config,
    storage: &Storage,
) -> Result<(NdrRuntime, nostr::Keys, String)> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: std::sync::Arc<dyn StorageAdapter> = std::sync::Arc::new(
        FileStorageAdapter::new(storage.data_dir().join("session_manager"))?,
    );
    let group_manager_store: std::sync::Arc<dyn StorageAdapter> = std::sync::Arc::new(
        FileStorageAdapter::new(storage.data_dir().join("group_manager"))?,
    );

    let runtime = NdrRuntime::new_with_group_storage(
        our_pubkey,
        our_private_key,
        our_pubkey_hex,
        owner_pubkey,
        Some(session_manager_store),
        Some(group_manager_store),
        None,
    );
    runtime.init()?;

    let signing_keys = nostr::Keys::new(nostr::SecretKey::from_slice(&our_private_key)?);
    Ok((runtime, signing_keys, owner_pubkey_hex))
}

pub(crate) fn sync_chats_from_runtime(
    storage: &Storage,
    runtime: &NdrRuntime,
    my_owner_pubkey_hex: &str,
) -> Result<()> {
    use std::collections::HashMap;

    let sessions = runtime.export_active_sessions();
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
                chat.session_state = state_json;
                changed = true;
            }

            if changed {
                storage.save_chat(&chat)?;
                chats[idx] = chat;
            }
            continue;
        }

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
