use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

fn temp_path_for(path: &Path) -> PathBuf {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.is_empty() {
        return path.with_extension("tmp");
    }
    path.with_extension(format!("{ext}.tmp"))
}

fn write_string_atomic(path: &Path, content: &str) -> Result<()> {
    let temp_path = temp_path_for(path);
    fs::write(&temp_path, content)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn write_json_pretty_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    write_string_atomic(path, &content)
}

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
    /// Peer device identity this session is bound to (if known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub created_at: u64,
    pub last_message_at: Option<u64>,
    pub session_state: String,
    /// Default disappearing-message TTL for this chat (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ttl_seconds: Option<u64>,
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
    /// UNIX timestamp (seconds) when the message should be considered expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

/// Stored reaction data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredReaction {
    pub id: String,
    pub chat_id: String,
    pub message_id: String,
    pub from_pubkey: String,
    pub emoji: String,
    pub timestamp: u64,
    pub is_outgoing: bool,
}

/// Stored group data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroup {
    #[serde(flatten)]
    pub data: nostr_double_ratchet::group::GroupData,
}

/// Stored group message data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroupMessage {
    pub id: String,
    pub group_id: String,
    pub sender_pubkey: String,
    pub content: String,
    pub timestamp: u64,
    pub is_outgoing: bool,
    /// UNIX timestamp (seconds) when the message should be considered expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

/// Stored Signal-style sender keys per group/sender.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroupSenderKeys {
    pub group_id: String,
    pub sender_pubkey: String,
    pub states: Vec<nostr_double_ratchet::SenderKeyState>,
}

/// Stored mapping from a group member's identity pubkey to their per-group outer "sender event" pubkey.
///
/// For our own identity, we also store the private key so we can publish events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroupSender {
    pub group_id: String,
    /// The member's device identity pubkey (hex). (Stable per device.)
    pub identity_pubkey: String,
    /// The member's owner pubkey (hex). When missing, assume `identity_pubkey` (single-device).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pubkey: Option<String>,
    /// The per-group outer pubkey used to author group message events (hex).
    pub sender_event_pubkey: String,
    /// Only present for our own identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_event_secret_key: Option<String>,
}

/// File-based storage (agent-friendly - can read JSON directly)
pub struct Storage {
    #[allow(dead_code)]
    base_dir: PathBuf,
    invites_dir: PathBuf,
    chats_dir: PathBuf,
    messages_dir: PathBuf,
    reactions_dir: PathBuf,
    groups_dir: PathBuf,
    group_messages_dir: PathBuf,
    group_sender_keys_dir: PathBuf,
    group_senders_dir: PathBuf,
}

impl Storage {
    /// Open the storage
    pub fn open(data_dir: &Path) -> Result<Self> {
        let base_dir = data_dir.to_path_buf();
        let invites_dir = base_dir.join("invites");
        let chats_dir = base_dir.join("chats");
        let messages_dir = base_dir.join("messages");
        let reactions_dir = base_dir.join("reactions");
        let groups_dir = base_dir.join("groups");
        let group_messages_dir = base_dir.join("group_messages");
        let group_sender_keys_dir = base_dir.join("group_sender_keys");
        let group_senders_dir = base_dir.join("group_senders");

        fs::create_dir_all(&invites_dir)?;
        fs::create_dir_all(&chats_dir)?;
        fs::create_dir_all(&messages_dir)?;
        fs::create_dir_all(&reactions_dir)?;
        fs::create_dir_all(&groups_dir)?;
        fs::create_dir_all(&group_messages_dir)?;
        fs::create_dir_all(&group_sender_keys_dir)?;
        fs::create_dir_all(&group_senders_dir)?;

        Ok(Self {
            base_dir,
            invites_dir,
            chats_dir,
            messages_dir,
            reactions_dir,
            groups_dir,
            group_messages_dir,
            group_sender_keys_dir,
            group_senders_dir,
        })
    }

    // === Multi-device (AppKeys) operations ===

    fn app_keys_path(&self) -> PathBuf {
        self.base_dir.join("app_keys.json")
    }

