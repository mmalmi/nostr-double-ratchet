use anyhow::Result;

use crate::storage::{Storage, StoredChat};

pub fn resolve_target(target: &str, storage: &Storage) -> Result<StoredChat> {
    // 1. Try as chat_id directly (short hex, e.g. 8 chars)
    if let Ok(Some(chat)) = storage.get_chat(target) {
        return Ok(chat);
    }

    // 2. Try as npub/nprofile (or Iris-style chat link containing one) -> decode to hex pubkey -> find chat
    if let Some(pk) = crate::commands::nip19::parse_pubkey(target) {
        let hex = pk.to_hex();
        if let Ok(Some(chat)) = storage.get_chat_by_pubkey(&hex) {
            return Ok(chat);
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
    if let Some(pk) = crate::commands::nip19::parse_pubkey(target) {
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

/// Extract the NIP-40-style `["expiration", "<unix seconds>"]` tag from a decrypted rumor JSON.
///
/// Returns the last valid expiration timestamp (seconds) if multiple are present.
pub(super) fn extract_expiration_tag_seconds(event: &serde_json::Value) -> Option<u64> {
    let tags = event["tags"].as_array()?;
    let mut out: Option<u64> = None;
    for t in tags {
        let arr = t.as_array()?;
        if arr.first()?.as_str()? != nostr_double_ratchet::EXPIRATION_TAG {
            continue;
        }
        let Some(v) = arr.get(1).and_then(|v| v.as_str()) else {
            continue;
        };
        if let Ok(ts) = v.parse::<u64>() {
            out = Some(ts);
        }
    }
    out
}

pub(super) fn is_expired(expires_at: Option<u64>, now_seconds: u64) -> bool {
    expires_at.is_some_and(|ts| ts <= now_seconds)
}

/// Parse an Iris-compatible `chat-settings` payload and return the requested TTL.
///
/// Returns:
/// - `Some(Some(ttl))` when `messageTtlSeconds > 0`
/// - `Some(None)` when `messageTtlSeconds` is `0`, `null`, or missing (treated as "off")
/// - `None` when the payload is invalid or not a supported version
pub(super) fn parse_chat_settings_ttl_seconds(content: &str) -> Option<Option<u64>> {
    let payload: serde_json::Value = serde_json::from_str(content).ok()?;
    let typ = payload.get("type").and_then(|v| v.as_str());
    let v = payload.get("v").and_then(|v| v.as_u64());
    if typ != Some("chat-settings") || v != Some(1) {
        return None;
    }

    match payload.get("messageTtlSeconds") {
        None => Some(None),
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::Number(n)) => {
            let ttl = n.as_u64()?;
            if ttl == 0 {
                Some(None)
            } else {
                Some(Some(ttl))
            }
        }
        _ => None,
    }
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
