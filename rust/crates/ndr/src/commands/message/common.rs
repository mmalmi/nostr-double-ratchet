use anyhow::Result;

use crate::storage::{Storage, StoredChat};

pub fn resolve_target(target: &str, storage: &Storage) -> Result<StoredChat> {
    // 1. Try as chat_id directly (short hex, e.g. 8 chars)
    if let Ok(Some(chat)) = storage.get_chat(target) {
        return Ok(chat);
    }

    // 2. Try as npub -> decode to hex pubkey -> find chat
    if target.starts_with("npub1") {
        use nostr::FromBech32;
        if let Ok(pk) = nostr::PublicKey::from_bech32(target) {
            let hex = pk.to_hex();
            if let Ok(Some(chat)) = storage.get_chat_by_pubkey(&hex) {
                return Ok(chat);
            }
        }
        anyhow::bail!("No chat found for {}", target);
    }

    // 3. Try as 64-char hex pubkey
    if target.len() == 64 && target.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(Some(chat)) = storage.get_chat_by_pubkey(target) {
            return Ok(chat);
        }
        anyhow::bail!("No chat found for pubkey {}", target);
    }

    // 4. Try as petname from contacts file
    if let Ok(Some(hex)) = storage.get_contact_pubkey(target) {
        if let Ok(Some(chat)) = storage.get_chat_by_pubkey(&hex) {
            return Ok(chat);
        }
        anyhow::bail!("Contact '{}' found but no chat exists with them", target);
    }

    anyhow::bail!("Chat not found: {}", target)
}

/// Resolve a target to a hex pubkey (npub, hex pubkey, or petname).
pub(super) fn resolve_target_pubkey(target: &str, storage: &Storage) -> Result<String> {
    if target.starts_with("npub1") {
        use nostr::FromBech32;
        let pk = nostr::PublicKey::from_bech32(target)
            .map_err(|_| anyhow::anyhow!("Invalid npub: {}", target))?;
        return Ok(pk.to_hex());
    }

    if target.len() == 64 && target.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(target.to_string());
    }

    if let Ok(Some(hex)) = storage.get_contact_pubkey(target) {
        return Ok(hex);
    }

    anyhow::bail!("Target is not a pubkey or contact: {}", target)
}

/// Extract first "e" tag value from a decrypted event JSON
pub(super) fn extract_e_tag(event: &serde_json::Value) -> String {
    event["tags"]
        .as_array()
        .and_then(|tags| {
            tags.iter().find_map(|t| {
                let arr = t.as_array()?;
                if arr.first()?.as_str()? == "e" {
                    arr.get(1)?.as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default()
}

/// Extract all "e" tag values from a decrypted event JSON
pub(super) fn extract_e_tags(event: &serde_json::Value) -> Vec<String> {
    event["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|t| {
                    let arr = t.as_array()?;
                    if arr.first()?.as_str()? == "e" {
                        arr.get(1)?.as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Helper to collect ephemeral pubkeys from chats for subscription
pub(super) fn collect_chat_pubkeys(
    storage: &Storage,
    chat_id: Option<&str>,
) -> Result<Vec<nostr::PublicKey>> {
    let chats = if let Some(id) = chat_id {
        vec![storage
            .get_chat(id)?
            .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?]
    } else {
        storage.list_chats()?
    };

    let mut pubkeys: Vec<nostr::PublicKey> = Vec::new();
    for chat in &chats {
        if let Ok(state) =
            serde_json::from_str::<nostr_double_ratchet::SessionState>(&chat.session_state)
        {
            if let Some(pk) = state.their_current_nostr_public_key {
                pubkeys.push(pk);
            }
            if let Some(pk) = state.their_next_nostr_public_key {
                pubkeys.push(pk);
            }
        }
    }
    Ok(pubkeys)
}

pub(super) fn allow_insecure_shared_channel_sender_keys() -> bool {
    if let Ok(val) = std::env::var("NDR_ALLOW_INSECURE_SHARED_CHANNEL_SENDER_KEYS") {
        let val = val.trim().to_lowercase();
        return matches!(val.as_str(), "1" | "true" | "yes" | "on");
    }
    false
}
