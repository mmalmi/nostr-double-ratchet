use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Stored invite data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInvite {
    pub id: String,
    pub label: Option<String>,
    pub url: String,
    pub created_at: u64,
    pub serialized: String,
}

/// Stored chat data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChat {
    pub id: String,
    pub their_pubkey: String,
    pub created_at: u64,
    pub last_message_at: Option<u64>,
    pub session_state: String,
}

/// Stored message data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub chat_id: String,
    pub from_pubkey: String,
    pub content: String,
    pub timestamp: u64,
    pub is_outgoing: bool,
}

/// File-based storage (agent-friendly - can read JSON directly)
pub struct Storage {
    #[allow(dead_code)]
    base_dir: PathBuf,
    invites_dir: PathBuf,
    chats_dir: PathBuf,
    messages_dir: PathBuf,
}

impl Storage {
    /// Open the storage
    pub fn open(data_dir: &Path) -> Result<Self> {
        let base_dir = data_dir.to_path_buf();
        let invites_dir = base_dir.join("invites");
        let chats_dir = base_dir.join("chats");
        let messages_dir = base_dir.join("messages");

        fs::create_dir_all(&invites_dir)?;
        fs::create_dir_all(&chats_dir)?;
        fs::create_dir_all(&messages_dir)?;

        Ok(Self {
            base_dir,
            invites_dir,
            chats_dir,
            messages_dir,
        })
    }

    // === Invite operations ===

