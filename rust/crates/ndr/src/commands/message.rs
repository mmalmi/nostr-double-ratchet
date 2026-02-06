use anyhow::Result;
use nostr_double_ratchet::{
    OneToManyChannel, Session, CHAT_MESSAGE_KIND, GROUP_METADATA_KIND, INVITE_EVENT_KIND,
    REACTION_KIND, RECEIPT_KIND, TYPING_KIND,
};
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{
    Storage, StoredChat, StoredGroup, StoredGroupMessage, StoredGroupSender, StoredMessage,
    StoredReaction,
};

/// Resolve a target (chat_id, npub, hex pubkey, or petname) to a StoredChat.
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
fn resolve_target_pubkey(target: &str, storage: &Storage) -> Result<String> {
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

#[derive(Serialize)]
struct MessageSent {
    id: String,
    chat_id: String,
    content: String,
    timestamp: u64,
    /// The encrypted nostr event to publish
    event: String,
}

#[derive(Serialize)]
struct MessageList {
    chat_id: String,
    messages: Vec<MessageInfo>,
    reactions: Vec<ReactionInfo>,
}

#[derive(Serialize)]
struct MessageInfo {
    id: String,
    from_pubkey: String,
    content: String,
    timestamp: u64,
    is_outgoing: bool,
}

#[derive(Serialize)]
struct IncomingMessage {
    chat_id: String,
    message_id: String,
    from_pubkey: String,
    content: String,
    timestamp: u64,
}

#[derive(Serialize)]
struct IncomingReaction {
    chat_id: String,
    from_pubkey: String,
    message_id: String,
    emoji: String,
    timestamp: u64,
}

#[derive(Serialize)]
struct ReactionInfo {
    id: String,
    message_id: String,
    from_pubkey: String,
    emoji: String,
    timestamp: u64,
    is_outgoing: bool,
}

/// Send a message
pub async fn send(
    target: &str,
    message: &str,
    reply_to: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let chat = match resolve_target(target, storage) {
        Ok(chat) => chat,
        Err(resolve_err) => {
            let target_pubkey = match resolve_target_pubkey(target, storage) {
                Ok(pubkey) => pubkey,
                Err(_) => return Err(resolve_err),
            };

            match create_chat_from_public_invite(&target_pubkey, config, storage).await {
                Ok(chat) => chat,
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "Chat not found and no public invite available for {}: {}",
                        target,
                        err
                    ));
                }
            }
        }
    };
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

    // Encrypt the message (with optional reply reference)
    let encrypted_event = match reply_to {
        Some(reply_id) => session
            .send_reply(message.to_string(), reply_id)
            .map_err(|e| anyhow::anyhow!("Failed to encrypt reply: {}", e))?,
        None => session
            .send(message.to_string())
            .map_err(|e| anyhow::anyhow!("Failed to encrypt message: {}", e))?,
    };

    let pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Use the outer event ID as message ID (for reaction compatibility with iris-chat)
    let msg_id = encrypted_event.id.to_hex();

    let stored = StoredMessage {
        id: msg_id.clone(),
        chat_id: chat_id.to_string(),
        from_pubkey: pubkey,
        content: message.to_string(),
        timestamp,
        is_outgoing: true,
    };

    storage.save_message(&stored)?;

    // Update chat with new session state and last_message_at
    let mut updated_chat = chat;
    updated_chat.last_message_at = Some(timestamp);
    updated_chat.session_state = serde_json::to_string(&session.state)?;
    storage.save_chat(&updated_chat)?;

    // Publish to relays
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    send_event_or_ignore(&client, encrypted_event.clone()).await?;

    output.success(
        "send",
        MessageSent {
            id: msg_id,
            chat_id: chat_id.to_string(),
            content: message.to_string(),
            timestamp,
            event: nostr::JsonUtil::as_json(&encrypted_event),
        },
    );

    Ok(())
}