    pub fn load_app_keys(&self) -> Result<Option<nostr_double_ratchet::AppKeys>> {
        let path = self.app_keys_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        match nostr_double_ratchet::AppKeys::deserialize(&content) {
            Ok(keys) => Ok(Some(keys)),
            Err(_) => Ok(None),
        }
    }

    pub fn save_app_keys(&self, keys: &nostr_double_ratchet::AppKeys) -> Result<()> {
        let path = self.app_keys_path();
        let json = keys.serialize()?;
        let content = match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => serde_json::to_string_pretty(&v)?,
            Err(_) => json,
        };
        write_string_atomic(&path, &content)
    }

    // === Invite operations ===

    pub fn save_invite(&self, invite: &StoredInvite) -> Result<()> {
        let path = self.invites_dir.join(format!("{}.json", invite.id));
        write_json_pretty_atomic(&path, invite)
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
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
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
        write_json_pretty_atomic(&path, chat)
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
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
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

        // Atomic write: write to temp file then rename to avoid corruption on crash
        write_string_atomic(&path, &content)
    }

    pub fn get_messages(&self, chat_id: &str, limit: usize) -> Result<Vec<StoredMessage>> {
        let dir = self.chat_messages_dir(chat_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        // Collect all day files, sorted by date descending (newest first)
        let mut day_files: Vec<_> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .collect();
        day_files.sort_by_key(|entry| std::cmp::Reverse(entry.path()));

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

    /// Remove expired messages (based on `expires_at`) from on-disk storage.
    ///
    /// Returns the number of messages removed.
    pub fn purge_expired_messages(&self, chat_id: &str, now_seconds: u64) -> Result<usize> {
        let dir = self.chat_messages_dir(chat_id);
        if !dir.exists() {
            return Ok(0);
        }

        let mut removed = 0usize;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            let day_messages: Vec<StoredMessage> = serde_json::from_str(&content)?;

            let mut kept: Vec<StoredMessage> = Vec::with_capacity(day_messages.len());
            for m in day_messages {
                if m.expires_at.is_some_and(|ts| ts <= now_seconds) {
                    removed += 1;
                    continue;
                }
                kept.push(m);
            }

            if kept.is_empty() {
                // Remove empty day file.
                let _ = fs::remove_file(&path);
                continue;
            }

            // Rewrite only when we removed something.
            if kept.len() < kept.capacity() {
                // Keep sort order stable.
                kept.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                let updated = serde_json::to_string_pretty(&kept)?;
                write_string_atomic(&path, &updated)?;
            }
        }

        Ok(removed)
    }

    // === Reaction operations ===
    // Reactions are stored per chat: reactions/<chat_id>.json

    pub fn save_reaction(&self, reaction: &StoredReaction) -> Result<()> {
        let path = self
            .reactions_dir
            .join(format!("{}.json", reaction.chat_id));

        // Load existing reactions for this chat, or start fresh
        let mut reactions: Vec<StoredReaction> = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            Vec::new()
        };

        // Remove any existing reaction with same id (update case)
        reactions.retain(|r| r.id != reaction.id);
        reactions.push(reaction.clone());

        // Sort by timestamp
        reactions.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        let content = serde_json::to_string_pretty(&reactions)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn get_reactions(&self, chat_id: &str, limit: usize) -> Result<Vec<StoredReaction>> {
        let path = self.reactions_dir.join(format!("{}.json", chat_id));
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)?;
        let mut reactions: Vec<StoredReaction> = serde_json::from_str(&content)?;

        // Sort by timestamp descending, take limit, reverse
        reactions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        reactions.truncate(limit);
        reactions.reverse();

        Ok(reactions)
    }

    // === Group message operations ===
    // Group messages stored at group_messages/<group_id>/<date>.json

    fn group_messages_path(&self, group_id: &str) -> PathBuf {
        self.group_messages_dir.join(group_id)
    }

    pub fn save_group_message(&self, msg: &StoredGroupMessage) -> Result<()> {
        let dir = self.group_messages_path(&msg.group_id);
        fs::create_dir_all(&dir)?;

        let date = Self::date_from_timestamp(msg.timestamp);
        let path = dir.join(format!("{}.json", date));

        let mut day_messages: Vec<StoredGroupMessage> = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            Vec::new()
        };

        day_messages.retain(|m| m.id != msg.id);
        day_messages.push(msg.clone());
        day_messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        let content = serde_json::to_string_pretty(&day_messages)?;
        write_string_atomic(&path, &content)
    }

    pub fn get_group_messages(
        &self,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredGroupMessage>> {
        let dir = self.group_messages_path(group_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut day_files: Vec<_> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .collect();
        day_files.sort_by_key(|entry| std::cmp::Reverse(entry.path()));

        let mut messages: Vec<StoredGroupMessage> = Vec::new();
        for entry in day_files {
            let content = fs::read_to_string(entry.path())?;
            let day_messages: Vec<StoredGroupMessage> = serde_json::from_str(&content)?;
            messages.extend(day_messages);
            if messages.len() >= limit {
                break;
            }
        }

        messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        messages.truncate(limit);
        messages.reverse();
        Ok(messages)
    }

    /// Remove expired group messages (based on `expires_at`) from on-disk storage.
    ///
    /// Returns the number of messages removed.
    pub fn purge_expired_group_messages(&self, group_id: &str, now_seconds: u64) -> Result<usize> {
        let dir = self.group_messages_path(group_id);
        if !dir.exists() {
            return Ok(0);
        }

        let mut removed = 0usize;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
                continue;
            }

            let content = fs::read_to_string(&path)?;
            let day_messages: Vec<StoredGroupMessage> = serde_json::from_str(&content)?;

            let mut kept: Vec<StoredGroupMessage> = Vec::with_capacity(day_messages.len());
            for m in day_messages {
                if m.expires_at.is_some_and(|ts| ts <= now_seconds) {
                    removed += 1;
                    continue;
                }
                kept.push(m);
            }

            if kept.is_empty() {
                let _ = fs::remove_file(&path);
                continue;
            }

            if kept.len() < kept.capacity() {
                kept.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                let updated = serde_json::to_string_pretty(&kept)?;
                write_string_atomic(&path, &updated)?;
            }
        }

        Ok(removed)
    }

    // === Group operations ===

    pub fn save_group(&self, group: &StoredGroup) -> Result<()> {
        let path = self.groups_dir.join(format!("{}.json", group.data.id));
        write_json_pretty_atomic(&path, group)
    }

    pub fn get_group(&self, id: &str) -> Result<Option<StoredGroup>> {
        let path = self.groups_dir.join(format!("{}.json", id));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn list_groups(&self) -> Result<Vec<StoredGroup>> {
        let mut groups: Vec<StoredGroup> = Vec::new();
        for entry in fs::read_dir(&self.groups_dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = fs::read_to_string(entry.path())?;
                groups.push(serde_json::from_str(&content)?);
            }
        }
        groups.sort_by(|a, b| b.data.created_at.cmp(&a.data.created_at));
        Ok(groups)
    }

    pub fn delete_group(&self, id: &str) -> Result<bool> {
        let path = self.groups_dir.join(format!("{}.json", id));
        let messages_path = self.group_messages_dir.join(id);
        let sender_keys_path = self.group_sender_keys_dir.join(id);
        let senders_path = self.group_senders_dir.join(id);
        let existed = path.exists();

        if path.exists() {
            fs::remove_file(path)?;
        }
        if messages_path.exists() {
            fs::remove_dir_all(messages_path)?;
        }
        if sender_keys_path.exists() {
            fs::remove_dir_all(sender_keys_path)?;
        }
        if senders_path.exists() {
            fs::remove_dir_all(senders_path)?;
        }

        Ok(existed)
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
        if self.reactions_dir.exists() {
            fs::remove_dir_all(&self.reactions_dir)?;
        }
        if self.groups_dir.exists() {
            fs::remove_dir_all(&self.groups_dir)?;
        }
        if self.group_messages_dir.exists() {
            fs::remove_dir_all(&self.group_messages_dir)?;
        }
        if self.group_sender_keys_dir.exists() {
            fs::remove_dir_all(&self.group_sender_keys_dir)?;
        }
        if self.group_senders_dir.exists() {
            fs::remove_dir_all(&self.group_senders_dir)?;
        }

        // Recreate dirs
        fs::create_dir_all(&self.invites_dir)?;
        fs::create_dir_all(&self.chats_dir)?;
        fs::create_dir_all(&self.messages_dir)?;
        fs::create_dir_all(&self.reactions_dir)?;
        fs::create_dir_all(&self.groups_dir)?;
        fs::create_dir_all(&self.group_messages_dir)?;
        fs::create_dir_all(&self.group_sender_keys_dir)?;
        fs::create_dir_all(&self.group_senders_dir)?;

        Ok(())
    }

    // === Group sender (outer pubkey) operations ===

    fn group_sender_path(&self, group_id: &str, identity_pubkey: &str) -> PathBuf {
        self.group_senders_dir
            .join(group_id)
            .join(format!("{}.json", identity_pubkey))
    }

    pub fn get_group_sender(
        &self,
        group_id: &str,
        identity_pubkey: &str,
    ) -> Result<Option<StoredGroupSender>> {
        let path = self.group_sender_path(group_id, identity_pubkey);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn upsert_group_sender(&self, sender: &StoredGroupSender) -> Result<()> {
        let dir = self.group_senders_dir.join(&sender.group_id);
        fs::create_dir_all(&dir)?;

        let path = self.group_sender_path(&sender.group_id, &sender.identity_pubkey);
        write_json_pretty_atomic(&path, sender)
    }

    pub fn list_group_senders(&self, group_id: &str) -> Result<Vec<StoredGroupSender>> {
        let dir = self.group_senders_dir.join(group_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = fs::read_to_string(entry.path())?;
                if let Ok(sender) = serde_json::from_str::<StoredGroupSender>(&content) {
                    out.push(sender);
                }
            }
        }
        out.sort_by(|a, b| a.identity_pubkey.cmp(&b.identity_pubkey));
        Ok(out)
    }

    // === Group sender key operations ===

    fn group_sender_keys_path(&self, group_id: &str, sender_pubkey: &str) -> PathBuf {
        self.group_sender_keys_dir
            .join(group_id)
            .join(format!("{}.json", sender_pubkey))
    }

    pub fn delete_group_sender_keys(&self, group_id: &str, sender_pubkey: &str) -> Result<bool> {
        let path = self.group_sender_keys_path(group_id, sender_pubkey);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    pub fn get_group_sender_keys(
        &self,
        group_id: &str,
        sender_pubkey: &str,
    ) -> Result<Vec<nostr_double_ratchet::SenderKeyState>> {
        let path = self.group_sender_keys_path(group_id, sender_pubkey);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        let stored: StoredGroupSenderKeys = serde_json::from_str(&content)?;
        Ok(stored.states)
    }

    pub fn get_group_sender_key_state(
        &self,
        group_id: &str,
        sender_pubkey: &str,
        key_id: u32,
    ) -> Result<Option<nostr_double_ratchet::SenderKeyState>> {
        let states = self.get_group_sender_keys(group_id, sender_pubkey)?;
        Ok(states.into_iter().find(|s| s.key_id == key_id))
    }

    pub fn get_latest_group_sender_key_state(
        &self,
        group_id: &str,
        sender_pubkey: &str,
    ) -> Result<Option<nostr_double_ratchet::SenderKeyState>> {
        let states = self.get_group_sender_keys(group_id, sender_pubkey)?;
        Ok(states.into_iter().last())
    }

    pub fn upsert_group_sender_key_state(
        &self,
        group_id: &str,
        sender_pubkey: &str,
        state: &nostr_double_ratchet::SenderKeyState,
    ) -> Result<()> {
        let dir = self.group_sender_keys_dir.join(group_id);
        fs::create_dir_all(&dir)?;

        let path = self.group_sender_keys_path(group_id, sender_pubkey);

        let mut stored = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str::<StoredGroupSenderKeys>(&content)?
        } else {
            StoredGroupSenderKeys {
                group_id: group_id.to_string(),
                sender_pubkey: sender_pubkey.to_string(),
                states: Vec::new(),
            }
        };

        if let Some(idx) = stored.states.iter().position(|s| s.key_id == state.key_id) {
            stored.states[idx] = state.clone();
        } else {
            stored.states.push(state.clone());
        }

        // Keep only a small number of old sender keys per sender (rotation history).
        const MAX_KEYS_PER_SENDER: usize = 5;
        if stored.states.len() > MAX_KEYS_PER_SENDER {
            let drain = stored.states.len() - MAX_KEYS_PER_SENDER;
            stored.states.drain(0..drain);
        }

        let content = serde_json::to_string_pretty(&stored)?;
        write_string_atomic(&path, &content)
    }

    /// Find a chat by the peer's pubkey (hex). If multiple, returns the most recently active.
    pub fn get_chat_by_pubkey(&self, pubkey_hex: &str) -> Result<Option<StoredChat>> {
        let chats = self.list_chats()?;
        Ok(chats
            .into_iter()
            .filter(|c| c.their_pubkey == pubkey_hex)
            .max_by_key(|c| c.last_message_at.unwrap_or(c.created_at)))
    }

    // === Contact operations ===
    // Contacts file: plain text, one per line: `npub1... petname`
    // Lines starting with # are comments, blank lines are ignored.

    fn contacts_path(&self) -> PathBuf {
        self.base_dir.join("contacts")
    }

    pub fn get_contact_pubkey(&self, name: &str) -> Result<Option<String>> {
        let path = self.contacts_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let npub = parts.next().unwrap_or("");
            let petname = parts.next().unwrap_or("").trim();
            if petname.eq_ignore_ascii_case(name) {
                // Decode npub to hex
                use nostr::FromBech32;
                if let Ok(pk) = nostr::PublicKey::from_bech32(npub) {
                    return Ok(Some(pk.to_hex()));
                }
            }
        }
        Ok(None)
    }

    pub fn add_contact(&self, npub: &str, name: &str) -> Result<()> {
        // Validate npub
        use nostr::FromBech32;
        nostr::PublicKey::from_bech32(npub)
            .map_err(|_| anyhow::anyhow!("Invalid npub: {}", npub))?;

        // Remove existing entry for this name or npub
        self.remove_contact_by_name_or_npub(name, npub)?;

        let path = self.contacts_path();
        let mut content = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("{} {}\n", npub, name));
        fs::write(&path, &content)?;
        Ok(())
    }

    pub fn remove_contact(&self, name: &str) -> Result<bool> {
        let path = self.contacts_path();
        if !path.exists() {
            return Ok(false);
        }
        let content = fs::read_to_string(&path)?;
        let mut found = false;
        let filtered: Vec<&str> = content
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return true;
                }
                let mut parts = trimmed.splitn(2, char::is_whitespace);
                let _npub = parts.next().unwrap_or("");
                let petname = parts.next().unwrap_or("").trim();
                if petname.eq_ignore_ascii_case(name) {
                    found = true;
                    false
                } else {
                    true
                }
            })
            .collect();
        if found {
            let mut out = filtered.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            fs::write(&path, &out)?;
        }
        Ok(found)
    }

    fn remove_contact_by_name_or_npub(&self, name: &str, npub: &str) -> Result<()> {
        let path = self.contacts_path();
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&path)?;
        let filtered: Vec<&str> = content
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return true;
                }
                let mut parts = trimmed.splitn(2, char::is_whitespace);
                let line_npub = parts.next().unwrap_or("");
                let petname = parts.next().unwrap_or("").trim();
                !(petname.eq_ignore_ascii_case(name) || line_npub == npub)
            })
            .collect();
        let mut out = filtered.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        fs::write(&path, &out)?;
        Ok(())
    }

    pub fn list_contacts(&self) -> Result<Vec<(String, String)>> {
        let path = self.contacts_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)?;
        let mut contacts = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let npub = parts.next().unwrap_or("").to_string();
            let name = parts.next().unwrap_or("").trim().to_string();
            if !npub.is_empty() && !name.is_empty() {
                contacts.push((npub, name));
            }
        }
        Ok(contacts)
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
    fn test_app_keys_save_and_load() {
        let (_temp, storage) = test_storage();

        let owner_keys = nostr::Keys::generate();
        let device_keys = nostr::Keys::generate();

        let app_keys = nostr_double_ratchet::AppKeys::new(vec![
            nostr_double_ratchet::DeviceEntry::new(owner_keys.public_key(), 1),
            nostr_double_ratchet::DeviceEntry::new(device_keys.public_key(), 2),
        ]);

        storage.save_app_keys(&app_keys).unwrap();

        let loaded = storage.load_app_keys().unwrap().expect("expected app keys");
        let devices = loaded.get_all_devices();
        assert_eq!(devices.len(), 2);
        assert!(devices
            .iter()
            .any(|d| d.identity_pubkey == owner_keys.public_key()));
        assert!(devices
            .iter()
            .any(|d| d.identity_pubkey == device_keys.public_key()));
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
            device_id: None,
            created_at: 1234567890,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        };

        storage.save_chat(&chat).unwrap();
        let loaded = storage.get_chat("chat-1").unwrap().unwrap();
        assert_eq!(loaded.their_pubkey, "abc123");
        assert_eq!(loaded.device_id, None);

        let chats = storage.list_chats().unwrap();
        assert_eq!(chats.len(), 1);
    }

    #[test]
    fn test_chat_device_id_roundtrip() {
        let (_temp, storage) = test_storage();

        let chat = StoredChat {
            id: "chat-device".to_string(),
            their_pubkey: "owner-pubkey".to_string(),
            device_id: Some("device-pubkey".to_string()),
            created_at: 1234567890,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        };

        storage.save_chat(&chat).unwrap();
        let loaded = storage.get_chat("chat-device").unwrap().unwrap();
        assert_eq!(loaded.device_id, Some("device-pubkey".to_string()));
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
                expires_at: None,
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
            device_id: None,
            created_at: 0,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        };
        storage.save_chat(&chat).unwrap();

        let msg = StoredMessage {
            id: "msg-1".to_string(),
            chat_id: "chat-1".to_string(),
            from_pubkey: "abc".to_string(),
            content: "Hello".to_string(),
            timestamp: 1704067200, // 2024-01-01 00:00:00 UTC
            is_outgoing: false,
            expires_at: None,
        };
        storage.save_message(&msg).unwrap();

        storage.delete_chat("chat-1").unwrap();
        assert!(storage.get_messages("chat-1", 100).unwrap().is_empty());
    }

    #[test]
    fn test_clear_all() {
        let (_temp, storage) = test_storage();

        storage
            .save_invite(&StoredInvite {
                id: "i".to_string(),
                label: None,
                url: "".to_string(),
                created_at: 0,
                serialized: "".to_string(),
            })
            .unwrap();

        storage.clear_all().unwrap();
        assert!(storage.list_invites().unwrap().is_empty());
    }

    #[test]
    fn test_contacts_crud() {
        let (_temp, storage) = test_storage();
        let keys = nostr::Keys::generate();
        let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();
        let hex = keys.public_key().to_hex();

        // Add
        storage.add_contact(&npub, "alice").unwrap();
        let contacts = storage.list_contacts().unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].1, "alice");

        // Lookup
        let found = storage.get_contact_pubkey("alice").unwrap().unwrap();
        assert_eq!(found, hex);

        // Not found
        assert!(storage.get_contact_pubkey("bob").unwrap().is_none());

        // Remove
        assert!(storage.remove_contact("alice").unwrap());
        assert!(storage.list_contacts().unwrap().is_empty());
        assert!(!storage.remove_contact("alice").unwrap());
    }

    #[test]
    fn test_contact_file_format() {
        let (temp, storage) = test_storage();
        let keys1 = nostr::Keys::generate();
        let keys2 = nostr::Keys::generate();
        let npub1 = nostr::ToBech32::to_bech32(&keys1.public_key()).unwrap();
        let npub2 = nostr::ToBech32::to_bech32(&keys2.public_key()).unwrap();

        storage.add_contact(&npub1, "alice").unwrap();
        storage.add_contact(&npub2, "bob").unwrap();

        let content = fs::read_to_string(temp.path().join("contacts")).unwrap();
        assert!(content.contains("alice"));
        assert!(content.contains("bob"));
        // Each line is: npub1... name
        for line in content.lines() {
            if !line.trim().is_empty() {
                assert!(line.starts_with("npub1"));
            }
        }
    }

    #[test]
    fn test_contact_dedup_on_add() {
        let (_temp, storage) = test_storage();
        let keys = nostr::Keys::generate();
        let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

        storage.add_contact(&npub, "alice").unwrap();
        storage.add_contact(&npub, "alice").unwrap();
        assert_eq!(storage.list_contacts().unwrap().len(), 1);

        // Re-add with different name replaces
        storage.add_contact(&npub, "bob").unwrap();
        let contacts = storage.list_contacts().unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].1, "bob");
    }

    #[test]
    fn test_get_chat_by_pubkey() {
        let (_temp, storage) = test_storage();

        storage
            .save_chat(&StoredChat {
                id: "c1".to_string(),
                their_pubkey: "aabbcc".to_string(),
                device_id: None,
                created_at: 1000,
                last_message_at: Some(2000),
                session_state: "{}".to_string(),
                message_ttl_seconds: None,
            })
            .unwrap();

        storage
            .save_chat(&StoredChat {
                id: "c2".to_string(),
                their_pubkey: "aabbcc".to_string(),
                device_id: None,
                created_at: 1000,
                last_message_at: Some(5000),
                session_state: "{}".to_string(),
                message_ttl_seconds: None,
            })
            .unwrap();

        // Should return the most recent
        let chat = storage.get_chat_by_pubkey("aabbcc").unwrap().unwrap();
        assert_eq!(chat.id, "c2");

        // Not found
        assert!(storage.get_chat_by_pubkey("zzz").unwrap().is_none());
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
    fn test_group_message_crud() {
        let (_temp, storage) = test_storage();

        let base_ts = 1704067200u64;
        for i in 0..5 {
            let msg = StoredGroupMessage {
                id: format!("gmsg-{}", i),
                group_id: "group-1".to_string(),
                sender_pubkey: "sender".to_string(),
                content: format!("Group message {}", i),
                timestamp: base_ts + i as u64 * 60,
                is_outgoing: i % 2 == 0,
                expires_at: None,
            };
            storage.save_group_message(&msg).unwrap();
        }

        let messages = storage.get_group_messages("group-1", 3).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "Group message 2");
        assert_eq!(messages[2].content, "Group message 4");
    }

    #[test]
    fn test_group_message_empty() {
        let (_temp, storage) = test_storage();
        let messages = storage.get_group_messages("nonexistent", 10).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_delete_group_cascades_messages() {
        let (_temp, storage) = test_storage();

        let group = StoredGroup {
            data: nostr_double_ratchet::group::GroupData {
                id: "g1".to_string(),
                name: "Test".to_string(),
                description: None,
                picture: None,
                members: vec!["a".to_string()],
                admins: vec!["a".to_string()],
                created_at: 1000,
                secret: None,
                accepted: Some(true),
            },
        };
        storage.save_group(&group).unwrap();

        let msg = StoredGroupMessage {
            id: "gm1".to_string(),
            group_id: "g1".to_string(),
            sender_pubkey: "a".to_string(),
            content: "Hello".to_string(),
            timestamp: 1704067200,
            is_outgoing: true,
            expires_at: None,
        };
        storage.save_group_message(&msg).unwrap();

        storage.delete_group("g1").unwrap();
        assert!(storage.get_group_messages("g1", 100).unwrap().is_empty());
    }

    #[test]
    fn test_group_message_dedup() {
        let (_temp, storage) = test_storage();

        let msg = StoredGroupMessage {
            id: "gm1".to_string(),
            group_id: "g1".to_string(),
            sender_pubkey: "a".to_string(),
            content: "v1".to_string(),
            timestamp: 1704067200,
            is_outgoing: true,
            expires_at: None,
        };
        storage.save_group_message(&msg).unwrap();

        let msg2 = StoredGroupMessage {
            content: "v2".to_string(),
            ..msg.clone()
        };
        storage.save_group_message(&msg2).unwrap();

        let messages = storage.get_group_messages("g1", 100).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "v2");
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
                expires_at: None,
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
