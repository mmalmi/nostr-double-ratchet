use crate::{
    is_app_keys_event, AppKeys, InMemoryStorage, Invite, NostrPubSub, Result, StorageAdapter,
    UserRecord,
};
use nostr::{Keys, PublicKey, Tag, UnsignedEvent};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub enum SessionManagerEvent {
    Subscribe {
        subid: String,
        filter_json: String,
    },
    Unsubscribe(String),
    Publish(UnsignedEvent),
    PublishSigned(nostr::Event), // For events pre-signed with ephemeral keys (kind 1059, 1060)
    ReceivedEvent(nostr::Event),
    DecryptedMessage {
        sender: PublicKey,
        content: String,
        event_id: Option<String>,
    },
}

struct InviteState {
    invite: Invite,
    our_identity_key: [u8; 32],
}

pub struct SessionManager {
    user_records: Arc<Mutex<HashMap<PublicKey, UserRecord>>>,
    our_public_key: PublicKey,
    our_identity_key: [u8; 32],
    device_id: String,
    owner_public_key: PublicKey,
    storage: Arc<dyn StorageAdapter>,
    pubsub: Arc<dyn NostrPubSub>,
    initialized: Arc<Mutex<bool>>,
    invite_state: Arc<Mutex<Option<InviteState>>>,
    provided_invite: Option<Invite>,
    delegate_to_owner: Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    cached_app_keys: Arc<Mutex<HashMap<PublicKey, AppKeys>>>,
    processed_invite_responses: Arc<Mutex<HashSet<String>>>,
    message_history: Arc<Mutex<HashMap<PublicKey, Vec<UnsignedEvent>>>>,
    invite_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    app_keys_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    pending_acceptances: Arc<Mutex<HashSet<PublicKey>>>,
    default_send_options: Arc<Mutex<Option<crate::SendOptions>>>,
    peer_send_options: Arc<Mutex<HashMap<PublicKey, crate::SendOptions>>>,
    group_send_options: Arc<Mutex<HashMap<String, crate::SendOptions>>>,
    auto_adopt_chat_settings: Arc<Mutex<bool>>,
}

impl SessionManager {
    pub fn set_default_send_options(&self, options: Option<crate::SendOptions>) -> Result<()> {
        *self.default_send_options.lock().unwrap() = options.clone();

        let key = self.send_options_default_key();
        match options {
            Some(o) => self.storage.put(&key, serde_json::to_string(&o)?)?,
            None => {
                let _ = self.storage.del(&key);
            }
        }
        Ok(())
    }

    pub fn set_peer_send_options(
        &self,
        peer_pubkey: PublicKey,
        options: Option<crate::SendOptions>,
    ) -> Result<()> {
        let owner = self.resolve_to_owner(&peer_pubkey);
        let key = self.send_options_peer_key(&owner);

        if let Some(o) = options.clone() {
            self.peer_send_options
                .lock()
                .unwrap()
                .insert(owner, o.clone());
            self.storage.put(&key, serde_json::to_string(&o)?)?;
        } else {
            self.peer_send_options.lock().unwrap().remove(&owner);
            let _ = self.storage.del(&key);
        }
        Ok(())
    }

    pub fn set_group_send_options(
        &self,
        group_id: String,
        options: Option<crate::SendOptions>,
    ) -> Result<()> {
        let key = self.send_options_group_key(&group_id);

        if let Some(o) = options.clone() {
            self.group_send_options
                .lock()
                .unwrap()
                .insert(group_id.clone(), o.clone());
            self.storage.put(&key, serde_json::to_string(&o)?)?;
        } else {
            self.group_send_options.lock().unwrap().remove(&group_id);
            let _ = self.storage.del(&key);
        }
        Ok(())
    }

    /// Enable/disable automatically adopting incoming `chat-settings` events (kind 10448).
    ///
    /// When enabled, receiving a valid settings payload updates per-peer SendOptions.
    pub fn set_auto_adopt_chat_settings(&self, enabled: bool) {
        *self.auto_adopt_chat_settings.lock().unwrap() = enabled;
    }

    pub fn new(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        event_tx: crossbeam_channel::Sender<SessionManagerEvent>,
        storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(event_tx);
        Self::new_with_pubsub(
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            pubsub,
            storage,
            invite,
        )
    }

    pub fn new_with_pubsub(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        pubsub: Arc<dyn NostrPubSub>,
        storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        Self {
            user_records: Arc::new(Mutex::new(HashMap::new())),
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            storage: storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            pubsub,
            initialized: Arc::new(Mutex::new(false)),
            invite_state: Arc::new(Mutex::new(None)),
            provided_invite: invite,
            delegate_to_owner: Arc::new(Mutex::new(HashMap::new())),
            cached_app_keys: Arc::new(Mutex::new(HashMap::new())),
            processed_invite_responses: Arc::new(Mutex::new(HashSet::new())),
            message_history: Arc::new(Mutex::new(HashMap::new())),
            invite_subscriptions: Arc::new(Mutex::new(HashSet::new())),
            app_keys_subscriptions: Arc::new(Mutex::new(HashSet::new())),
            pending_acceptances: Arc::new(Mutex::new(HashSet::new())),
            default_send_options: Arc::new(Mutex::new(None)),
            peer_send_options: Arc::new(Mutex::new(HashMap::new())),
            group_send_options: Arc::new(Mutex::new(HashMap::new())),
            auto_adopt_chat_settings: Arc::new(Mutex::new(true)),
        }
    }