async fn create_chat_from_public_invite(
    target_pubkey_hex: &str,
    config: &Config,
    storage: &Storage,
) -> Result<StoredChat> {
    use nostr_sdk::Filter;
    use std::time::Duration;

    let target_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(target_pubkey_hex)?;
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;

    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    let filter = Filter::new()
        .kind(nostr::Kind::Custom(INVITE_EVENT_KIND as u16))
        .author(target_pubkey)
        .limit(10);

    let events = client
        .fetch_events(vec![filter], Some(Duration::from_secs(10)))
        .await?;
    let has_tag = |event: &nostr::Event, name: &str, value: &str| {
        event.tags.iter().any(|t| {
            let parts = t.as_slice();
            parts.first().map(|s| s.as_str()) == Some(name)
                && parts.get(1).map(|s| s.as_str()) == Some(value)
        })
    };

    let public_invite = events.iter().find_map(|event| {
        if has_tag(event, "d", "double-ratchet/invites/public") {
            nostr_double_ratchet::Invite::from_event(event).ok()
        } else {
            None
        }
    });

    let invite = public_invite
        .or_else(|| {
            events
                .iter()
                .find_map(|event| nostr_double_ratchet::Invite::from_event(event).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("No public invite found for {}", target_pubkey_hex))?;

    let their_pubkey_hex = invite.inviter.to_hex();
    let (session, response_event) =
        invite.accept_with_owner(our_pubkey, our_private_key, None, Some(owner_pubkey))?;

    let session_state = serde_json::to_string(&session.state)?;
    let chat_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let chat = StoredChat {
        id: chat_id.clone(),
        their_pubkey: their_pubkey_hex,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state,
    };

    storage.save_chat(&chat)?;
    send_event_or_ignore(&client, response_event).await?;

    Ok(chat)
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

    // Load session state
    let session_state: nostr_double_ratchet::SessionState =
        serde_json::from_str(&chat.session_state).map_err(|e| {
            anyhow::anyhow!(
                "Invalid session state: {}. Chat may not be properly initialized.",
                e
            )
        })?;

    let mut session = Session::new(session_state, chat_id.to_string());

    // Send the reaction
    let encrypted_event = session
        .send_reaction(message_id, emoji)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt reaction: {}", e))?;

    let pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Save outgoing reaction
    let reaction_id = encrypted_event.id.to_hex();
    let stored = StoredReaction {
        id: reaction_id.clone(),
        chat_id: chat_id.to_string(),
        message_id: message_id.to_string(),
        from_pubkey: pubkey,
        emoji: emoji.to_string(),
        timestamp,
        is_outgoing: true,
    };
    storage.save_reaction(&stored)?;

    // Update chat with new session state
    let mut updated_chat = chat;
    updated_chat.session_state = serde_json::to_string(&session.state)?;
    storage.save_chat(&updated_chat)?;

    // Publish to relays
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    send_event_or_ignore(&client, encrypted_event.clone()).await?;

    output.success(
        "react",
        serde_json::json!({
            "id": reaction_id,
            "chat_id": chat_id,
            "message_id": message_id,
            "emoji": emoji,
            "timestamp": timestamp,
            "event": nostr::JsonUtil::as_json(&encrypted_event),
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

    let session_state: nostr_double_ratchet::SessionState =
        serde_json::from_str(&chat.session_state).map_err(|e| {
            anyhow::anyhow!(
                "Invalid session state: {}. Chat may not be properly initialized.",
                e
            )
        })?;

    let mut session = Session::new(session_state, chat_id.to_string());

    let encrypted_event = session
        .send_receipt(receipt_type, message_ids)
        .map_err(|e| anyhow::anyhow!("Failed to encrypt receipt: {}", e))?;

    // Update chat with new session state
    let mut updated_chat = chat;
    updated_chat.session_state = serde_json::to_string(&session.state)?;
    storage.save_chat(&updated_chat)?;

    // Publish to relays
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    send_event_or_ignore(&client, encrypted_event.clone()).await?;

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

    let session_state: nostr_double_ratchet::SessionState =
        serde_json::from_str(&chat.session_state).map_err(|e| {
            anyhow::anyhow!(
                "Invalid session state: {}. Chat may not be properly initialized.",
                e
            )
        })?;

    let mut session = Session::new(session_state, chat_id.to_string());

    let encrypted_event = session
        .send_typing()
        .map_err(|e| anyhow::anyhow!("Failed to send typing indicator: {}", e))?;

    // Update chat with new session state (ratchet advances)
    let mut updated_chat = chat;
    updated_chat.session_state = serde_json::to_string(&session.state)?;
    storage.save_chat(&updated_chat)?;

    // Publish to relays
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    send_event_or_ignore(&client, encrypted_event.clone()).await?;

    output.success(
        "typing",
        serde_json::json!({
            "chat_id": chat_id,
            "event": nostr::JsonUtil::as_json(&encrypted_event),
        }),
    );

    Ok(())
}

/// Read messages from a chat
pub async fn read(target: &str, limit: usize, storage: &Storage, output: &Output) -> Result<()> {
    let chat = resolve_target(target, storage)?;
    let chat_id = chat.id.clone();

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

/// Extract first "e" tag value from a decrypted event JSON
fn extract_e_tag(event: &serde_json::Value) -> String {
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
fn extract_e_tags(event: &serde_json::Value) -> Vec<String> {
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
fn collect_chat_pubkeys(storage: &Storage, chat_id: Option<&str>) -> Result<Vec<nostr::PublicKey>> {
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

async fn send_event_or_ignore(client: &Client, event: nostr::Event) -> Result<()> {
    match client.send_event(event).await {
        Ok(_) => Ok(()),
        Err(_) if should_ignore_publish_errors() => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn should_ignore_publish_errors() -> bool {
    for key in ["NDR_IGNORE_PUBLISH_ERRORS", "NOSTR_IGNORE_PUBLISH_ERRORS"] {
        if let Ok(val) = std::env::var(key) {
            let val = val.trim().to_lowercase();
            return matches!(val.as_str(), "1" | "true" | "yes" | "on");
        }
    }
    false
}

fn allow_insecure_shared_channel_sender_keys() -> bool {
    if let Ok(val) = std::env::var("NDR_ALLOW_INSECURE_SHARED_CHANNEL_SENDER_KEYS") {
        let val = val.trim().to_lowercase();
        return matches!(val.as_str(), "1" | "true" | "yes" | "on");
    }
    false
}

/// Listen for new messages and invite responses
pub async fn listen(
    chat_id: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    use nostr_double_ratchet::{
        GROUP_INVITE_RUMOR_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND, GROUP_SENDER_KEY_MESSAGE_KIND,
        INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND, SHARED_CHANNEL_KIND,
    };
    use nostr_sdk::{Client, Filter, RelayPoolNotification};
    use notify::{Event as NotifyEvent, EventKind, RecursiveMode, Watcher};
    use std::collections::{HashMap, HashSet};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let our_private_key = config.private_key_bytes()?;
    let mut config = config.clone();
    let chat_id_owned = chat_id.map(|s| s.to_string());

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

    type GroupSenderMap = HashMap<String, (String, String)>;

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
                if !members.contains(&sender.identity_pubkey) {
                    continue;
                }
                if nostr::PublicKey::from_hex(&sender.sender_event_pubkey).is_err() {
                    continue;
                }
                map.insert(
                    sender.sender_event_pubkey.clone(),
                    (group.data.id.clone(), sender.identity_pubkey.clone()),
                );
            }
        }
        Ok(map)
    };

    // Helper to build filters from current state
    let build_filters =
        |storage: &Storage,
         chat_id: Option<&str>,
         channel_map: &HashMap<String, (nostr_double_ratchet::SharedChannel, String)>,
         group_sender_map: &GroupSenderMap|
         -> Result<FilterState> {
            let pubkeys_to_watch = collect_chat_pubkeys(storage, chat_id)?;
            let subscribed_pubkeys: HashSet<String> =
                pubkeys_to_watch.iter().map(|pk| pk.to_hex()).collect();

            let mut filters = Vec::new();

            if !pubkeys_to_watch.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                        .authors(pubkeys_to_watch),
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
                        .authors(group_sender_pubkeys),
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
                        .pubkeys(ephemeral_pubkeys),
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
                        .authors(channel_pubkeys),
                );
            }

            Ok((
                filters,
                subscribed_pubkeys,
                invite_pubkeys,
                channel_pubkeys_hex,
                group_sender_pubkeys_hex,
            ))
        };

    // Build initial filters
    let my_pubkey = config.public_key()?;
    let my_pubkey_key = nostr::PublicKey::from_hex(&my_pubkey)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;
    let one_to_many = OneToManyChannel::default();
    let mut channel_map = build_channel_map(storage)?;
    let mut group_sender_map = build_group_sender_map(storage)?;
    let (
        mut filters,
        mut subscribed_pubkeys,
        mut subscribed_invite_pubkeys,
        mut subscribed_channel_pubkeys,
        mut subscribed_group_sender_pubkeys,
    ) = build_filters(storage, chat_id, &channel_map, &group_sender_map)?;
    let mut last_refresh = Instant::now();

    // If we receive sender-key messages before the sender-key distribution, keep them here and retry
    // when the distribution arrives.
    let mut pending_sender_key_messages: HashMap<(String, String, u32), Vec<nostr::Event>> =
        HashMap::new();

    // If we receive per-sender published group messages before the sender-key distribution, keep
    // them here and retry when the distribution arrives.
    let mut pending_group_sender_events: HashMap<(String, u32), Vec<nostr::Event>> = HashMap::new();

    output.success_message(
        "listen",
        &format!(
            "Listening for messages and invite responses on {}... (Ctrl+C to stop)",
            scope
        ),
    );

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
        client.subscribe(filters.clone(), None).await?;
    }

    // Wait for invites/chats if we have nothing to subscribe to yet
    while !has_subscription {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Check for filesystem changes
        while let Ok(()) = fs_rx.try_recv() {
            channel_map = build_channel_map(storage)?;
            group_sender_map = build_group_sender_map(storage)?;
            let (
                new_filters,
                new_pubkeys,
                new_invite_pubkeys,
                new_channel_pubkeys,
                new_group_sender_pubkeys,
            ) = build_filters(
                storage,
                chat_id_owned.as_deref(),
                &channel_map,
                &group_sender_map,
            )?;
            if !new_filters.is_empty() {
                filters = new_filters;
                subscribed_pubkeys = new_pubkeys;
                subscribed_invite_pubkeys = new_invite_pubkeys;
                subscribed_channel_pubkeys = new_channel_pubkeys;
                subscribed_group_sender_pubkeys = new_group_sender_pubkeys;
                if !connected {
                    client.connect().await;
                    connected = true;
                }
                client.subscribe(filters.clone(), None).await?;
                has_subscription = true;
                break;
            }
        }
    }

    // Handle incoming events - only start after we have a subscription
    let mut notifications = client.notifications();
    loop {
        // Check for filesystem changes (new invites/chats created by other processes)
        let mut should_refresh = false;
        while let Ok(()) = fs_rx.try_recv() {
            should_refresh = true;
        }
        if should_refresh || last_refresh.elapsed() >= Duration::from_secs(1) {
            channel_map = build_channel_map(storage)?;
            group_sender_map = build_group_sender_map(storage)?;
            let (
                new_filters,
                new_pubkeys,
                new_invite_pubkeys,
                new_channel_pubkeys,
                new_group_sender_pubkeys,
            ) = build_filters(
                storage,
                chat_id_owned.as_deref(),
                &channel_map,
                &group_sender_map,
            )?;
            if !new_filters.is_empty()
                && (new_filters.len() != filters.len()
                    || new_pubkeys != subscribed_pubkeys
                    || new_invite_pubkeys != subscribed_invite_pubkeys
                    || new_channel_pubkeys != subscribed_channel_pubkeys
                    || new_group_sender_pubkeys != subscribed_group_sender_pubkeys)
            {
                filters = new_filters;
                subscribed_pubkeys = new_pubkeys;
                subscribed_invite_pubkeys = new_invite_pubkeys;
                subscribed_channel_pubkeys = new_channel_pubkeys;
                subscribed_group_sender_pubkeys = new_group_sender_pubkeys;
                client.subscribe(filters.clone(), None).await?;
            }
            last_refresh = Instant::now();
        }

        // Wait for relay notification with timeout to allow fs check
        let notification = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            notifications.recv(),
        )
        .await;

        let notification = match notification {
            Ok(Ok(n)) => n,
            Ok(Err(_)) => break, // Channel closed
            Err(_) => continue,  // Timeout, loop to check fs
        };

        if let RelayPoolNotification::Event { event, .. } = notification {
            let event_kind = event.kind.as_u16() as u32;

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
                            if invite.purpose.as_deref() == Some("link") {
                                let owner_pubkey = response
                                    .owner_public_key
                                    .unwrap_or(response.invitee_identity);
                                let owner_pubkey_hex = owner_pubkey.to_hex();

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

                            let session = response.session;
                            let their_pubkey = response
                                .owner_public_key
                                .unwrap_or(response.invitee_identity);
                            let session_state = serde_json::to_string(&session.state)?;
                            let new_chat_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
                            let their_pubkey_hex = hex::encode(their_pubkey.to_bytes());

                            let chat = crate::storage::StoredChat {
                                id: new_chat_id.clone(),
                                their_pubkey: their_pubkey_hex.clone(),
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)?
                                    .as_secs(),
                                last_message_at: None,
                                session_state,
                            };

                            storage.save_chat(&chat)?;
                            storage.delete_invite(&stored_invite.id)?;

                            output.event(
                                "session_created",
                                serde_json::json!({
                                    "invite_id": stored_invite.id,
                                    "chat_id": new_chat_id,
                                    "their_pubkey": their_pubkey_hex,
                                }),
                            );

                            // Update subscription for new chat's ephemeral keys
                            let new_pubkeys =
                                collect_chat_pubkeys(storage, chat_id_owned.as_deref())?;
                            if !new_pubkeys.is_empty() {
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone());
                                client.subscribe(vec![new_filter], None).await?;
                                subscribed_pubkeys =
                                    new_pubkeys.iter().map(|pk| pk.to_hex()).collect();
                            }

                            break;
                        }
                        Ok(None) => continue,
                        Err(_) => continue,
                    }
                }
                continue;
            }

            // Handle SharedChannel events (kind 4)
            if event_kind == SHARED_CHANNEL_KIND {
                let sender_hex = event.pubkey.to_hex();
                if let Some((channel, group_id)) = channel_map.get(&sender_hex) {
                    if let Ok(inner_json) = channel.decrypt_event(&event) {
                        // We support two formats on the shared channel:
                        // 1) Unsigned "rumor" JSON (used for legacy group invites).
                        // 2) Signed Nostr events (used for sender-key distribution and messages).

                        if let Ok(inner_event) = (|| {
                            let ev: nostr::Event = nostr::JsonUtil::from_json(inner_json.as_str())?;
                            Ok::<nostr::Event, nostr::event::Error>(ev)
                        })() {
                            // Signed event path
                            if inner_event.verify().is_err() {
                                continue;
                            }

                            // Skip our own events.
                            if inner_event.pubkey.to_hex() == my_pubkey {
                                continue;
                            }

                            match inner_event.kind.as_u16() as u32 {
                                GROUP_SENDER_KEY_DISTRIBUTION_KIND => {
                                    // Forward secrecy note:
                                    // SharedChannel encryption is a static shared secret; don't rely on it for sender-key
                                    // distribution unless explicitly opted-in for compatibility.
                                    if !allow_insecure_shared_channel_sender_keys() {
                                        continue;
                                    }

                                    // Sender-key distribution (one per sender per group, plus rotations).
                                    let dist = match serde_json::from_str::<
                                        nostr_double_ratchet::SenderKeyDistribution,
                                    >(
                                        &inner_event.content
                                    ) {
                                        Ok(d) => d,
                                        Err(_) => continue,
                                    };

                                    if dist.group_id != *group_id {
                                        continue;
                                    }

                                    let sender_pubkey_hex = inner_event.pubkey.to_hex();
                                    if let Some(group) = storage.get_group(group_id)? {
                                        if !group.data.members.contains(&sender_pubkey_hex) {
                                            continue;
                                        }
                                    }

                                    // Don't overwrite an existing progressed state (would break decrypt).
                                    if storage
                                        .get_group_sender_key_state(
                                            group_id,
                                            &sender_pubkey_hex,
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
                                            group_id,
                                            &sender_pubkey_hex,
                                            &state,
                                        )?;
                                    }

                                    // Retry any pending messages for this (group,sender,key_id).
                                    let pending_key =
                                        (group_id.clone(), sender_pubkey_hex.clone(), dist.key_id);
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
                                            // Re-run handling by re-inserting into the loop's logic:
                                            // simplest is to just push it back into the current branch below.
                                            // We'll attempt decrypt inline.
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
                                                    group_id,
                                                    &sender_pubkey_hex,
                                                    key_id,
                                                )?
                                            {
                                                if let Ok(plaintext_json) =
                                                    st.decrypt(n, &env.content)
                                                {
                                                    storage.upsert_group_sender_key_state(
                                                        group_id,
                                                        &sender_pubkey_hex,
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

                                                        if rumor_kind == CHAT_MESSAGE_KIND
                                                            || rumor_kind == 14
                                                        {
                                                            let msg_id = env.id.to_hex();
                                                            let stored = StoredGroupMessage {
                                                                id: msg_id.clone(),
                                                                group_id: group_id.clone(),
                                                                sender_pubkey: sender_pubkey_hex
                                                                    .clone(),
                                                                content: content.clone(),
                                                                timestamp,
                                                                is_outgoing: false,
                                                            };
                                                            storage.save_group_message(&stored)?;
                                                            output.event(
                                                                "group_message",
                                                                serde_json::json!({
                                                                    "group_id": group_id,
                                                                    "message_id": msg_id,
                                                                    "sender_pubkey": sender_pubkey_hex,
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
                                                                    "group_id": group_id,
                                                                    "sender_pubkey": sender_pubkey_hex,
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
                                                                    "sender_pubkey": sender_pubkey_hex,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        }
                                                    }
                                                } else {
                                                    // Still can't decrypt; drop.
                                                }
                                            }
                                        }
                                    }

                                    output.event(
                                        "group_sender_key",
                                        serde_json::json!({
                                            "group_id": group_id,
                                            "sender_pubkey": sender_pubkey_hex,
                                            "key_id": dist.key_id,
                                            "iteration": dist.iteration,
                                            "transport": "shared_channel",
                                        }),
                                    );
                                }
                                GROUP_SENDER_KEY_MESSAGE_KIND => {
                                    // Sender-key encrypted group event.
                                    let sender_pubkey_hex = inner_event.pubkey.to_hex();

                                    // Extract group id + header tags.
                                    let group_tag = inner_event.tags.iter().find_map(|t| {
                                        let v = t.clone().to_vec();
                                        if v.first().map(|s| s.as_str()) == Some("l") {
                                            v.get(1).cloned()
                                        } else {
                                            None
                                        }
                                    });
                                    let gid = group_tag.unwrap_or_else(|| group_id.clone());
                                    if gid != *group_id {
                                        continue;
                                    }

                                    let key_id = inner_event
                                        .tags
                                        .iter()
                                        .find_map(|t| {
                                            let v = t.clone().to_vec();
                                            if v.first().map(|s| s.as_str()) == Some("key") {
                                                v.get(1)?.parse::<u32>().ok()
                                            } else {
                                                None
                                            }
                                        })
                                        .unwrap_or(0);
                                    let n = inner_event
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

                                    let Some(mut st) = storage.get_group_sender_key_state(
                                        &gid,
                                        &sender_pubkey_hex,
                                        key_id,
                                    )?
                                    else {
                                        pending_sender_key_messages
                                            .entry((gid.clone(), sender_pubkey_hex.clone(), key_id))
                                            .or_default()
                                            .push(inner_event);
                                        continue;
                                    };

                                    let plaintext_json = match st.decrypt(n, &inner_event.content) {
                                        Ok(p) => p,
                                        Err(_) => continue,
                                    };
                                    storage.upsert_group_sender_key_state(
                                        &gid,
                                        &sender_pubkey_hex,
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

                                    let timestamp = inner_event.created_at.as_u64();

                                    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                                        let msg_id = inner_event.id.to_hex();
                                        let stored = StoredGroupMessage {
                                            id: msg_id.clone(),
                                            group_id: gid.clone(),
                                            sender_pubkey: sender_pubkey_hex.clone(),
                                            content: content.clone(),
                                            timestamp,
                                            is_outgoing: false,
                                        };
                                        storage.save_group_message(&stored)?;

                                        output.event(
                                            "group_message",
                                            serde_json::json!({
                                                "group_id": gid,
                                                "message_id": msg_id,
                                                "sender_pubkey": sender_pubkey_hex,
                                                "content": content,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    } else if rumor_kind == REACTION_KIND {
                                        let message_id = extract_e_tag(&decrypted_event);
                                        output.event(
                                            "group_reaction",
                                            serde_json::json!({
                                                "group_id": gid,
                                                "sender_pubkey": sender_pubkey_hex,
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
                                                "sender_pubkey": sender_pubkey_hex,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    }
                                }
                                _ => {}
                            }

                            continue;
                        }

                        // Unsigned rumor path (legacy)
                        if let Ok(rumor) = serde_json::from_str::<serde_json::Value>(&inner_json) {
                            let rumor_kind = rumor["kind"].as_u64().unwrap_or(0) as u32;
                            let rumor_pubkey = rumor["pubkey"].as_str().unwrap_or("").to_string();

                            // Skip if it's our own event
                            if rumor_pubkey == my_pubkey {
                                continue;
                            }

                            if rumor_kind == GROUP_INVITE_RUMOR_KIND {
                                // Group member published an invite on the channel
                                let content_str = rumor["content"].as_str().unwrap_or("{}");
                                if let Ok(invite_data) =
                                    serde_json::from_str::<serde_json::Value>(content_str)
                                {
                                    let invite_url =
                                        invite_data["inviteUrl"].as_str().unwrap_or("");

                                    // Check if this is from a group member
                                    if let Some(group) = storage.get_group(group_id)? {
                                        if !group.data.members.contains(&rumor_pubkey) {
                                            continue;
                                        }

                                        // Skip if we already have a session with this member
                                        if storage.get_chat_by_pubkey(&rumor_pubkey)?.is_some() {
                                            continue;
                                        }

                                        // Auto-accept: parse invite URL, create session
                                        if !invite_url.is_empty() {
                                            if let Ok(invite) =
                                                nostr_double_ratchet::Invite::from_url(invite_url)
                                            {
                                                if let Ok((accept_session, response_event)) = invite
                                                    .accept_with_owner(
                                                        my_pubkey_key.clone(),
                                                        our_private_key,
                                                        None,
                                                        Some(owner_pubkey.clone()),
                                                    )
                                                {
                                                    // Save the new chat
                                                    let new_chat_id = uuid::Uuid::new_v4()
                                                        .to_string()[..8]
                                                        .to_string();
                                                    let session_state_str = serde_json::to_string(
                                                        &accept_session.state,
                                                    )?;

                                                    let chat = crate::storage::StoredChat {
                                                        id: new_chat_id.clone(),
                                                        their_pubkey: rumor_pubkey.clone(),
                                                        created_at: std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)?
                                                            .as_secs(),
                                                        last_message_at: None,
                                                        session_state: session_state_str,
                                                    };
                                                    storage.save_chat(&chat)?;

                                                    // Publish the response event
                                                    client.send_event(response_event).await?;

                                                    output.event(
                                                        "group_invite_accepted",
                                                        serde_json::json!({
                                                            "group_id": group_id,
                                                            "member_pubkey": rumor_pubkey,
                                                            "chat_id": new_chat_id,
                                                        }),
                                                    );

                                                    // Update subscription for new chat's keys
                                                    let new_pubkeys = collect_chat_pubkeys(
                                                        storage,
                                                        chat_id_owned.as_deref(),
                                                    )?;
                                                    if !new_pubkeys.is_empty() {
                                                        let new_filter = Filter::new()
                                                            .kind(nostr::Kind::Custom(
                                                                MESSAGE_EVENT_KIND as u16,
                                                            ))
                                                            .authors(new_pubkeys.clone());
                                                        client
                                                            .subscribe(vec![new_filter], None)
                                                            .await?;
                                                        subscribed_pubkeys = new_pubkeys
                                                            .iter()
                                                            .map(|pk| pk.to_hex())
                                                            .collect();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // Handle messages
            if event_kind == MESSAGE_EVENT_KIND {
                // === Per-sender published group message ===
                let sender_event_pubkey_hex = event.pubkey.to_hex();
                if let Some((group_id, sender_identity_pubkey_hex)) =
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
                    if !group.data.members.contains(&sender_identity_pubkey_hex) {
                        continue;
                    }

                    let Some(mut st) = storage.get_group_sender_key_state(
                        &group_id,
                        &sender_identity_pubkey_hex,
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
                        &sender_identity_pubkey_hex,
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

                    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                        let msg_id = event.id.to_hex();
                        let stored = StoredGroupMessage {
                            id: msg_id.clone(),
                            group_id: group_id.clone(),
                            sender_pubkey: sender_identity_pubkey_hex.clone(),
                            content: content.clone(),
                            timestamp,
                            is_outgoing: false,
                        };
                        storage.save_group_message(&stored)?;

                        output.event(
                            "group_message",
                            serde_json::json!({
                                "group_id": group_id,
                                "message_id": msg_id,
                                "sender_pubkey": sender_identity_pubkey_hex,
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
                                "sender_pubkey": sender_identity_pubkey_hex,
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
                                "sender_pubkey": sender_identity_pubkey_hex,
                                "timestamp": timestamp,
                            }),
                        );
                    }

                    continue;
                }

                for chat in storage.list_chats()? {
                    let session_state: nostr_double_ratchet::SessionState =
                        match serde_json::from_str(&chat.session_state) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };

                    let mut session = Session::new(session_state, chat.id.clone());

                    match session.receive(&event) {
                        Ok(Some(decrypted_event_json)) => {
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
                            let from_pubkey_hex = chat.their_pubkey.clone();

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
                                        let my_pubkey = config.public_key()?;
                                        let existing = storage.get_group(gid)?;

                                        match existing {
                                            Some(existing_group) => {
                                                let validation = nostr_double_ratchet::group::validate_metadata_update(
                                                    &existing_group.data,
                                                    &metadata,
                                                    &from_pubkey_hex,
                                                    &my_pubkey,
                                                );
                                                match validation {
                                                    nostr_double_ratchet::group::MetadataValidation::Accept => {
                                                        let old_secret = existing_group.data.secret.clone();
                                                        let updated = nostr_double_ratchet::group::apply_metadata_update(
                                                            &existing_group.data,
                                                            &metadata,
                                                        );
                                                        storage.save_group(&StoredGroup { data: updated.clone() })?;

                                                        // If the group's shared-channel secret rotated (e.g. membership changed),
                                                        // force *our* sender key to rotate too so newly-added members can decrypt.
                                                        if updated.secret != old_secret {
                                                            let _ = storage.delete_group_sender_keys(gid, &my_pubkey)?;
                                                        }

                                                        storage.save_chat(&updated_chat)?;
                                                        output.event("group_metadata", serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "updated",
                                                            "sender_pubkey": from_pubkey_hex,
                                                            "name": updated.name,
                                                            "members": updated.members,
                                                            "admins": updated.admins,
                                                        }));
                                                    }
                                                    nostr_double_ratchet::group::MetadataValidation::Removed => {
                                                        storage.delete_group(gid)?;
                                                        storage.save_chat(&updated_chat)?;
                                                        output.event("group_metadata", serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "removed",
                                                            "sender_pubkey": from_pubkey_hex,
                                                        }));
                                                    }
                                                    nostr_double_ratchet::group::MetadataValidation::Reject => {}
                                                }
                                            }
                                            None => {
                                                // New group creation
                                                if nostr_double_ratchet::group::validate_metadata_creation(
                                                    &metadata,
                                                    &from_pubkey_hex,
                                                    &my_pubkey,
                                                ) {
                                                    let group_data = nostr_double_ratchet::group::GroupData {
                                                        id: metadata.id.clone(),
                                                        name: metadata.name.clone(),
                                                        description: metadata.description,
                                                        picture: metadata.picture,
                                                        members: metadata.members.clone(),
                                                        admins: metadata.admins.clone(),
                                                        created_at: timestamp * 1000,
                                                        secret: metadata.secret,
                                                        accepted: None,
                                                    };
                                                    storage.save_group(&StoredGroup { data: group_data })?;
                                                    storage.save_chat(&updated_chat)?;
                                                    output.event("group_metadata", serde_json::json!({
                                                        "group_id": gid,
                                                        "action": "created",
                                                        "sender_pubkey": from_pubkey_hex,
                                                        "name": metadata.name,
                                                        "members": metadata.members,
                                                        "admins": metadata.admins,
                                                    }));
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

                                    // Learn/update the sender's per-group outer pubkey mapping.
                                    if let Some(ref sender_event_pubkey) = dist.sender_event_pubkey
                                    {
                                        if nostr::PublicKey::from_hex(sender_event_pubkey).is_ok() {
                                            storage.upsert_group_sender(&StoredGroupSender {
                                                group_id: gid.clone(),
                                                identity_pubkey: from_pubkey_hex.clone(),
                                                sender_event_pubkey: sender_event_pubkey.clone(),
                                                sender_event_secret_key: None,
                                            })?;
                                        }
                                    }

                                    // Don't overwrite an existing progressed state (would break decrypt).
                                    if storage
                                        .get_group_sender_key_state(
                                            gid,
                                            &from_pubkey_hex,
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
                                            &from_pubkey_hex,
                                            &state,
                                        )?;
                                    }

                                    // Retry any pending SharedChannel messages for this (group,sender,key_id).
                                    let pending_key =
                                        (gid.clone(), from_pubkey_hex.clone(), dist.key_id);
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
                                                    &from_pubkey_hex,
                                                    key_id,
                                                )?
                                            {
                                                if let Ok(plaintext_json) =
                                                    st.decrypt(n, &env.content)
                                                {
                                                    storage.upsert_group_sender_key_state(
                                                        gid,
                                                        &from_pubkey_hex,
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

                                                        if rumor_kind == CHAT_MESSAGE_KIND
                                                            || rumor_kind == 14
                                                        {
                                                            let msg_id = env.id.to_hex();
                                                            let stored = StoredGroupMessage {
                                                                id: msg_id.clone(),
                                                                group_id: gid.clone(),
                                                                sender_pubkey: from_pubkey_hex
                                                                    .clone(),
                                                                content: content.clone(),
                                                                timestamp,
                                                                is_outgoing: false,
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
                                                        } else if rumor_kind == REACTION_KIND {
                                                            let message_id =
                                                                extract_e_tag(&decrypted_event);
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
                                                            output.event(
                                                                "group_typing",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "sender_pubkey": from_pubkey_hex,
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
                                                        &from_pubkey_hex,
                                                        key_id,
                                                    )?
                                                {
                                                    if let Ok(plaintext_json) =
                                                        st.decrypt_from_bytes(n, &ciphertext)
                                                    {
                                                        storage.upsert_group_sender_key_state(
                                                            gid,
                                                            &from_pubkey_hex,
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

                                                            if rumor_kind == CHAT_MESSAGE_KIND
                                                                || rumor_kind == 14
                                                            {
                                                                let msg_id = outer.id.to_hex();
                                                                let stored = StoredGroupMessage {
                                                                    id: msg_id.clone(),
                                                                    group_id: gid.clone(),
                                                                    sender_pubkey: from_pubkey_hex
                                                                        .clone(),
                                                                    content: content.clone(),
                                                                    timestamp,
                                                                    is_outgoing: false,
                                                                };
                                                                storage
                                                                    .save_group_message(&stored)?;
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
                                                            } else if rumor_kind == REACTION_KIND {
                                                                let message_id =
                                                                    extract_e_tag(&decrypted_event);
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
                                                                output.event(
                                                                    "group_typing",
                                                                    serde_json::json!({
                                                                        "group_id": gid,
                                                                        "sender_pubkey": from_pubkey_hex,
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
                                            "key_id": dist.key_id,
                                            "iteration": dist.iteration,
                                            "sender_event_pubkey": dist.sender_event_pubkey,
                                            "transport": "session",
                                        }),
                                    );
                                } else if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                                    // Group chat message
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
                                    storage.save_chat(&updated_chat)?;

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
                                if rumor_kind == RECEIPT_KIND {
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
                                    let msg_id = event.id.to_hex();
                                    let stored = StoredMessage {
                                        id: msg_id.clone(),
                                        chat_id: chat.id.clone(),
                                        from_pubkey: from_pubkey_hex.clone(),
                                        content: content.clone(),
                                        timestamp,
                                        is_outgoing: false,
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

                            storage.save_chat(&updated_chat)?;

                            // KEY FIX: Update subscription after receiving a message
                            // because the ratchet may have rotated ephemeral keys
                            let new_pubkeys =
                                collect_chat_pubkeys(storage, chat_id_owned.as_deref())?;
                            let new_pubkey_set: HashSet<String> =
                                new_pubkeys.iter().map(|pk| pk.to_hex()).collect();

                            if new_pubkey_set != subscribed_pubkeys {
                                // Keys changed, resubscribe
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone());
                                client.subscribe(vec![new_filter], None).await?;
                                subscribed_pubkeys = new_pubkey_set;
                            }

                            break;
                        }
                        _ => continue,
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoredChat;
    use std::sync::Once;
    use tempfile::TempDir;

    fn create_test_session() -> nostr_double_ratchet::Session {
        // Create an invite
        let alice_keys = nostr::Keys::generate();
        let bob_keys = nostr::Keys::generate();

        let invite =
            nostr_double_ratchet::Invite::create_new(alice_keys.public_key(), None, None).unwrap();

        // Bob accepts the invite - this creates a session where Bob can send
        let bob_pk = bob_keys.public_key();
        let (bob_session, _response) = invite
            .accept_with_owner(
                bob_pk,
                bob_keys.secret_key().to_secret_bytes(),
                None,
                Some(bob_pk),
            )
            .unwrap();

        bob_session
    }

    fn init_test_env() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            std::env::set_var("NDR_IGNORE_PUBLISH_ERRORS", "1");
            std::env::set_var("NOSTR_PREFER_LOCAL", "0");
        });
    }

    fn setup() -> (TempDir, Config, Storage, String) {
        init_test_env();
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        config
            .set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();

        // Create a proper test session
        let session = create_test_session();
        let session_state = serde_json::to_string(&session.state).unwrap();

        // Create a test chat with valid session
        storage
            .save_chat(&StoredChat {
                id: "test-chat".to_string(),
                their_pubkey: "abc123".to_string(),
                created_at: 1234567890,
                last_message_at: None,
                session_state: session_state.clone(),
            })
            .unwrap();

        (temp, config, storage, session_state)
    }

    #[tokio::test]
    async fn test_send_message() {
        let (_temp, config, storage, _) = setup();
        let output = Output::new(true);

        send("test-chat", "Hello!", None, &config, &storage, &output)
            .await
            .unwrap();

        let messages = storage.get_messages("test-chat", 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Hello!");
        assert!(messages[0].is_outgoing);
    }

    #[tokio::test]
    async fn test_read_messages() {
        let (_temp, config, storage, _) = setup();
        let output = Output::new(true);

        send("test-chat", "One", None, &config, &storage, &output)
            .await
            .unwrap();
        send("test-chat", "Two", None, &config, &storage, &output)
            .await
            .unwrap();

        read("test-chat", 10, &storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_send_updates_last_message_at() {
        let (_temp, config, storage, _) = setup();
        let output = Output::new(true);

        let before = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(before.last_message_at.is_none());

        send("test-chat", "Hello!", None, &config, &storage, &output)
            .await
            .unwrap();

        let after = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(after.last_message_at.is_some());
    }

    #[test]
    fn test_resolve_target_by_chat_id() {
        let (_temp, _config, storage, _) = setup();
        let chat = resolve_target("test-chat", &storage).unwrap();
        assert_eq!(chat.id, "test-chat");
    }

    #[test]
    fn test_resolve_target_by_hex_pubkey() {
        let (_temp, _config, storage, _) = setup();
        let keys = nostr::Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();

        // Create a chat with this pubkey
        let session = create_test_session();
        let session_state = serde_json::to_string(&session.state).unwrap();
        storage
            .save_chat(&StoredChat {
                id: "pk-chat".to_string(),
                their_pubkey: pubkey_hex.clone(),
                created_at: 1234567890,
                last_message_at: None,
                session_state,
            })
            .unwrap();

        let chat = resolve_target(&pubkey_hex, &storage).unwrap();
        assert_eq!(chat.id, "pk-chat");
    }

    #[test]
    fn test_resolve_target_by_npub() {
        let (_temp, _config, storage, _) = setup();
        let keys = nostr::Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();
        let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

        let session = create_test_session();
        let session_state = serde_json::to_string(&session.state).unwrap();
        storage
            .save_chat(&StoredChat {
                id: "npub-chat".to_string(),
                their_pubkey: pubkey_hex,
                created_at: 1234567890,
                last_message_at: None,
                session_state,
            })
            .unwrap();

        let chat = resolve_target(&npub, &storage).unwrap();
        assert_eq!(chat.id, "npub-chat");
    }

    #[test]
    fn test_resolve_target_not_found() {
        let (_temp, _config, storage, _) = setup();
        assert!(resolve_target("nonexistent", &storage).is_err());
    }

    #[test]
    fn test_resolve_target_prefers_recent() {
        let (_temp, _config, storage, _) = setup();
        let keys = nostr::Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();

        let session1 = create_test_session();
        let session2 = create_test_session();
        storage
            .save_chat(&StoredChat {
                id: "old-chat".to_string(),
                their_pubkey: pubkey_hex.clone(),
                created_at: 1000,
                last_message_at: Some(2000),
                session_state: serde_json::to_string(&session1.state).unwrap(),
            })
            .unwrap();
        storage
            .save_chat(&StoredChat {
                id: "new-chat".to_string(),
                their_pubkey: pubkey_hex.clone(),
                created_at: 1000,
                last_message_at: Some(5000),
                session_state: serde_json::to_string(&session2.state).unwrap(),
            })
            .unwrap();

        let chat = resolve_target(&pubkey_hex, &storage).unwrap();
        assert_eq!(chat.id, "new-chat");
    }

    #[test]
    fn test_resolve_target_by_petname() {
        let (_temp, _config, storage, _) = setup();
        let keys = nostr::Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();
        let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

        let session = create_test_session();
        let session_state = serde_json::to_string(&session.state).unwrap();
        storage
            .save_chat(&StoredChat {
                id: "pet-chat".to_string(),
                their_pubkey: pubkey_hex,
                created_at: 1234567890,
                last_message_at: None,
                session_state,
            })
            .unwrap();

        storage.add_contact(&npub, "alice").unwrap();
        let chat = resolve_target("alice", &storage).unwrap();
        assert_eq!(chat.id, "pet-chat");
    }
}
