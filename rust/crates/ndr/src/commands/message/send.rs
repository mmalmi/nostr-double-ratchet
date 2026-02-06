use anyhow::Result;

use nostr_double_ratchet::{Session, INVITE_EVENT_KIND};

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
use crate::output::Output;
use crate::storage::{Storage, StoredChat, StoredMessage, StoredReaction};

use super::common::resolve_target_pubkey;
use super::resolve_target;
use super::types::{MessageInfo, MessageList, MessageSent, ReactionInfo};

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
    let client = connect_client(config).await?;
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

    let client = connect_client(config).await?;

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
    let client = connect_client(config).await?;
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
    let client = connect_client(config).await?;
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
    let client = connect_client(config).await?;
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
