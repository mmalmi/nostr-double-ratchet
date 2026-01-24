use anyhow::Result;
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

#[allow(dead_code)]
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

    let pubkey = config.public_key()?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let msg_id = uuid::Uuid::new_v4().to_string();

    // TODO: Actually encrypt and send via nostr-double-ratchet

    let stored = StoredMessage {
        id: msg_id.clone(),
        chat_id: chat_id.to_string(),
        from_pubkey: pubkey,
        content: message.to_string(),
        timestamp,
        is_outgoing: true,
    };

    storage.save_message(&stored)?;

    // Update chat's last_message_at
    let mut updated_chat = chat;
    updated_chat.last_message_at = Some(timestamp);
    storage.save_chat(&updated_chat)?;

    output.success("send", MessageSent {
        id: msg_id,
        chat_id: chat_id.to_string(),
        content: message.to_string(),
        timestamp,
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

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        config.set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();

        // Create a test chat
        storage.save_chat(&StoredChat {
            id: "test-chat".to_string(),
            their_pubkey: "abc123".to_string(),
            created_at: 1234567890,
            last_message_at: None,
            session_state: "{}".to_string(),
        }).unwrap();

        (temp, config, storage)
    }

    #[tokio::test]
    async fn test_send_message() {
        let (_temp, config, storage) = setup();
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
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        send("test-chat", "One", &config, &storage, &output).await.unwrap();
        send("test-chat", "Two", &config, &storage, &output).await.unwrap();

        read("test-chat", 10, &storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_send_updates_last_message_at() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        let before = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(before.last_message_at.is_none());

        send("test-chat", "Hello!", &config, &storage, &output).await.unwrap();

        let after = storage.get_chat("test-chat").unwrap().unwrap();
        assert!(after.last_message_at.is_some());
    }
}