    pub fn save_invite(&self, invite: &StoredInvite) -> Result<()> {
        let path = self.invites_dir.join(format!("{}.json", invite.id));
        let content = serde_json::to_string_pretty(invite)?;
        fs::write(path, content)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_invite(&self, id: &str) -> Result<Option<StoredInvite>> {
        let path = self.invites_dir.join(format!("{}.json", id));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn list_invites(&self) -> Result<Vec<StoredInvite>> {
        let mut invites: Vec<StoredInvite> = Vec::new();
        for entry in fs::read_dir(&self.invites_dir)? {
            let entry = entry?;
            if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                let content = fs::read_to_string(entry.path())?;
                invites.push(serde_json::from_str(&content)?);
            }
        }
        invites.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(invites)
    }

    pub fn delete_invite(&self, id: &str) -> Result<bool> {
        let path = self.invites_dir.join(format!("{}.json", id));
        if path.exists() {
            fs::remove_file(path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // === Chat operations ===

    pub fn save_chat(&self, chat: &StoredChat) -> Result<()> {
        let path = self.chats_dir.join(format!("{}.json", chat.id));
        let content = serde_json::to_string_pretty(chat)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn get_chat(&self, id: &str) -> Result<Option<StoredChat>> {
        let path = self.chats_dir.join(format!("{}.json", id));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn list_chats(&self) -> Result<Vec<StoredChat>> {
        let mut chats: Vec<StoredChat> = Vec::new();
        for entry in fs::read_dir(&self.chats_dir)? {
            let entry = entry?;
            if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                let content = fs::read_to_string(entry.path())?;
                chats.push(serde_json::from_str(&content)?);
            }
        }
        chats.sort_by(|a, b| {
            let a_time = a.last_message_at.unwrap_or(a.created_at);
            let b_time = b.last_message_at.unwrap_or(b.created_at);
            b_time.cmp(&a_time)
        });
        Ok(chats)
    }

    pub fn delete_chat(&self, id: &str) -> Result<bool> {
        let chat_path = self.chats_dir.join(format!("{}.json", id));
        let messages_path = self.messages_dir.join(id);

        let existed = chat_path.exists();

        if chat_path.exists() {
            fs::remove_file(chat_path)?;
        }

        if messages_path.exists() {
            fs::remove_dir_all(messages_path)?;
        }

        Ok(existed)
    }

    // === Message operations ===
    // Messages are grouped by day: messages/<chat_id>/<date>.json
    // Each file contains an array of messages for that day

    fn chat_messages_dir(&self, chat_id: &str) -> PathBuf {
        self.messages_dir.join(chat_id)
    }

    fn date_from_timestamp(timestamp: u64) -> String {
        use std::time::{Duration, UNIX_EPOCH};
        let datetime = UNIX_EPOCH + Duration::from_secs(timestamp);
        let secs = datetime.duration_since(UNIX_EPOCH).unwrap().as_secs();
        // Simple date calculation (UTC)
        let days = secs / 86400;
        let year = 1970 + (days / 365); // Approximate, good enough for grouping
        let day_of_year = days % 365;
        let month = day_of_year / 30 + 1;
        let day = day_of_year % 30 + 1;
        format!("{:04}-{:02}-{:02}", year, month.min(12), day.min(31))
    }

    pub fn save_message(&self, message: &StoredMessage) -> Result<()> {
        let dir = self.chat_messages_dir(&message.chat_id);
        fs::create_dir_all(&dir)?;

        let date = Self::date_from_timestamp(message.timestamp);
        let path = dir.join(format!("{}.json", date));

        // Load existing messages for this day, or start fresh
        let mut day_messages: Vec<StoredMessage> = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            Vec::new()
        };

        // Remove any existing message with same id (update case)
        day_messages.retain(|m| m.id != message.id);
        day_messages.push(message.clone());

        // Sort by timestamp
        day_messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        let content = serde_json::to_string_pretty(&day_messages)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn get_messages(&self, chat_id: &str, limit: usize) -> Result<Vec<StoredMessage>> {
        let dir = self.chat_messages_dir(chat_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        // Collect all day files, sorted by date descending (newest first)
        let mut day_files: Vec<_> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "json").unwrap_or(false))
            .collect();
        day_files.sort_by(|a, b| b.path().cmp(&a.path()));

        let mut messages: Vec<StoredMessage> = Vec::new();
        for entry in day_files {
            let content = fs::read_to_string(entry.path())?;
            let day_messages: Vec<StoredMessage> = serde_json::from_str(&content)?;
            messages.extend(day_messages);

            // Early exit if we have enough
            if messages.len() >= limit {
                break;
            }
        }

        // Sort all collected messages by timestamp descending, take limit, reverse
        messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        messages.truncate(limit);
        messages.reverse();

        Ok(messages)
    }

    /// Clear all data (for logout)
    pub fn clear_all(&self) -> Result<()> {
        if self.invites_dir.exists() {
            fs::remove_dir_all(&self.invites_dir)?;
        }
        if self.chats_dir.exists() {
            fs::remove_dir_all(&self.chats_dir)?;
        }
        if self.messages_dir.exists() {
            fs::remove_dir_all(&self.messages_dir)?;
        }

        // Recreate dirs
        fs::create_dir_all(&self.invites_dir)?;
        fs::create_dir_all(&self.chats_dir)?;
        fs::create_dir_all(&self.messages_dir)?;

        Ok(())
    }

    /// Get the base data directory (for agents to find)
    #[allow(dead_code)]
    pub fn data_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage() -> (TempDir, Storage) {
        let temp = TempDir::new().unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        (temp, storage)
    }

    #[test]
    fn test_invite_crud() {
        let (_temp, storage) = test_storage();

        let invite = StoredInvite {
            id: "test-id".to_string(),
            label: Some("Test".to_string()),
            url: "https://example.com".to_string(),
            created_at: 1234567890,
            serialized: "{}".to_string(),
        };

        storage.save_invite(&invite).unwrap();

        let loaded = storage.get_invite("test-id").unwrap().unwrap();
        assert_eq!(loaded.id, "test-id");
        assert_eq!(loaded.label, Some("Test".to_string()));

        let invites = storage.list_invites().unwrap();
        assert_eq!(invites.len(), 1);

        assert!(storage.delete_invite("test-id").unwrap());
        assert!(storage.get_invite("test-id").unwrap().is_none());
    }

    #[test]
    fn test_chat_crud() {
        let (_temp, storage) = test_storage();

        let chat = StoredChat {
            id: "chat-1".to_string(),
            their_pubkey: "abc123".to_string(),
            created_at: 1234567890,
            last_message_at: None,
            session_state: "{}".to_string(),
        };

        storage.save_chat(&chat).unwrap();
        let loaded = storage.get_chat("chat-1").unwrap().unwrap();
        assert_eq!(loaded.their_pubkey, "abc123");

        let chats = storage.list_chats().unwrap();
        assert_eq!(chats.len(), 1);
    }

    #[test]
    fn test_message_storage() {
        let (_temp, storage) = test_storage();

        // Use timestamps that all fall on the same day
        let base_ts = 1704067200u64; // 2024-01-01 00:00:00 UTC

        for i in 0..5 {
            let msg = StoredMessage {
                id: format!("msg-{}", i),
                chat_id: "chat-1".to_string(),
                from_pubkey: "sender".to_string(),
                content: format!("Message {}", i),
                timestamp: base_ts + i as u64 * 60, // Each message 1 minute apart
                is_outgoing: i % 2 == 0,
            };
            storage.save_message(&msg).unwrap();
        }

        // Get last 3 messages
        let messages = storage.get_messages("chat-1", 3).unwrap();
        assert_eq!(messages.len(), 3);
        // Should be messages 2, 3, 4 in chronological order
        assert_eq!(messages[0].content, "Message 2");
        assert_eq!(messages[1].content, "Message 3");
        assert_eq!(messages[2].content, "Message 4");
    }

    #[test]
    fn test_delete_chat_cascades_messages() {
        let (_temp, storage) = test_storage();

        let chat = StoredChat {
            id: "chat-1".to_string(),
            their_pubkey: "abc".to_string(),
            created_at: 0,
            last_message_at: None,
            session_state: "{}".to_string(),
        };
        storage.save_chat(&chat).unwrap();

        let msg = StoredMessage {
            id: "msg-1".to_string(),
            chat_id: "chat-1".to_string(),
            from_pubkey: "abc".to_string(),
            content: "Hello".to_string(),
            timestamp: 1704067200, // 2024-01-01 00:00:00 UTC
            is_outgoing: false,
        };
        storage.save_message(&msg).unwrap();

        storage.delete_chat("chat-1").unwrap();
        assert!(storage.get_messages("chat-1", 100).unwrap().is_empty());
    }

    #[test]
    fn test_clear_all() {
        let (_temp, storage) = test_storage();

        storage.save_invite(&StoredInvite {
            id: "i".to_string(),
            label: None,
            url: "".to_string(),
            created_at: 0,
            serialized: "".to_string(),
        }).unwrap();

        storage.clear_all().unwrap();
        assert!(storage.list_invites().unwrap().is_empty());
    }

    #[test]
    fn test_json_files_are_readable() {
        let (temp, storage) = test_storage();

        let invite = StoredInvite {
            id: "readable".to_string(),
            label: Some("Test".to_string()),
            url: "https://example.com".to_string(),
            created_at: 1234567890,
            serialized: "{}".to_string(),
        };
        storage.save_invite(&invite).unwrap();

        // Verify the JSON file is human/agent readable
        let path = temp.path().join("invites/readable.json");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"id\": \"readable\""));
        assert!(content.contains("\"label\": \"Test\""));
    }

    #[test]
    fn test_messages_grouped_by_day() {
        let (temp, storage) = test_storage();

        // Save messages on same day
        let base_ts = 1704067200u64; // 2024-01-01 00:00:00 UTC
        for i in 0..3 {
            let msg = StoredMessage {
                id: format!("msg-{}", i),
                chat_id: "chat-1".to_string(),
                from_pubkey: "sender".to_string(),
                content: format!("Message {}", i),
                timestamp: base_ts + i as u64 * 60,
                is_outgoing: true,
            };
            storage.save_message(&msg).unwrap();
        }

        // All messages should be in one day file
        let chat_dir = temp.path().join("messages/chat-1");
        let files: Vec<_> = fs::read_dir(&chat_dir).unwrap().collect();
        assert_eq!(files.len(), 1);

        // File should contain array of 3 messages
        let content = fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        let messages: Vec<StoredMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 3);
    }
}
