use anyhow::Result;
use nostr_double_ratchet::Session;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredMessage};

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
    from_pubkey: String,
    content: String,
    timestamp: u64,
}

/// Send a message
pub async fn send(
    chat_id: &str,
    message: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let chat = storage.get_chat(chat_id)?
        .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", chat_id))?;

    // Load session state
    let session_state: nostr_double_ratchet::SessionState = serde_json::from_str(&chat.session_state)
        .map_err(|e| anyhow::anyhow!("Invalid session state: {}. Chat may not be properly initialized.", e))?;

    let mut session = Session::new(session_state, chat_id.to_string());

    // Encrypt the message
    let encrypted_event = session.send(message.to_string())
        .map_err(|e| anyhow::anyhow!("Failed to encrypt message: {}", e))?;

    let pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let msg_id = uuid::Uuid::new_v4().to_string();

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

    output.success("send", MessageSent {
        id: msg_id,
        chat_id: chat_id.to_string(),
        content: message.to_string(),
        timestamp,
        event: nostr::JsonUtil::as_json(&encrypted_event),
    });

    Ok(())
}

/// Read messages from a chat
pub async fn read(
    chat_id: &str,
    limit: usize,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let _ = storage.get_chat(chat_id)?
        .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", chat_id))?;

    let messages = storage.get_messages(chat_id, limit)?;

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

    output.success("read", MessageList {
        chat_id: chat_id.to_string(),
        messages: message_infos,
    });

    Ok(())
}

/// Receive and decrypt a message from a nostr event
pub async fn receive(
    event_json: &str,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    // Parse the nostr event
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json)
        .map_err(|e| anyhow::anyhow!("Invalid event JSON: {}", e))?;

    // Find the chat by looking at the event's pubkey tags or trying all chats
    // The sender's current key is in the event author field
    let sender_pubkey = event.pubkey;

    // Try to find a matching chat and decrypt
    let chats = storage.list_chats()?;

    for chat in chats {
        let session_state: nostr_double_ratchet::SessionState = match serde_json::from_str(&chat.session_state) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut session = Session::new(session_state, chat.id.clone());

        // Try to decrypt with this session
        match session.receive(&event) {
            Ok(Some(decrypted_event_json)) => {
                // The decrypted result is a nostr event JSON ("rumor"), extract its content
                let decrypted_event: serde_json::Value = serde_json::from_str(&decrypted_event_json)
                    .map_err(|e| anyhow::anyhow!("Failed to parse decrypted event: {}", e))?;

                let content = decrypted_event["content"]
                    .as_str()
                    .unwrap_or(&decrypted_event_json)
                    .to_string();

                let timestamp = event.created_at.as_u64();

                let msg_id = uuid::Uuid::new_v4().to_string();
                let stored = StoredMessage {
                    id: msg_id.clone(),
                    chat_id: chat.id.clone(),
                    from_pubkey: hex::encode(sender_pubkey.to_bytes()),
                    content: content.clone(),
                    timestamp,
                    is_outgoing: false,
                };

                storage.save_message(&stored)?;

                // Update session state
                let mut updated_chat = chat;
                updated_chat.last_message_at = Some(timestamp);
                updated_chat.session_state = serde_json::to_string(&session.state)?;
                storage.save_chat(&updated_chat)?;

                output.success("receive", IncomingMessage {
                    chat_id: updated_chat.id,
                    from_pubkey: hex::encode(sender_pubkey.to_bytes()),
                    content,
                    timestamp,
                });

                return Ok(());
            }
            Ok(None) => continue,
            Err(_) => continue,
        }
    }

    anyhow::bail!("Could not decrypt message - no matching session found");
}

/// Listen for new messages
pub async fn listen(
    chat_id: Option<&str>,
    config: &Config,
    _storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let scope = chat_id.map(|id| format!("chat {}", id))
        .unwrap_or_else(|| "all chats".to_string());

    output.success_message("listen", &format!("Listening for messages on {}... (Ctrl+C to stop)", scope));

    // TODO: Implement actual listening with nostr-sdk
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoredChat;
    use tempfile::TempDir;

    fn create_test_session() -> nostr_double_ratchet::Session {
        // Create an invite
        let alice_keys = nostr::Keys::generate();
        let bob_keys = nostr::Keys::generate();

        let invite = nostr_double_ratchet::Invite::create_new(
            alice_keys.public_key(),
            None,
            None,
        ).unwrap();

        // Bob accepts the invite - this creates a session where Bob can send
        let (bob_session, _response) = invite.accept(
            bob_keys.public_key(),
            bob_keys.secret_key().to_secret_bytes(),
            None,
        ).unwrap();

        bob_session
    }

    fn setup() -> (TempDir, Config, Storage, String) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        config.set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();

        // Create a proper test session
        let session = create_test_session();
        let session_state = serde_json::to_string(&session.state).unwrap();

        // Create a test chat with valid session
        storage.save_chat(&StoredChat {
            id: "test-chat".to_string(),
            their_pubkey: "abc123".to_string(),
            created_at: 1234567890,
            last_message_at: None,
            session_state: session_state.clone(),
        }).unwrap();

        (temp, config, storage, session_state)
    }

    #[tokio::test]
    async fn test_send_message() {
        let (_temp, config, storage, _) = setup();
        let output = Output::new(true);

        send("test-chat", "Hello!", &config, &storage, &output)
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

        send("test-chat", "One", &config, &storage, &output).await.unwrap();
        send("test-chat", "Two", &config, &storage, &output).await.unwrap();

        read("test-chat", 10, &storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_send_updates_last_message_at() {
        let (_temp, config, storage, _) = setup();
        let output = Output::new(true);

        let before = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(before.last_message_at.is_none());

        send("test-chat", "Hello!", &config, &storage, &output).await.unwrap();

        let after = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(after.last_message_at.is_some());
    }
}