    pub fn init(&self) -> Result<()> {
        let mut initialized = self.initialized.lock().unwrap();
        if *initialized {
            return Ok(());
        }
        *initialized = true;
        drop(initialized);

        self.load_all_user_records()?;
        let _ = self.load_send_options();

        // Ensure our own device is present in our owner's record
        {
            let mut records = self.user_records.lock().unwrap();
            let record = records
                .entry(self.owner_public_key)
                .or_insert_with(|| UserRecord::new(hex::encode(self.owner_public_key.to_bytes())));
            self.upsert_device_record(record, &self.device_id);
        }

        let device_invite_key = self.device_invite_key(&self.device_id);
        let invite = if let Some(invite) = self.provided_invite.clone() {
            invite
        } else {
            match self.storage.get(&device_invite_key)? {
                Some(data) => Invite::deserialize(&data)?,
                None => {
                    Invite::create_new(self.our_public_key, Some(self.device_id.clone()), None)?
                }
            }
        };

        self.storage.put(&device_invite_key, invite.serialize()?)?;

        if invite.inviter_ephemeral_private_key.is_none() {
            return Err(crate::Error::Invite(
                "Invite missing ephemeral keys".to_string(),
            ));
        }

        *self.invite_state.lock().unwrap() = Some(InviteState {
            invite: invite.clone(),
            our_identity_key: self.our_identity_key,
        });

        // Subscribe to invite responses using Invite's own filter (with #p tag)
        invite.listen_with_pubsub(self.pubsub.as_ref())?;

        // Publish our invite (signed with device identity key)
        if let Ok(unsigned) = invite.get_event() {
            let keys = Keys::new(nostr::SecretKey::from_slice(&self.our_identity_key)?);
            if let Ok(signed) = unsigned.sign_with_keys(&keys) {
                let _ = self.pubsub.publish_signed(signed);
            }
        }

        // Sessions manage their own kind 1060 subscriptions
        let mut records = self.user_records.lock().unwrap();
        for user_record in records.values_mut() {
            for device_record in user_record.device_records.values_mut() {
                if let Some(ref mut session) = device_record.active_session {
                    session.set_pubsub(self.pubsub.clone());
                    let _ = session.subscribe_to_messages();
                }
                for session in &mut device_record.inactive_sessions {
                    session.set_pubsub(self.pubsub.clone());
                    let _ = session.subscribe_to_messages();
                }
            }
        }
        drop(records);

        // Start listening for AppKeys for our owner (to discover sibling devices)
        self.setup_user(self.owner_public_key);

        Ok(())
    }

