use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredChat};

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
struct ChatJoined {
    id: String,
    their_pubkey: String,
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
pub async fn join(
    url: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    // Parse the invite URL
    let invite = nostr_double_ratchet::Invite::from_url(url)?;
    let their_pubkey = hex::encode(invite.inviter.to_bytes());

    // Accept the invite to create a session
    // TODO: Actually publish the acceptance and create the session
    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let chat = StoredChat {
        id: id.clone(),
        their_pubkey: their_pubkey.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state: "{}".to_string(), // TODO: Serialize actual session
    };

    storage.save_chat(&chat)?;

    output.success("chat.join", ChatJoined {
        id,
        their_pubkey,
    });

    Ok(())
}

/// Show chat details
pub async fn show(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    let chat = storage.get_chat(id)?
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
pub async fn delete(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    if storage.delete_chat(id)? {
        output.success_message("chat.delete", &format!("Deleted chat {}", id));
    } else {
        anyhow::bail!("Chat not found: {}", id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        config.set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
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
        let (_temp, _config, storage) = setup();
        let output = Output::new(true);

        // Add a chat manually
        storage.save_chat(&StoredChat {
            id: "test-chat".to_string(),
            their_pubkey: "abc123".to_string(),
            created_at: 1234567890,
            last_message_at: None,
            session_state: "{}".to_string(),
        }).unwrap();

        // List
        list(&storage, &output).await.unwrap();

        // Show
        show("test-chat", &storage, &output).await.unwrap();

        // Delete
        delete("test-chat", &storage, &output).await.unwrap();

        assert!(storage.list_chats().unwrap().is_empty());
    }
}