    pub fn send_text(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<crate::SendOptions>,
    ) -> Result<Vec<String>> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }

        let (_, event_ids) = self.send_text_with_inner_id(recipient, text, options)?;
        Ok(event_ids)
    }

    #[deprecated(
        note = "use send_text(recipient, text, Some(SendOptions{ expires_at: Some(...) }))"
    )]
    pub fn send_text_with_expiration(
        &self,
        recipient: PublicKey,
        text: String,
        expires_at: u64,
    ) -> Result<Vec<String>> {
        self.send_text(
            recipient,
            text,
            Some(crate::SendOptions {
                expires_at: Some(expires_at),
                ttl_seconds: None,
            }),
        )
    }

    /// Send a chat message and return both its stable inner (rumor) id and the
    /// list of outer message event ids that were published.
    pub fn send_text_with_inner_id(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<crate::SendOptions>,
    ) -> Result<(String, Vec<String>)> {
        if text.trim().is_empty() {
            return Ok((String::new(), Vec::new()));
        }

        let owner = self.resolve_to_owner(&recipient);
        let options = self.effective_send_options(owner, None, options);
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut tags: Vec<Tag> = Vec::new();
        if let Some(expires_at) = crate::utils::resolve_expiration_seconds(&options, now_s)? {
            tags.push(
                Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let event = self.build_message_event(recipient, crate::CHAT_MESSAGE_KIND, text, tags)?;

        let inner_id = event
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();

        let event_ids = self.send_event(recipient, event)?;
        Ok((inner_id, event_ids))
    }

    /// Send an encrypted 1:1 chat settings event (inner kind 10448).
    ///
    /// Settings events themselves should never expire; they are sent without a NIP-40 expiration tag.
    pub fn send_chat_settings(
        &self,
        recipient: PublicKey,
        message_ttl_seconds: u64,
    ) -> Result<Vec<String>> {
        let payload = crate::ChatSettingsPayloadV1 {
            typ: "chat-settings".to_string(),
            v: 1,
            message_ttl_seconds: Some(message_ttl_seconds),
        };

        let content = serde_json::to_string(&payload)?;
        let event =
            self.build_message_event(recipient, crate::CHAT_SETTINGS_KIND, content, vec![])?;
        self.send_event(recipient, event)
    }

    /// Convenience: set per-peer disappearing-message TTL and notify the peer via a settings event.
    ///
    /// `message_ttl_seconds`:
    /// - `> 0`: set per-peer `ttl_seconds`
    /// - `== 0`: disable per-peer expiration even if a global default exists
    pub fn set_chat_settings_for_peer(
        &self,
        peer_pubkey: PublicKey,
        message_ttl_seconds: u64,
    ) -> Result<Vec<String>> {
        let opts = if message_ttl_seconds == 0 {
            crate::SendOptions::default()
        } else {
            crate::SendOptions {
                ttl_seconds: Some(message_ttl_seconds),
                expires_at: None,
            }
        };
        self.set_peer_send_options(peer_pubkey, Some(opts))?;
        self.send_chat_settings(peer_pubkey, message_ttl_seconds)
    }

    pub fn send_receipt(
        &self,
        recipient: PublicKey,
        receipt_type: &str,
        message_ids: Vec<String>,
        options: Option<crate::SendOptions>,
    ) -> Result<Vec<String>> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut tags: Vec<Tag> = Vec::new();
        for id in message_ids {
            tags.push(
                Tag::parse(&["e".to_string(), id])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let owner = self.resolve_to_owner(&recipient);
        let options = self.effective_send_options(owner, None, options);
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if let Some(expires_at) = crate::utils::resolve_expiration_seconds(&options, now_s)? {
            tags.push(
                Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let event = self.build_message_event(
            recipient,
            crate::RECEIPT_KIND,
            receipt_type.to_string(),
            tags,
        )?;

        self.send_event(recipient, event)
    }

    pub fn send_typing(
        &self,
        recipient: PublicKey,
        options: Option<crate::SendOptions>,
    ) -> Result<Vec<String>> {
        let owner = self.resolve_to_owner(&recipient);
        let options = self.effective_send_options(owner, None, options);
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut tags: Vec<Tag> = Vec::new();
        if let Some(expires_at) = crate::utils::resolve_expiration_seconds(&options, now_s)? {
            tags.push(
                Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let event =
            self.build_message_event(recipient, crate::TYPING_KIND, "typing".to_string(), tags)?;

        self.send_event(recipient, event)
    }

    /// Send an emoji reaction (kind 7) to a specific message id.
    ///
    /// `message_id` should typically be the *outer* Nostr event id of the target message
    /// (this is what other Iris clients expect for reactions).
    pub fn send_reaction(
        &self,
        recipient: PublicKey,
        message_id: String,
        emoji: String,
        options: Option<crate::SendOptions>,
    ) -> Result<Vec<String>> {
        if message_id.trim().is_empty() || emoji.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut tags: Vec<Tag> = Vec::new();
        tags.push(
            Tag::parse(&["e".to_string(), message_id])
                .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
        );

        let owner = self.resolve_to_owner(&recipient);
        let options = self.effective_send_options(owner, None, options);
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if let Some(expires_at) = crate::utils::resolve_expiration_seconds(&options, now_s)? {
            tags.push(
                Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let event = self.build_message_event(recipient, crate::REACTION_KIND, emoji, tags)?;

        self.send_event(recipient, event)
    }

    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }

    pub fn get_user_pubkeys(&self) -> Vec<PublicKey> {
        self.user_records.lock().unwrap().keys().copied().collect()
    }

    pub fn get_total_sessions(&self) -> usize {
        self.user_records
            .lock()
            .unwrap()
            .values()
            .map(|ur| {
                ur.device_records
                    .values()
                    .filter(|dr| dr.active_session.is_some())
                    .count()
            })
            .sum()
    }

    pub fn import_session_state(
        &self,
        peer_pubkey: PublicKey,
        device_id: Option<String>,
        state: crate::SessionState,
    ) -> Result<()> {
        let mut session = crate::Session::new(state, "imported".to_string());
        session.set_pubsub(self.pubsub.clone());
        let _ = session.subscribe_to_messages();

        let mut records = self.user_records.lock().unwrap();
        let user_record = records
            .entry(peer_pubkey)
            .or_insert_with(|| UserRecord::new(hex::encode(peer_pubkey.to_bytes())));
        user_record.upsert_session(device_id.as_deref(), session);
        drop(records);

        let _ = self.store_user_record(&peer_pubkey);
        Ok(())
    }

    pub fn export_active_session_state(
        &self,
        peer_pubkey: PublicKey,
    ) -> Result<Option<crate::SessionState>> {
        let mut records = self.user_records.lock().unwrap();
        let user_record = match records.get_mut(&peer_pubkey) {
            Some(record) => record,
            None => return Ok(None),
        };

        let mut sessions = user_record.get_active_sessions_mut();
        if let Some(session) = sessions.first_mut() {
            return Ok(Some(session.state.clone()));
        }

        Ok(None)
    }

    pub fn debug_session_keys(&self) -> String {
        let records = self.user_records.lock().unwrap();
        let mut output = String::new();

        for (user_pk, user_record) in records.iter() {
            for (device_id, device_record) in &user_record.device_records {
                if let Some(ref session) = device_record.active_session {
                    output.push_str(&format!(
                        "Session with {}[{}]:\n",
                        &hex::encode(user_pk.to_bytes())[..16],
                        device_id
                    ));
                    if let Some(our_current) = &session.state.our_current_nostr_key {
                        output.push_str(&format!(
                            "  our_current:    {}\n",
                            &hex::encode(our_current.public_key.to_bytes())[..16]
                        ));
                    } else {
                        output.push_str("  our_current:    None\n");
                    }
                    output.push_str(&format!(
                        "  our_next:       {}\n",
                        &hex::encode(session.state.our_next_nostr_key.public_key.to_bytes())[..16]
                    ));
                    if let Some(their_current) = session.state.their_current_nostr_public_key {
                        output.push_str(&format!(
                            "  their_current:  {}\n",
                            &hex::encode(their_current.to_bytes())[..16]
                        ));
                    } else {
                        output.push_str("  their_current:  None\n");
                    }
                    if let Some(their_next) = session.state.their_next_nostr_public_key {
                        output.push_str(&format!(
                            "  their_next:     {}\n",
                            &hex::encode(their_next.to_bytes())[..16]
                        ));
                    } else {
                        output.push_str("  their_next:     None\n");
                    }
                }
            }
        }
        output
    }

    pub fn get_our_pubkey(&self) -> PublicKey {
        self.our_public_key
    }

    pub fn get_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key
    }

    fn build_message_event(
        &self,
        recipient: PublicKey,
        kind: u32,
        content: String,
        mut extra_tags: Vec<Tag>,
    ) -> Result<UnsignedEvent> {
        let recipient_hex = hex::encode(recipient.to_bytes());
        let has_recipient_p_tag = extra_tags.iter().any(|t| {
            let v = t.clone().to_vec();
            v.first().map(|s| s.as_str()) == Some("p")
                && v.get(1).map(|s| s.as_str()) == Some(recipient_hex.as_str())
        });

        if !has_recipient_p_tag {
            extra_tags.insert(
                0,
                Tag::parse(&["p".to_string(), recipient_hex])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let now_s = now.as_secs();
        let now_ms = now.as_millis();

        // Include an ms tag so the inner rumor id is stable (and matches what TS expects).
        if !extra_tags.iter().any(|t| {
            let v = t.clone().to_vec();
            v.first().map(|s| s.as_str()) == Some("ms")
        }) {
            extra_tags.push(
                Tag::parse(&["ms".to_string(), now_ms.to_string()])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            );
        }

        let kind = nostr::Kind::from(kind as u16);
        let mut event = nostr::EventBuilder::new(kind, &content)
            .tags(extra_tags)
            .custom_created_at(nostr::Timestamp::from(now_s))
            .build(self.owner_public_key);

        event.ensure_id();
        Ok(event)
    }

    pub fn send_event(&self, recipient: PublicKey, event: UnsignedEvent) -> Result<Vec<String>> {
        let recipient_owner = self.resolve_to_owner(&recipient);

        // Add to history for recipient and our owner (for sibling device sync)
        let mut history = self.message_history.lock().unwrap();
        for key in [recipient_owner, self.owner_public_key] {
            history.entry(key).or_default().push(event.clone());
        }
        drop(history);

        // Ensure we are set up for recipient and our own owner
        self.setup_user(recipient_owner);
        self.setup_user(self.owner_public_key);

        // Gather target devices (recipient + own siblings), de-dup, exclude ourselves
        let mut device_targets: Vec<(PublicKey, String)> = Vec::new();
        {
            let records = self.user_records.lock().unwrap();
            for owner in [recipient_owner, self.owner_public_key] {
                if let Some(record) = records.get(&owner) {
                    for device_id in record.device_records.keys() {
                        if device_id != &self.device_id {
                            device_targets.push((owner, device_id.clone()));
                        }
                    }
                }
            }
        }

        let mut seen = HashSet::new();
        device_targets.retain(|(_, device_id)| seen.insert(device_id.clone()));

        let mut event_ids = Vec::new();

        for (owner, device_id) in device_targets {
            let mut records = self.user_records.lock().unwrap();
            let Some(user_record) = records.get_mut(&owner) else {
                continue;
            };

            // Check if device is still authorized
            if let Ok(device_pk) = crate::utils::pubkey_from_hex(&device_id) {
                if !self.is_device_authorized_with_record(owner, device_pk, Some(&*user_record)) {
                    continue;
                }
            }

            let Some(device_record) = user_record.device_records.get_mut(&device_id) else {
                continue;
            };

            if let Some(ref mut session) = device_record.active_session {
                if let Ok(signed_event) = session.send_event(event.clone()) {
                    event_ids.push(signed_event.id.to_string());
                    let _ = self.pubsub.publish_signed(signed_event);
                }
            }
        }

        if !event_ids.is_empty() {
            let _ = self.store_user_record(&recipient_owner);
            if self.owner_public_key != recipient_owner {
                let _ = self.store_user_record(&self.owner_public_key);
            }
        }

        Ok(event_ids)
    }

    fn device_invite_key(&self, device_id: &str) -> String {
        format!("device-invite/{}", device_id)
    }

    fn send_options_default_key(&self) -> String {
        "send-options/default".to_string()
    }

    fn send_options_peer_prefix(&self) -> String {
        "send-options/peer/".to_string()
    }

    fn send_options_peer_key(&self, owner_pubkey: &PublicKey) -> String {
        format!(
            "{}{}",
            self.send_options_peer_prefix(),
            hex::encode(owner_pubkey.to_bytes())
        )
    }

    fn send_options_group_prefix(&self) -> String {
        "send-options/group/".to_string()
    }

    fn send_options_group_key(&self, group_id: &str) -> String {
        format!("{}{}", self.send_options_group_prefix(), group_id)
    }

    fn load_send_options(&self) -> Result<()> {
        // Default
        if let Some(data) = self.storage.get(&self.send_options_default_key())? {
            if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                *self.default_send_options.lock().unwrap() = Some(opts);
            }
        }

        // Per-peer
        let peer_keys = self.storage.list(&self.send_options_peer_prefix())?;
        for k in peer_keys {
            let hex_pk = k
                .strip_prefix(&self.send_options_peer_prefix())
                .unwrap_or("");
            if hex_pk.is_empty() {
                continue;
            }
            let Ok(pk) = crate::utils::pubkey_from_hex(hex_pk) else {
                continue;
            };
            if let Some(data) = self.storage.get(&k)? {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                    self.peer_send_options.lock().unwrap().insert(pk, opts);
                }
            }
        }

        // Per-group
        let group_keys = self.storage.list(&self.send_options_group_prefix())?;
        for k in group_keys {
            let group_id = k
                .strip_prefix(&self.send_options_group_prefix())
                .unwrap_or("")
                .to_string();
            if group_id.is_empty() {
                continue;
            }
            if let Some(data) = self.storage.get(&k)? {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                    self.group_send_options
                        .lock()
                        .unwrap()
                        .insert(group_id, opts);
                }
            }
        }

        Ok(())
    }

    fn effective_send_options(
        &self,
        recipient_owner: PublicKey,
        group_id: Option<&str>,
        override_options: Option<crate::SendOptions>,
    ) -> crate::SendOptions {
        if let Some(o) = override_options {
            return o;
        }

        if let Some(gid) = group_id {
            if let Some(o) = self.group_send_options.lock().unwrap().get(gid).cloned() {
                return o;
            }
        }

        if let Some(o) = self
            .peer_send_options
            .lock()
            .unwrap()
            .get(&recipient_owner)
            .cloned()
        {
            return o;
        }

        if let Some(o) = self.default_send_options.lock().unwrap().clone() {
            return o;
        }

        crate::SendOptions::default()
    }

    fn chat_settings_peer_pubkey(
        &self,
        from_owner_pubkey: PublicKey,
        rumor: &UnsignedEvent,
    ) -> Option<PublicKey> {
        let us = self.owner_public_key;

        // Determine which peer this applies to:
        // - for incoming messages, `from_owner_pubkey` is the peer
        // - for sender-copy sync across our own devices, `["p", <peer>]` indicates the peer
        let recipient_p = rumor.tags.iter().find_map(|t| {
            let v = t.clone().to_vec();
            if v.first().map(|s| s.as_str()) != Some("p") {
                return None;
            }
            let pk_hex = v.get(1)?;
            crate::utils::pubkey_from_hex(pk_hex).ok()
        });

        if let Some(p) = recipient_p {
            if p != us {
                return Some(p);
            }
        }

        if from_owner_pubkey != us {
            return Some(from_owner_pubkey);
        }

        None
    }

    fn maybe_auto_adopt_chat_settings(&self, from_owner_pubkey: PublicKey, rumor: &UnsignedEvent) {
        if !*self.auto_adopt_chat_settings.lock().unwrap() {
            return;
        }

        if rumor.kind.as_u16() != crate::CHAT_SETTINGS_KIND as u16 {
            return;
        }

        let payload = match serde_json::from_str::<serde_json::Value>(&rumor.content) {
            Ok(v) => v,
            Err(_) => return,
        };

        let typ = payload.get("type").and_then(|v| v.as_str());
        let v = payload.get("v").and_then(|v| v.as_u64());
        if typ != Some("chat-settings") || v != Some(1) {
            return;
        }

        let Some(peer_pubkey) = self.chat_settings_peer_pubkey(from_owner_pubkey, rumor) else {
            return;
        };

        match payload.get("messageTtlSeconds") {
            // Missing: clear per-peer override (fall back to global default).
            None => {
                let _ = self.set_peer_send_options(peer_pubkey, None);
            }
            // Null: disable per-peer expiration (even if a global default exists).
            Some(serde_json::Value::Null) => {
                let _ =
                    self.set_peer_send_options(peer_pubkey, Some(crate::SendOptions::default()));
            }
            Some(serde_json::Value::Number(n)) => {
                let Some(ttl) = n.as_u64() else {
                    return;
                };
                let opts = if ttl == 0 {
                    crate::SendOptions::default()
                } else {
                    crate::SendOptions {
                        ttl_seconds: Some(ttl),
                        expires_at: None,
                    }
                };
                let _ = self.set_peer_send_options(peer_pubkey, Some(opts));
            }
            _ => {}
        }
    }

    fn user_record_key(&self, pubkey: &PublicKey) -> String {
        format!("user/{}", hex::encode(pubkey.to_bytes()))
    }

    fn user_record_key_prefix(&self) -> String {
        "user/".to_string()
    }

    fn resolve_to_owner(&self, pubkey: &PublicKey) -> PublicKey {
        self.delegate_to_owner
            .lock()
            .unwrap()
            .get(pubkey)
            .copied()
            .unwrap_or(*pubkey)
    }

    fn update_delegate_mapping(&self, owner_pubkey: PublicKey, app_keys: &AppKeys) {
        let mut records = self.user_records.lock().unwrap();
        let user_record = records
            .entry(owner_pubkey)
            .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));

        let new_identities: HashSet<String> = app_keys
            .get_all_devices()
            .into_iter()
            .map(|d| hex::encode(d.identity_pubkey.to_bytes()))
            .collect();

        // Remove stale mappings
        let old_identities = user_record.known_device_identities.clone();
        for identity_hex in old_identities.iter() {
            if !new_identities.contains(identity_hex) {
                if let Ok(pk) = crate::utils::pubkey_from_hex(identity_hex) {
                    self.delegate_to_owner.lock().unwrap().remove(&pk);
                }
            }
        }

        user_record.known_device_identities = new_identities.iter().cloned().collect();

        for identity_hex in new_identities.iter() {
            if let Ok(pk) = crate::utils::pubkey_from_hex(identity_hex) {
                self.delegate_to_owner
                    .lock()
                    .unwrap()
                    .insert(pk, owner_pubkey);
            }
        }

        self.cached_app_keys
            .lock()
            .unwrap()
            .insert(owner_pubkey, app_keys.clone());

        drop(records);
        let _ = self.store_user_record(&owner_pubkey);
    }

    fn is_device_authorized(&self, owner_pubkey: PublicKey, device_pubkey: PublicKey) -> bool {
        if owner_pubkey == device_pubkey {
            return true;
        }

        if let Some(app_keys) = self.cached_app_keys.lock().unwrap().get(&owner_pubkey) {
            return app_keys.get_device(&device_pubkey).is_some();
        }

        let records = self.user_records.lock().unwrap();
        if let Some(record) = records.get(&owner_pubkey) {
            let device_hex = hex::encode(device_pubkey.to_bytes());
            return record.known_device_identities.contains(&device_hex);
        }

        false
    }

    fn is_device_authorized_with_record(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
        user_record: Option<&UserRecord>,
    ) -> bool {
        if owner_pubkey == device_pubkey {
            return true;
        }

        if let Some(app_keys) = self.cached_app_keys.lock().unwrap().get(&owner_pubkey) {
            return app_keys.get_device(&device_pubkey).is_some();
        }

        if let Some(record) = user_record {
            let device_hex = hex::encode(device_pubkey.to_bytes());
            return record.known_device_identities.contains(&device_hex);
        }

        false
    }

    fn subscribe_to_app_keys(&self, owner_pubkey: PublicKey) {
        let mut subs = self.app_keys_subscriptions.lock().unwrap();
        if subs.contains(&owner_pubkey) {
            return;
        }
        subs.insert(owner_pubkey);
        drop(subs);

        let filter = nostr::Filter::new()
            .kind(nostr::Kind::Custom(crate::APP_KEYS_EVENT_KIND as u16))
            .authors(vec![owner_pubkey])
            .custom_tag(
                nostr::types::filter::SingleLetterTag::lowercase(nostr::types::filter::Alphabet::D),
                ["double-ratchet/app-keys"],
            );
        if let Ok(filter_json) = serde_json::to_string(&filter) {
            let subid = format!("app-keys-{}", uuid::Uuid::new_v4());
            let _ = self.pubsub.subscribe(subid, filter_json);
        }
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) {
        let owner_pubkey = self.resolve_to_owner(&user_pubkey);

        // Ensure record exists
        {
            let mut records = self.user_records.lock().unwrap();
            records
                .entry(owner_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
        }

        self.subscribe_to_app_keys(owner_pubkey);

        // Subscribe to invites for any known devices from stored identities
        let known_identities = {
            let records = self.user_records.lock().unwrap();
            records
                .get(&owner_pubkey)
                .map(|r| r.known_device_identities.clone())
                .unwrap_or_default()
        };

        for identity_hex in known_identities {
            if let Ok(pk) = crate::utils::pubkey_from_hex(&identity_hex) {
                self.subscribe_to_device_invite(owner_pubkey, pk);
            }
        }
    }

    fn subscribe_to_device_invite(&self, owner_pubkey: PublicKey, device_pubkey: PublicKey) {
        let mut subs = self.invite_subscriptions.lock().unwrap();
        if subs.contains(&device_pubkey) {
            return;
        }
        subs.insert(device_pubkey);
        drop(subs);

        let records = self.user_records.lock().unwrap();
        if let Some(record) = records.get(&owner_pubkey) {
            let device_hex = hex::encode(device_pubkey.to_bytes());
            if let Some(device_record) = record.device_records.get(&device_hex) {
                if device_record.active_session.is_some() {
                    return;
                }
            }
        }
        drop(records);

        let _ = Invite::from_user_with_pubsub(device_pubkey, self.pubsub.as_ref());
    }

    fn upsert_device_record(&self, record: &mut UserRecord, device_id: &str) {
        if record.device_records.contains_key(device_id) {
            return;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        record.device_records.insert(
            device_id.to_string(),
            crate::DeviceRecord {
                device_id: device_id.to_string(),
                public_key: String::new(),
                active_session: None,
                inactive_sessions: Vec::new(),
                created_at: now,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(now),
            },
        );
    }

    fn record_known_device_identity(&self, owner_pubkey: PublicKey, device_pubkey: PublicKey) {
        let identity_hex = hex::encode(device_pubkey.to_bytes());
        let mut records = self.user_records.lock().unwrap();
        let record = records
            .entry(owner_pubkey)
            .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
        let mut updated = false;
        if !record.known_device_identities.contains(&identity_hex) {
            record.known_device_identities.push(identity_hex.clone());
            updated = true;
        }
        self.delegate_to_owner
            .lock()
            .unwrap()
            .insert(device_pubkey, owner_pubkey);
        drop(records);
        if updated {
            let _ = self.store_user_record(&owner_pubkey);
        }
    }

    fn send_message_history(&self, owner_pubkey: PublicKey, device_id: &str) {
        let history = {
            self.message_history
                .lock()
                .unwrap()
                .get(&owner_pubkey)
                .cloned()
                .unwrap_or_default()
        };

        if history.is_empty() {
            return;
        }

        let mut records = self.user_records.lock().unwrap();
        let Some(user_record) = records.get_mut(&owner_pubkey) else {
            return;
        };
        let Some(device_record) = user_record.device_records.get_mut(device_id) else {
            return;
        };
        let Some(ref mut session) = device_record.active_session else {
            return;
        };

        for event in history {
            if let Ok(signed_event) = session.send_event(event.clone()) {
                let _ = self.pubsub.publish_signed(signed_event);
            }
        }
        drop(records);
        let _ = self.store_user_record(&owner_pubkey);
    }

    fn cleanup_device(&self, owner_pubkey: PublicKey, device_id: &str) {
        let mut records = self.user_records.lock().unwrap();
        let Some(user_record) = records.get_mut(&owner_pubkey) else {
            return;
        };

        if let Some(device_record) = user_record.device_records.remove(device_id) {
            if let Some(session) = device_record.active_session {
                session.close();
            }
            for session in device_record.inactive_sessions {
                session.close();
            }
        }

        if let Ok(device_pk) = crate::utils::pubkey_from_hex(device_id) {
            self.delegate_to_owner.lock().unwrap().remove(&device_pk);
        }

        drop(records);
        let _ = self.store_user_record(&owner_pubkey);
    }

    fn handle_app_keys_event(&self, owner_pubkey: PublicKey, app_keys: AppKeys) {
        self.update_delegate_mapping(owner_pubkey, &app_keys);

        let devices = app_keys.get_all_devices();
        let active_ids: HashSet<String> = devices
            .iter()
            .map(|d| hex::encode(d.identity_pubkey.to_bytes()))
            .collect();

        // Cleanup revoked devices
        let existing_devices = {
            let records = self.user_records.lock().unwrap();
            records
                .get(&owner_pubkey)
                .map(|r| r.device_records.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };

        for device_id in existing_devices {
            if !active_ids.contains(&device_id) {
                self.cleanup_device(owner_pubkey, &device_id);
                self.invite_subscriptions
                    .lock()
                    .unwrap()
                    .retain(|pk| hex::encode(pk.to_bytes()) != device_id);
            }
        }

        for device in devices {
            self.subscribe_to_device_invite(owner_pubkey, device.identity_pubkey);
        }
    }

    fn store_user_record(&self, pubkey: &PublicKey) -> Result<()> {
        let user_records = self.user_records.lock().unwrap();
        if let Some(user_record) = user_records.get(pubkey) {
            let stored = user_record.to_stored();
            let key = self.user_record_key(pubkey);
            let json = serde_json::to_string(&stored)?;
            self.storage.put(&key, json)?;
        }
        Ok(())
    }

    fn load_all_user_records(&self) -> Result<()> {
        let prefix = self.user_record_key_prefix();
        let keys = self.storage.list(&prefix)?;

        let mut records = self.user_records.lock().unwrap();

        for key in keys {
            let Some(data) = self.storage.get(&key)? else {
                continue;
            };

            let stored: crate::StoredUserRecord = match serde_json::from_str(&data) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let pubkey_hex = key.strip_prefix(&prefix).unwrap_or(&stored.user_id);
            let owner_pubkey = match crate::utils::pubkey_from_hex(pubkey_hex) {
                Ok(pk) => pk,
                Err(_) => continue,
            };

            let mut user_record = UserRecord::new(stored.user_id.clone());
            user_record.known_device_identities = stored.known_device_identities.clone();

            for device in stored.devices {
                let mut device_record = crate::DeviceRecord {
                    device_id: device.device_id.clone(),
                    public_key: String::new(),
                    active_session: None,
                    inactive_sessions: Vec::new(),
                    created_at: device.created_at,
                    is_stale: device.is_stale,
                    stale_timestamp: device.stale_timestamp,
                    last_activity: device.last_activity,
                };

                if let Some(state) = device.active_session {
                    let mut session =
                        crate::Session::new(state, format!("session-{}", device.device_id));
                    session.set_pubsub(self.pubsub.clone());
                    let _ = session.subscribe_to_messages();
                    device_record.active_session = Some(session);
                }

                for state in device.inactive_sessions {
                    let mut session = crate::Session::new(
                        state,
                        format!("session-{}-inactive", device.device_id),
                    );
                    session.set_pubsub(self.pubsub.clone());
                    let _ = session.subscribe_to_messages();
                    device_record.inactive_sessions.push(session);
                }

                user_record
                    .device_records
                    .insert(device.device_id.clone(), device_record);
            }

            for identity_hex in stored.known_device_identities.iter() {
                if let Ok(pk) = crate::utils::pubkey_from_hex(identity_hex) {
                    self.delegate_to_owner
                        .lock()
                        .unwrap()
                        .insert(pk, owner_pubkey);
                }
            }

            records.insert(owner_pubkey, user_record);
        }

        Ok(())
    }

    fn promote_session_to_active(
        user_record: &mut UserRecord,
        device_id: &str,
        session_index: usize,
    ) {
        let Some(device_record) = user_record.device_records.get_mut(device_id) else {
            return;
        };

        if session_index >= device_record.inactive_sessions.len() {
            return;
        }

        let session = device_record.inactive_sessions.remove(session_index);
        if let Some(active) = device_record.active_session.take() {
            device_record.inactive_sessions.insert(0, active);
        }
        device_record.active_session = Some(session);

        const MAX_INACTIVE: usize = 10;
        if device_record.inactive_sessions.len() > MAX_INACTIVE {
            device_record.inactive_sessions.truncate(MAX_INACTIVE);
        }
    }

    pub fn process_received_event(&self, event: nostr::Event) {
        if is_app_keys_event(&event) {
            if let Ok(app_keys) = AppKeys::from_event(&event) {
                self.handle_app_keys_event(event.pubkey, app_keys);
            }
            return;
        }

        if event.kind.as_u16() == crate::INVITE_RESPONSE_KIND as u16 {
            if self
                .processed_invite_responses
                .lock()
                .unwrap()
                .contains(&event.id.to_string())
            {
                return;
            }

            if let Some(state) = self.invite_state.lock().unwrap().as_ref() {
                if let Ok(Some(response)) = state
                    .invite
                    .process_invite_response(&event, state.our_identity_key)
                {
                    if response.invitee_identity == self.our_public_key {
                        return;
                    }

                    let owner_pubkey = response
                        .owner_public_key
                        .unwrap_or_else(|| self.resolve_to_owner(&response.invitee_identity));

                    if !self.is_device_authorized(owner_pubkey, response.invitee_identity) {
                        return;
                    }

                    self.record_known_device_identity(owner_pubkey, response.invitee_identity);

                    let device_id = response
                        .device_id
                        .unwrap_or_else(|| hex::encode(response.invitee_identity.to_bytes()));

                    let mut session = response.session;
                    session.set_pubsub(self.pubsub.clone());
                    let _ = session.subscribe_to_messages();

                    {
                        let mut records = self.user_records.lock().unwrap();
                        let user_record = records.entry(owner_pubkey).or_insert_with(|| {
                            UserRecord::new(hex::encode(owner_pubkey.to_bytes()))
                        });
                        self.upsert_device_record(user_record, &device_id);
                        user_record.upsert_session(Some(&device_id), session);
                    }

                    let _ = self.store_user_record(&owner_pubkey);
                    self.send_message_history(owner_pubkey, &device_id);

                    self.processed_invite_responses
                        .lock()
                        .unwrap()
                        .insert(event.id.to_string());
                }
            }
            return;
        }

        if event.kind.as_u16() == crate::INVITE_EVENT_KIND as u16 {
            if let Ok(invite) = Invite::from_event(&event) {
                if invite.inviter == self.our_public_key {
                    return;
                }

                let inviter_device = invite.inviter;
                let owner_pubkey = self.resolve_to_owner(&inviter_device);

                if !self.is_device_authorized(owner_pubkey, inviter_device) {
                    return;
                }

                let device_id = invite
                    .device_id
                    .clone()
                    .unwrap_or_else(|| hex::encode(inviter_device.to_bytes()));

                let already_has_session = {
                    let records = self.user_records.lock().unwrap();
                    records
                        .get(&owner_pubkey)
                        .and_then(|r| r.device_records.get(&device_id))
                        .and_then(|d| d.active_session.as_ref())
                        .is_some()
                };

                if already_has_session {
                    return;
                }

                {
                    let mut pending = self.pending_acceptances.lock().unwrap();
                    if pending.contains(&inviter_device) {
                        return;
                    }
                    pending.insert(inviter_device);
                }

                let accept_result = invite.accept_with_owner(
                    self.our_public_key,
                    self.our_identity_key,
                    Some(self.device_id.clone()),
                    Some(self.owner_public_key),
                );

                match accept_result {
                    Ok((mut session, response_event)) => {
                        let _ = self.pubsub.publish_signed(response_event);
                        session.set_pubsub(self.pubsub.clone());
                        let _ = session.subscribe_to_messages();

                        {
                            let mut records = self.user_records.lock().unwrap();
                            let user_record = records.entry(owner_pubkey).or_insert_with(|| {
                                UserRecord::new(hex::encode(owner_pubkey.to_bytes()))
                            });
                            self.upsert_device_record(user_record, &device_id);
                            user_record.upsert_session(Some(&device_id), session);
                        }

                        self.record_known_device_identity(owner_pubkey, inviter_device);
                        let _ = self.store_user_record(&owner_pubkey);
                        self.send_message_history(owner_pubkey, &device_id);
                    }
                    Err(_) => {}
                }

                self.pending_acceptances
                    .lock()
                    .unwrap()
                    .remove(&inviter_device);
            }
            return;
        }

        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16 {
            let event_id = Some(event.id.to_string());
            let mut decrypted: Option<(PublicKey, String, String)> = None;

            {
                let mut records = self.user_records.lock().unwrap();
                'outer: for (owner_pubkey, user_record) in records.iter_mut() {
                    let device_ids: Vec<String> =
                        user_record.device_records.keys().cloned().collect();

                    for device_id in device_ids {
                        let Some(device_record) = user_record.device_records.get_mut(&device_id)
                        else {
                            continue;
                        };

                        if let Some(ref mut session) = device_record.active_session {
                            if let Ok(Some(plaintext)) = session.receive(&event) {
                                decrypted = Some((*owner_pubkey, plaintext, device_id.clone()));
                                break 'outer;
                            }
                        }

                        for idx in 0..device_record.inactive_sessions.len() {
                            let plaintext_opt = {
                                let session = &mut device_record.inactive_sessions[idx];
                                session.receive(&event).ok().flatten()
                            };

                            if let Some(plaintext) = plaintext_opt {
                                SessionManager::promote_session_to_active(
                                    user_record,
                                    &device_id,
                                    idx,
                                );
                                decrypted = Some((*owner_pubkey, plaintext, device_id.clone()));
                                break 'outer;
                            }
                        }
                    }
                }
            }

            if let Some((owner_pubkey, plaintext, device_id)) = decrypted {
                if let Ok(sender_pk) = crate::utils::pubkey_from_hex(&device_id) {
                    let sender_owner = self.resolve_to_owner(&sender_pk);
                    if sender_owner != sender_pk
                        && !self.is_device_authorized(sender_owner, sender_pk)
                    {
                        return;
                    }
                }

                if let Ok(rumor) = serde_json::from_str::<UnsignedEvent>(&plaintext) {
                    self.maybe_auto_adopt_chat_settings(owner_pubkey, &rumor);
                }

                let _ = self
                    .pubsub
                    .decrypted_message(owner_pubkey, plaintext, event_id);
                let _ = self.store_user_record(&owner_pubkey);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    #[test]
    fn test_session_manager_new() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (tx, _rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(
            pubkey,
            identity_key,
            device_id.clone(),
            pubkey,
            tx,
            None,
            None,
        );

        assert_eq!(manager.get_device_id(), device_id);
    }

    #[test]
    fn test_send_text_no_sessions() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (tx, _rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

        let recipient = Keys::generate().public_key();
        let result = manager.send_text(recipient, "test".to_string(), None);

        assert!(result.is_ok());
    }

    #[test]
    fn test_auto_adopt_chat_settings_sender_copy_uses_p_tag_peer() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();
        let (tx, _rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

        let peer = Keys::generate().public_key();
        let peer_hex = hex::encode(peer.to_bytes());

        // Sender-copy: from_owner_pubkey == us, so peer must be taken from the ["p", ...] tag.
        let payload = serde_json::json!({
            "type": "chat-settings",
            "v": 1,
            "messageTtlSeconds": 90,
        })
        .to_string();

        let rumor = nostr::EventBuilder::new(
            nostr::Kind::from(crate::CHAT_SETTINGS_KIND as u16),
            &payload,
        )
        .tag(
            Tag::parse(&["p".to_string(), peer_hex])
                .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
                .unwrap(),
        )
        .build(pubkey);

        manager.maybe_auto_adopt_chat_settings(pubkey, &rumor);

        let opts = manager
            .peer_send_options
            .lock()
            .unwrap()
            .get(&peer)
            .cloned()
            .unwrap();
        assert_eq!(opts.ttl_seconds, Some(90));
        assert_eq!(opts.expires_at, None);

        // Null disables per-peer expiration (stores an empty SendOptions override).
        let payload_disable = serde_json::json!({
            "type": "chat-settings",
            "v": 1,
            "messageTtlSeconds": null,
        })
        .to_string();

        let rumor_disable = nostr::EventBuilder::new(
            nostr::Kind::from(crate::CHAT_SETTINGS_KIND as u16),
            &payload_disable,
        )
        .tag(
            Tag::parse(&["p".to_string(), hex::encode(peer.to_bytes())])
                .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
                .unwrap(),
        )
        .build(pubkey);

        manager.maybe_auto_adopt_chat_settings(pubkey, &rumor_disable);
        let opts_disable = manager
            .peer_send_options
            .lock()
            .unwrap()
            .get(&peer)
            .cloned()
            .unwrap();
        assert_eq!(opts_disable.ttl_seconds, None);
        assert_eq!(opts_disable.expires_at, None);
    }
}
