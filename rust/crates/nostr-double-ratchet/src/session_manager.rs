use crate::{
    is_app_keys_event, AppKeys, DeviceEntry, InMemoryStorage, Invite, MessageQueue, NostrPubSub,
    OneToManyChannel, Result, SenderKeyDistribution, SenderKeyState, StorageAdapter, UserRecord,
    GROUP_SENDER_KEY_DISTRIBUTION_KIND,
};
use nostr::{Keys, PublicKey, Tag, UnsignedEvent};
use serde::{Deserialize, Serialize};
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
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    },
}

pub struct AcceptInviteResult {
    pub owner_pubkey: PublicKey,
    pub inviter_device_pubkey: PublicKey,
    pub device_id: String,
    pub created_new_session: bool,
}

struct InviteState {
    invite: Invite,
    our_identity_key: [u8; 32],
}

/// Stored mapping for routing one-to-many group messages authored by a per-sender Nostr pubkey.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredGroupSenderEventInfo {
    group_id: String,
    sender_owner_pubkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender_device_pubkey: Option<String>,
}

/// In-memory mapping for routing one-to-many group messages authored by a per-sender Nostr pubkey.
#[derive(Debug, Clone)]
struct GroupSenderEventInfo {
    group_id: String,
    sender_owner_pubkey: PublicKey,
    sender_device_pubkey: Option<PublicKey>,
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
    message_queue: MessageQueue,
    discovery_queue: MessageQueue,
    invite_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    app_keys_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    pending_acceptances: Arc<Mutex<HashSet<PublicKey>>>,
    default_send_options: Arc<Mutex<Option<crate::SendOptions>>>,
    peer_send_options: Arc<Mutex<HashMap<PublicKey, crate::SendOptions>>>,
    group_send_options: Arc<Mutex<HashMap<String, crate::SendOptions>>>,
    auto_adopt_chat_settings: Arc<Mutex<bool>>,
    group_sender_events: Arc<Mutex<HashMap<PublicKey, GroupSenderEventInfo>>>,
    group_sender_key_states: Arc<Mutex<HashMap<(PublicKey, u32), SenderKeyState>>>,
    group_sender_key_pending: Arc<Mutex<HashMap<(PublicKey, u32), Vec<nostr::Event>>>>,
    group_sender_event_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
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

    /// Delete local chat/session state for a peer owner.
    ///
    /// This is intentionally local-only and does not create persistent tombstones.
    /// A chat can be re-initialized later by explicit join/send flows.
    pub fn delete_chat(&self, user_pubkey: PublicKey) -> Result<()> {
        self.init()?;
        let owner_pubkey = self.resolve_to_owner(&user_pubkey);
        if owner_pubkey == self.owner_public_key {
            return Ok(());
        }
        self.delete_user_local(owner_pubkey)
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
        let storage = storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new()));
        let message_queue = MessageQueue::new(storage.clone(), "v1/message-queue/");
        let discovery_queue = MessageQueue::new(storage.clone(), "v1/discovery-queue/");
        Self {
            user_records: Arc::new(Mutex::new(HashMap::new())),
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            storage,
            pubsub,
            initialized: Arc::new(Mutex::new(false)),
            invite_state: Arc::new(Mutex::new(None)),
            provided_invite: invite,
            delegate_to_owner: Arc::new(Mutex::new(HashMap::new())),
            cached_app_keys: Arc::new(Mutex::new(HashMap::new())),
            processed_invite_responses: Arc::new(Mutex::new(HashSet::new())),
            message_history: Arc::new(Mutex::new(HashMap::new())),
            message_queue,
            discovery_queue,
            invite_subscriptions: Arc::new(Mutex::new(HashSet::new())),
            app_keys_subscriptions: Arc::new(Mutex::new(HashSet::new())),
            pending_acceptances: Arc::new(Mutex::new(HashSet::new())),
            default_send_options: Arc::new(Mutex::new(None)),
            peer_send_options: Arc::new(Mutex::new(HashMap::new())),
            group_send_options: Arc::new(Mutex::new(HashMap::new())),
            auto_adopt_chat_settings: Arc::new(Mutex::new(true)),
            group_sender_events: Arc::new(Mutex::new(HashMap::new())),
            group_sender_key_states: Arc::new(Mutex::new(HashMap::new())),
            group_sender_key_pending: Arc::new(Mutex::new(HashMap::new())),
            group_sender_event_subscriptions: Arc::new(Mutex::new(HashSet::new())),
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
        let _ = self.load_group_sender_event_infos();

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
        let active_device_ids: Vec<String> = records
            .values()
            .flat_map(|user_record| {
                user_record
                    .device_records
                    .iter()
                    .filter_map(|(device_id, device_record)| {
                        device_record
                            .active_session
                            .as_ref()
                            .map(|_| device_id.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        drop(records);

        for device_id in active_device_ids {
            let _ = self.flush_message_queue(&device_id);
        }

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

    pub fn export_active_sessions(&self) -> Vec<(PublicKey, String, crate::SessionState)> {
        let records = self.user_records.lock().unwrap();
        let mut out = Vec::new();

        for (owner_pubkey, user_record) in records.iter() {
            for (device_id, device_record) in user_record.device_records.iter() {
                if let Some(session) = &device_record.active_session {
                    out.push((*owner_pubkey, device_id.clone(), session.state.clone()));
                }
            }
        }

        out
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

    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<AcceptInviteResult> {
        let inviter_device_pubkey = invite.inviter;
        if inviter_device_pubkey == self.our_public_key {
            return Err(crate::Error::Invite(
                "Cannot accept invite from this device".to_string(),
            ));
        }

        let owner_pubkey = owner_pubkey_hint
            .or(invite.owner_public_key)
            .unwrap_or_else(|| self.resolve_to_owner(&inviter_device_pubkey));

        if owner_pubkey != inviter_device_pubkey {
            let cached_app_keys = self
                .cached_app_keys
                .lock()
                .unwrap()
                .get(&owner_pubkey)
                .cloned();
            if let Some(app_keys) = cached_app_keys {
                if app_keys.get_device(&inviter_device_pubkey).is_none() {
                    return Err(crate::Error::Invite(
                        "Invite device is not authorized by cached AppKeys".to_string(),
                    ));
                }
                self.update_delegate_mapping(owner_pubkey, &app_keys);
            } else {
                let known_device_identities = self
                    .user_records
                    .lock()
                    .unwrap()
                    .get(&owner_pubkey)
                    .map(|record| record.known_device_identities.clone())
                    .unwrap_or_default();
                if !known_device_identities.is_empty() {
                    let inviter_hex = inviter_device_pubkey.to_hex();
                    if !known_device_identities.iter().any(|id| id == &inviter_hex) {
                        return Err(crate::Error::Invite(
                            "Invite device is not authorized by stored AppKeys".to_string(),
                        ));
                    }
                }
            }
        }

        let device_id = invite
            .device_id
            .clone()
            .unwrap_or_else(|| hex::encode(inviter_device_pubkey.to_bytes()));

        let already_has_session = {
            let records = self.user_records.lock().unwrap();
            records
                .get(&owner_pubkey)
                .and_then(|r| r.device_records.get(&device_id))
                .and_then(|d| d.active_session.as_ref())
                .is_some()
        };
        if already_has_session {
            self.record_known_device_identity(owner_pubkey, inviter_device_pubkey);
            return Ok(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: false,
            });
        }

        {
            let mut pending = self.pending_acceptances.lock().unwrap();
            if pending.contains(&inviter_device_pubkey) {
                return Err(crate::Error::Invite(
                    "Invite acceptance already in progress".to_string(),
                ));
            }
            pending.insert(inviter_device_pubkey);
        }

        let result = (|| -> Result<AcceptInviteResult> {
            let (mut session, response_event) = invite.accept_with_owner(
                self.our_public_key,
                self.our_identity_key,
                Some(self.device_id.clone()),
                Some(self.owner_public_key),
            )?;

            self.pubsub.publish_signed(response_event)?;

            session.set_pubsub(self.pubsub.clone());
            let _ = session.subscribe_to_messages();

            {
                let mut records = self.user_records.lock().unwrap();
                let user_record = records
                    .entry(owner_pubkey)
                    .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
                self.upsert_device_record(user_record, &device_id);
                user_record.upsert_session(Some(&device_id), session);
            }

            self.record_known_device_identity(owner_pubkey, inviter_device_pubkey);
            let _ = self.store_user_record(&owner_pubkey);
            self.send_message_history(owner_pubkey, &device_id);
            let _ = self.flush_message_queue(&device_id);

            Ok(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: true,
            })
        })();

        self.pending_acceptances
            .lock()
            .unwrap()
            .remove(&inviter_device_pubkey);

        result
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

    fn send_event_internal(
        &self,
        recipient_owner: PublicKey,
        event: UnsignedEvent,
        include_owner_sync: bool,
    ) -> Result<Vec<String>> {
        let mut owners = vec![recipient_owner];
        if include_owner_sync && self.owner_public_key != recipient_owner {
            owners.push(self.owner_public_key);
        }

        // Add to history for all target owners.
        //
        // Avoid persisting ephemeral typing indicators here: they are noisy, not meaningful to replay
        // to newly discovered devices, and can grow memory usage in long-running processes.
        if event.kind.as_u16() != crate::TYPING_KIND as u16 {
            let mut history = self.message_history.lock().unwrap();
            for owner in owners.iter().copied() {
                history.entry(owner).or_default().push(event.clone());
            }
        }

        // Ensure all target owners are set up.
        for owner in owners.iter().copied() {
            self.setup_user(owner);
        }

        // Gather known devices per owner.
        let mut owner_targets: HashMap<PublicKey, Vec<String>> = HashMap::new();
        {
            let records = self.user_records.lock().unwrap();
            for owner in owners.iter().copied() {
                let mut device_ids = Vec::new();
                if let Some(record) = records.get(&owner) {
                    for device_id in record.device_records.keys() {
                        if device_id != &self.device_id {
                            device_ids.push(device_id.clone());
                        }
                    }
                }
                owner_targets.insert(owner, device_ids);
            }
        }

        // Queue for each target owner:
        // - known devices -> message queue per device
        // - no known devices -> discovery queue per owner
        for owner in owners.iter().copied() {
            let mut seen_for_owner = HashSet::new();
            let device_ids = owner_targets.get(&owner).cloned().unwrap_or_default();
            let mut queued_any_device = false;
            for device_id in device_ids {
                if !seen_for_owner.insert(device_id.clone()) {
                    continue;
                }
                queued_any_device = true;
                let _ = self.message_queue.add(&device_id, &event);
            }
            if !queued_any_device {
                let _ = self.discovery_queue.add(&owner.to_hex(), &event);
            }
        }

        // Current known active targets to send immediately.
        let mut device_targets: Vec<(PublicKey, String)> = Vec::new();
        let mut seen = HashSet::new();
        for owner in owners.iter().copied() {
            if let Some(device_ids) = owner_targets.get(&owner) {
                for device_id in device_ids {
                    if seen.insert(device_id.clone()) {
                        device_targets.push((owner, device_id.clone()));
                    }
                }
            }
        }

        let mut event_ids = Vec::new();
        let inner_event_id = event.id.as_ref().map(|id| id.to_string());
        let mut published_device_ids: Vec<String> = Vec::new();

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
                    if self.pubsub.publish_signed(signed_event).is_ok() {
                        published_device_ids.push(device_id.clone());
                    }
                }
            }
        }

        if let Some(ref id) = inner_event_id {
            let mut seen = HashSet::new();
            for device_id in published_device_ids {
                if !seen.insert(device_id.clone()) {
                    continue;
                }
                let _ = self
                    .message_queue
                    .remove_by_target_and_event_id(&device_id, id);
                let _ = self.flush_message_queue(&device_id);
            }
        }

        if !event_ids.is_empty() {
            let _ = self.store_user_record(&recipient_owner);
            if include_owner_sync && self.owner_public_key != recipient_owner {
                let _ = self.store_user_record(&self.owner_public_key);
            }
        }

        Ok(event_ids)
    }

    pub fn send_event(&self, recipient: PublicKey, event: UnsignedEvent) -> Result<Vec<String>> {
        let recipient_owner = self.resolve_to_owner(&recipient);
        self.send_event_internal(recipient_owner, event, true)
    }

    pub fn send_event_recipient_only(
        &self,
        recipient: PublicKey,
        event: UnsignedEvent,
    ) -> Result<Vec<String>> {
        let recipient_owner = self.resolve_to_owner(&recipient);
        self.send_event_internal(recipient_owner, event, false)
    }

    fn delete_user_local(&self, owner_pubkey: PublicKey) -> Result<()> {
        if owner_pubkey == self.owner_public_key {
            return Ok(());
        }

        let removed_user_record = {
            let mut records = self.user_records.lock().unwrap();
            records.remove(&owner_pubkey)
        };

        let mut known_device_pubkeys: Vec<PublicKey> = Vec::new();
        let mut known_device_ids: Vec<String> = Vec::new();

        if let Some(user_record) = removed_user_record {
            for (device_id, device_record) in user_record.device_records {
                if let Some(session) = device_record.active_session {
                    session.close();
                }
                for session in device_record.inactive_sessions {
                    session.close();
                }
                known_device_ids.push(device_id.clone());
                if let Ok(device_pk) = crate::utils::pubkey_from_hex(&device_id) {
                    known_device_pubkeys.push(device_pk);
                }
            }

            for identity_hex in user_record.known_device_identities {
                if let Ok(device_pk) = crate::utils::pubkey_from_hex(&identity_hex) {
                    known_device_pubkeys.push(device_pk);
                }
            }
        }

        self.delegate_to_owner
            .lock()
            .unwrap()
            .retain(|pk, owner| *owner != owner_pubkey && !known_device_pubkeys.contains(pk));
        self.invite_subscriptions
            .lock()
            .unwrap()
            .retain(|pk| self.resolve_to_owner(pk) != owner_pubkey);
        self.app_keys_subscriptions
            .lock()
            .unwrap()
            .remove(&owner_pubkey);
        self.pending_acceptances
            .lock()
            .unwrap()
            .retain(|pk| self.resolve_to_owner(pk) != owner_pubkey);

        self.cached_app_keys.lock().unwrap().remove(&owner_pubkey);
        self.peer_send_options.lock().unwrap().remove(&owner_pubkey);
        self.message_history.lock().unwrap().remove(&owner_pubkey);

        self.discovery_queue
            .remove_for_target(&owner_pubkey.to_hex())?;
        for device_id in known_device_ids {
            self.message_queue.remove_for_target(&device_id)?;
        }

        let _ = self.storage.del(&self.send_options_peer_key(&owner_pubkey));
        let _ = self.storage.del(&self.user_record_key(&owner_pubkey));
        Ok(())
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

    fn group_sender_event_info_prefix(&self) -> String {
        "group-sender-key/sender-event/".to_string()
    }

    fn group_sender_event_info_key(&self, sender_event_pubkey: &PublicKey) -> String {
        format!(
            "{}{}",
            self.group_sender_event_info_prefix(),
            hex::encode(sender_event_pubkey.to_bytes())
        )
    }

    fn group_sender_key_state_key(&self, sender_event_pubkey: &PublicKey, key_id: u32) -> String {
        format!(
            "group-sender-key/state/{}/{}",
            hex::encode(sender_event_pubkey.to_bytes()),
            key_id
        )
    }

    fn tag_value(tags: &nostr::Tags, key: &str) -> Option<String> {
        tags.iter()
            .find_map(|t| {
                let v = t.clone().to_vec();
                if v.first().map(|s| s.as_str()) != Some(key) {
                    return None;
                }
                v.get(1).cloned()
            })
            .filter(|s| !s.is_empty())
    }

    fn load_group_sender_event_infos(&self) -> Result<()> {
        let prefix = self.group_sender_event_info_prefix();
        let keys = self.storage.list(&prefix)?;

        for key in keys {
            let Some(hex_pk) = key.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(sender_event_pubkey) = crate::utils::pubkey_from_hex(hex_pk) else {
                continue;
            };
            let Some(data) = self.storage.get(&key)? else {
                continue;
            };

            let stored: StoredGroupSenderEventInfo = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let Ok(sender_owner_pubkey) =
                crate::utils::pubkey_from_hex(&stored.sender_owner_pubkey)
            else {
                continue;
            };
            let sender_device_pubkey = stored
                .sender_device_pubkey
                .as_deref()
                .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

            let info = GroupSenderEventInfo {
                group_id: stored.group_id,
                sender_owner_pubkey,
                sender_device_pubkey,
            };

            self.group_sender_events
                .lock()
                .unwrap()
                .insert(sender_event_pubkey, info);

            let _ = self.subscribe_to_group_sender_event(sender_event_pubkey);
        }

        Ok(())
    }

    fn subscribe_to_group_sender_event(&self, sender_event_pubkey: PublicKey) -> Result<()> {
        {
            let mut subs = self.group_sender_event_subscriptions.lock().unwrap();
            if subs.contains(&sender_event_pubkey) {
                return Ok(());
            }
            subs.insert(sender_event_pubkey);
        }

        let filter = crate::pubsub::build_filter()
            .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
            .authors(vec![sender_event_pubkey])
            .build();
        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!(
            "group-sender-event-{}",
            hex::encode(sender_event_pubkey.to_bytes())
        );
        self.pubsub.subscribe(subid, filter_json)
    }

    fn load_group_sender_event_info(
        &self,
        sender_event_pubkey: &PublicKey,
    ) -> Option<GroupSenderEventInfo> {
        {
            if let Some(info) = self
                .group_sender_events
                .lock()
                .unwrap()
                .get(sender_event_pubkey)
            {
                return Some(info.clone());
            }
        }

        let key = self.group_sender_event_info_key(sender_event_pubkey);
        let data = self.storage.get(&key).ok().flatten()?;
        let stored: StoredGroupSenderEventInfo = serde_json::from_str(&data).ok()?;
        let sender_owner_pubkey =
            crate::utils::pubkey_from_hex(&stored.sender_owner_pubkey).ok()?;
        let sender_device_pubkey = stored
            .sender_device_pubkey
            .as_deref()
            .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

        let info = GroupSenderEventInfo {
            group_id: stored.group_id,
            sender_owner_pubkey,
            sender_device_pubkey,
        };

        self.group_sender_events
            .lock()
            .unwrap()
            .insert(*sender_event_pubkey, info.clone());

        Some(info)
    }

    fn load_sender_key_state(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
    ) -> Option<SenderKeyState> {
        {
            if let Some(state) = self
                .group_sender_key_states
                .lock()
                .unwrap()
                .get(&(*sender_event_pubkey, key_id))
            {
                return Some(state.clone());
            }
        }

        let key = self.group_sender_key_state_key(sender_event_pubkey, key_id);
        let data = self.storage.get(&key).ok().flatten()?;
        let state: SenderKeyState = serde_json::from_str(&data).ok()?;
        self.group_sender_key_states
            .lock()
            .unwrap()
            .insert((*sender_event_pubkey, key_id), state.clone());
        Some(state)
    }

    fn store_sender_key_state(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
        state: &SenderKeyState,
    ) -> Result<()> {
        let key = self.group_sender_key_state_key(sender_event_pubkey, key_id);
        self.storage.put(&key, serde_json::to_string(state)?)?;
        Ok(())
    }

    fn ensure_sender_key_state_from_distribution(
        &self,
        sender_event_pubkey: PublicKey,
        dist: &SenderKeyDistribution,
    ) -> Result<()> {
        if self
            .load_sender_key_state(&sender_event_pubkey, dist.key_id)
            .is_some()
        {
            return Ok(());
        }

        let state = SenderKeyState::new(dist.key_id, dist.chain_key, dist.iteration);
        self.group_sender_key_states
            .lock()
            .unwrap()
            .insert((sender_event_pubkey, dist.key_id), state.clone());
        self.store_sender_key_state(&sender_event_pubkey, dist.key_id, &state)?;
        Ok(())
    }

    fn store_group_sender_event_info(
        &self,
        sender_event_pubkey: PublicKey,
        info: &GroupSenderEventInfo,
    ) -> Result<()> {
        self.group_sender_events
            .lock()
            .unwrap()
            .insert(sender_event_pubkey, info.clone());

        let stored = StoredGroupSenderEventInfo {
            group_id: info.group_id.clone(),
            sender_owner_pubkey: hex::encode(info.sender_owner_pubkey.to_bytes()),
            sender_device_pubkey: info
                .sender_device_pubkey
                .map(|pk| hex::encode(pk.to_bytes())),
        };
        let key = self.group_sender_event_info_key(&sender_event_pubkey);
        self.storage.put(&key, serde_json::to_string(&stored)?)?;

        let _ = self.subscribe_to_group_sender_event(sender_event_pubkey);
        Ok(())
    }

    fn maybe_handle_group_sender_key_distribution(
        &self,
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
        rumor: &UnsignedEvent,
    ) -> Result<()> {
        if rumor.kind.as_u16() != GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16 {
            return Ok(());
        }

        let tag_group_id = Self::tag_value(&rumor.tags, "l");
        let dist: SenderKeyDistribution = serde_json::from_str(&rumor.content)?;

        if let Some(ref gid) = tag_group_id {
            if dist.group_id != *gid {
                return Ok(());
            }
        }

        let Some(sender_event_hex) = dist.sender_event_pubkey.as_deref() else {
            return Ok(());
        };
        let Ok(sender_event_pubkey) = crate::utils::pubkey_from_hex(sender_event_hex) else {
            return Ok(());
        };

        let info = GroupSenderEventInfo {
            group_id: dist.group_id.clone(),
            sender_owner_pubkey: from_owner_pubkey,
            // Sender device must come from authenticated session context.
            // Never trust inner rumor `pubkey` for identity attribution.
            sender_device_pubkey: from_sender_device_pubkey,
        };
        self.store_group_sender_event_info(sender_event_pubkey, &info)?;
        self.ensure_sender_key_state_from_distribution(sender_event_pubkey, &dist)?;

        // Decrypt any queued outer events that were waiting for this sender key id.
        let pending = {
            let mut map = self.group_sender_key_pending.lock().unwrap();
            map.remove(&(sender_event_pubkey, dist.key_id))
                .unwrap_or_default()
        };
        if pending.is_empty() {
            return Ok(());
        }

        // Best-effort: process in message-number order to reduce skipped-key cache pressure.
        let one_to_many = OneToManyChannel::default();
        let mut pending = pending;
        pending.sort_by_key(|outer| {
            one_to_many
                .parse_outer_content(&outer.content)
                .map(|m| m.message_number)
                .unwrap_or(0)
        });

        for outer in pending {
            if let Some((sender, sender_device, plaintext, event_id)) =
                self.try_decrypt_group_sender_key_outer(&outer, Some(info.clone()))
            {
                let _ = self
                    .pubsub
                    .decrypted_message(sender, sender_device, plaintext, event_id);
            }
        }

        Ok(())
    }

    fn try_decrypt_group_sender_key_outer(
        &self,
        outer: &nostr::Event,
        info_hint: Option<GroupSenderEventInfo>,
    ) -> Option<(PublicKey, Option<PublicKey>, String, Option<String>)> {
        if outer.kind.as_u16() != crate::MESSAGE_EVENT_KIND as u16 {
            return None;
        }
        if outer.verify().is_err() {
            return None;
        }

        let sender_event_pubkey = outer.pubkey;
        let info = info_hint.or_else(|| self.load_group_sender_event_info(&sender_event_pubkey))?;

        let one_to_many = OneToManyChannel::default();
        let parsed = one_to_many.parse_outer_content(&outer.content).ok()?;

        let key_id = parsed.key_id;

        let mut state = match self.load_sender_key_state(&sender_event_pubkey, key_id) {
            Some(s) => s,
            None => {
                // Mapping exists, but we don't yet have this key id; queue until we receive
                // a distribution rumor over a 1:1 session.
                self.group_sender_key_pending
                    .lock()
                    .unwrap()
                    .entry((sender_event_pubkey, key_id))
                    .or_default()
                    .push(outer.clone());
                return None;
            }
        };

        let plaintext = parsed.decrypt(&mut state).ok()?;

        // Persist updated sender-key state.
        let _ = self.store_sender_key_state(&sender_event_pubkey, key_id, &state);
        self.group_sender_key_states
            .lock()
            .unwrap()
            .insert((sender_event_pubkey, key_id), state);

        // Ensure decrypted plaintext is a rumor-shaped JSON event so downstream callers can parse it.
        let plaintext = match serde_json::from_str::<UnsignedEvent>(&plaintext) {
            Ok(r) => {
                if let Some(inner_gid) = Self::tag_value(&r.tags, "l") {
                    if inner_gid != info.group_id {
                        return None;
                    }
                }
                serde_json::to_string(&r).ok()?
            }
            Err(_) => {
                let mut tags = Vec::new();
                if let Ok(tag) = Tag::parse(&["l".to_string(), info.group_id.clone()]) {
                    tags.push(tag);
                }
                let rumor = nostr::EventBuilder::new(
                    nostr::Kind::Custom(crate::CHAT_MESSAGE_KIND as u16),
                    &plaintext,
                )
                .tags(tags)
                .custom_created_at(outer.created_at)
                .build(info.sender_owner_pubkey);
                serde_json::to_string(&rumor).ok()?
            }
        };

        Some((
            info.sender_owner_pubkey,
            info.sender_device_pubkey,
            plaintext,
            Some(outer.id.to_string()),
        ))
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

    fn flush_message_queue(&self, device_identity: &str) -> Result<()> {
        let entries = self.message_queue.get_for_target(device_identity)?;
        if entries.is_empty() {
            return Ok(());
        }

        let owner_pubkey = {
            let records = self.user_records.lock().unwrap();
            records.iter().find_map(|(owner, user_record)| {
                user_record
                    .device_records
                    .contains_key(device_identity)
                    .then_some(*owner)
            })
        };
        let Some(owner_pubkey) = owner_pubkey else {
            return Ok(());
        };

        let mut sent: Vec<(String, Option<String>)> = Vec::new();
        {
            let mut records = self.user_records.lock().unwrap();
            let Some(user_record) = records.get_mut(&owner_pubkey) else {
                return Ok(());
            };
            let Some(device_record) = user_record.device_records.get_mut(device_identity) else {
                return Ok(());
            };
            let Some(session) = device_record.active_session.as_mut() else {
                return Ok(());
            };

            for entry in &entries {
                if let Ok(signed_event) = session.send_event(entry.event.clone()) {
                    if self.pubsub.publish_signed(signed_event).is_ok() {
                        sent.push((
                            entry.id.clone(),
                            entry.event.id.as_ref().map(|id| id.to_string()),
                        ));
                    }
                }
            }
        }

        for (entry_id, maybe_event_id) in sent {
            if let Some(event_id) = maybe_event_id {
                let _ = self
                    .message_queue
                    .remove_by_target_and_event_id(device_identity, &event_id);
            } else {
                let _ = self.message_queue.remove(&entry_id);
            }
        }

        let _ = self.store_user_record(&owner_pubkey);
        Ok(())
    }

    fn expand_discovery_queue(
        &self,
        owner_pubkey: PublicKey,
        devices: &[DeviceEntry],
    ) -> Result<()> {
        let entries = self
            .discovery_queue
            .get_for_target(&owner_pubkey.to_hex())?;
        if entries.is_empty() {
            return Ok(());
        }

        for entry in entries {
            for device in devices {
                let device_id = device.identity_pubkey.to_hex();
                if device_id == self.device_id {
                    continue;
                }
                let _ = self.message_queue.add(&device_id, &entry.event);
            }

            let _ = self.discovery_queue.remove(&entry.id);
        }

        Ok(())
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
        let _ = self.expand_discovery_queue(owner_pubkey, &devices);
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

        for device in &devices {
            self.subscribe_to_device_invite(owner_pubkey, device.identity_pubkey);
        }

        for device in &devices {
            let device_id = device.identity_pubkey.to_hex();
            if device_id == self.device_id {
                continue;
            }
            let has_active_session = {
                let records = self.user_records.lock().unwrap();
                records
                    .get(&owner_pubkey)
                    .and_then(|r| r.device_records.get(&device_id))
                    .and_then(|d| d.active_session.as_ref())
                    .is_some()
            };
            if has_active_session {
                let _ = self.flush_message_queue(&device_id);
            }
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
                    let _ = self.flush_message_queue(&device_id);

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
                let _ = self.accept_invite(&invite, None);
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
                let sender_device = if let Ok(sender_pk) = crate::utils::pubkey_from_hex(&device_id)
                {
                    let sender_owner = self.resolve_to_owner(&sender_pk);
                    if sender_owner != sender_pk
                        && !self.is_device_authorized(sender_owner, sender_pk)
                    {
                        return;
                    }
                    Some(sender_pk)
                } else {
                    None
                };

                if let Ok(rumor) = serde_json::from_str::<UnsignedEvent>(&plaintext) {
                    self.maybe_auto_adopt_chat_settings(owner_pubkey, &rumor);
                    let _ = self.maybe_handle_group_sender_key_distribution(
                        owner_pubkey,
                        sender_device,
                        &rumor,
                    );
                }

                let _ =
                    self.pubsub
                        .decrypted_message(owner_pubkey, sender_device, plaintext, event_id);
                let _ = self.store_user_record(&owner_pubkey);
            } else if let Some((sender, sender_device, plaintext, event_id)) =
                self.try_decrypt_group_sender_key_outer(&event, None)
            {
                let _ = self
                    .pubsub
                    .decrypted_message(sender, sender_device, plaintext, event_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;
    use std::sync::Arc;

    fn drain_events(
        rx: &crossbeam_channel::Receiver<SessionManagerEvent>,
    ) -> Vec<SessionManagerEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

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
    fn test_send_typing_does_not_record_in_message_history() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (tx, _rx) = crossbeam_channel::unbounded();
        let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

        let recipient = Keys::generate().public_key();
        manager.send_typing(recipient, None).unwrap();

        let history = manager.message_history.lock().unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_delete_chat_removes_local_state_and_allows_reinit() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (tx, _rx) = crossbeam_channel::unbounded();
        let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);
        manager.init().unwrap();

        let peer = Keys::generate().public_key();
        manager.setup_user(peer);
        assert!(manager.get_user_pubkeys().contains(&peer));

        manager.delete_chat(peer).unwrap();
        assert!(!manager.get_user_pubkeys().contains(&peer));

        manager.send_text(peer, "reinit".to_string(), None).unwrap();
        assert!(manager.get_user_pubkeys().contains(&peer));
    }

    #[test]
    fn group_sender_key_distribution_allows_decrypting_one_to_many_outer_messages() {
        let our_keys = Keys::generate();
        let our_pubkey = our_keys.public_key();
        let identity_key = our_keys.secret_key().to_secret_bytes();

        let storage = Arc::new(InMemoryStorage::new());
        let (tx, rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(
            our_pubkey,
            identity_key,
            "test-device".to_string(),
            our_pubkey,
            tx,
            Some(storage),
            None,
        );

        let group_id = "g1".to_string();

        let sender_owner_pubkey = Keys::generate().public_key();
        let sender_device_pubkey = Keys::generate().public_key();

        let sender_event_keys = Keys::generate();
        let sender_event_pubkey_hex = hex::encode(sender_event_keys.public_key().to_bytes());

        let key_id = 123u32;
        let chain_key = [7u8; 32];
        let dist = SenderKeyDistribution {
            group_id: group_id.clone(),
            key_id,
            chain_key,
            iteration: 0,
            created_at: 1,
            sender_event_pubkey: Some(sender_event_pubkey_hex.clone()),
        };
        let dist_json = serde_json::to_string(&dist).unwrap();

        let dist_rumor = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            &dist_json,
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(1))
        .build(sender_device_pubkey);

        manager
            .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist_rumor,
            )
            .unwrap();

        let events = drain_events(&rx);
        let expected_subid = format!(
            "group-sender-event-{}",
            hex::encode(sender_event_keys.public_key().to_bytes())
        );
        assert!(events.iter().any(|ev| match ev {
            SessionManagerEvent::Subscribe { subid, .. } => subid == &expected_subid,
            _ => false,
        }));

        let inner = nostr::EventBuilder::new(
            nostr::Kind::Custom(crate::CHAT_MESSAGE_KIND as u16),
            "hello",
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(10))
        .build(sender_device_pubkey);
        let inner_json = serde_json::to_string(&inner).unwrap();

        let mut sender_state = SenderKeyState::new(key_id, chain_key, 0);
        let outer = OneToManyChannel::default()
            .encrypt_to_outer_event(
                &sender_event_keys,
                &mut sender_state,
                &inner_json,
                nostr::Timestamp::from(10),
            )
            .unwrap();

        manager.process_received_event(outer.clone());

        let events = drain_events(&rx);
        let dec = events.iter().find_map(|ev| match ev {
            SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                event_id,
            } => Some((*sender, *sender_device, content.clone(), event_id.clone())),
            _ => None,
        });
        let (sender, sender_device, content, event_id) = dec.expect("expected decrypted message");
        assert_eq!(sender, sender_owner_pubkey);
        assert_eq!(sender_device, Some(sender_device_pubkey));
        assert_eq!(event_id, Some(outer.id.to_string()));

        let rumor: UnsignedEvent = serde_json::from_str(&content).unwrap();
        assert_eq!(u32::from(rumor.kind.as_u16()), crate::CHAT_MESSAGE_KIND);
        assert_eq!(rumor.content, "hello");
        assert_eq!(SessionManager::tag_value(&rumor.tags, "l"), Some(group_id));
    }

    #[test]
    fn group_sender_key_queues_outer_until_distribution_arrives_for_key_id() {
        let our_keys = Keys::generate();
        let our_pubkey = our_keys.public_key();
        let identity_key = our_keys.secret_key().to_secret_bytes();

        let storage = Arc::new(InMemoryStorage::new());
        let (tx, rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(
            our_pubkey,
            identity_key,
            "test-device".to_string(),
            our_pubkey,
            tx,
            Some(storage),
            None,
        );

        let group_id = "g1".to_string();
        let sender_owner_pubkey = Keys::generate().public_key();
        let sender_device_pubkey = Keys::generate().public_key();

        let sender_event_keys = Keys::generate();
        let sender_event_pubkey_hex = hex::encode(sender_event_keys.public_key().to_bytes());

        // First distribution establishes the sender-event pubkey mapping (key id 1).
        let dist1 = SenderKeyDistribution {
            group_id: group_id.clone(),
            key_id: 1,
            chain_key: [1u8; 32],
            iteration: 0,
            created_at: 1,
            sender_event_pubkey: Some(sender_event_pubkey_hex.clone()),
        };
        let dist1_rumor = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            &serde_json::to_string(&dist1).unwrap(),
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(1))
        .build(sender_device_pubkey);
        manager
            .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist1_rumor,
            )
            .unwrap();
        let _ = drain_events(&rx);

        // Now receive an outer message for a new key id (2) before we've seen its distribution.
        let key2 = 2u32;
        let chain2 = [2u8; 32];
        let inner = nostr::EventBuilder::new(
            nostr::Kind::Custom(crate::CHAT_MESSAGE_KIND as u16),
            "later",
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(10))
        .build(sender_device_pubkey);
        let inner_json = serde_json::to_string(&inner).unwrap();
        let mut sender_state = SenderKeyState::new(key2, chain2, 0);
        let outer = OneToManyChannel::default()
            .encrypt_to_outer_event(
                &sender_event_keys,
                &mut sender_state,
                &inner_json,
                nostr::Timestamp::from(10),
            )
            .unwrap();

        manager.process_received_event(outer.clone());
        assert!(
            drain_events(&rx)
                .iter()
                .all(|ev| !matches!(ev, SessionManagerEvent::DecryptedMessage { .. })),
            "outer should be queued until key distribution arrives"
        );

        // Distribution for key id 2 arrives; queued outer should now decrypt.
        let dist2 = SenderKeyDistribution {
            group_id: group_id.clone(),
            key_id: key2,
            chain_key: chain2,
            iteration: 0,
            created_at: 2,
            sender_event_pubkey: Some(sender_event_pubkey_hex),
        };
        let dist2_rumor = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            &serde_json::to_string(&dist2).unwrap(),
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(2))
        .build(sender_device_pubkey);
        manager
            .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist2_rumor,
            )
            .unwrap();

        let events = drain_events(&rx);
        let dec = events.iter().find_map(|ev| match ev {
            SessionManagerEvent::DecryptedMessage {
                sender, content, ..
            } => Some((*sender, content.clone())),
            _ => None,
        });
        let (sender, content) = dec.expect("expected decrypted queued message");
        assert_eq!(sender, sender_owner_pubkey);

        let rumor: UnsignedEvent = serde_json::from_str(&content).unwrap();
        assert_eq!(rumor.content, "later");
    }

    #[test]
    fn init_resubscribes_to_stored_group_sender_event_pubkeys() {
        let our_keys = Keys::generate();
        let our_pubkey = our_keys.public_key();

        let storage = Arc::new(InMemoryStorage::new());

        // First manager stores sender-event mapping in storage.
        {
            let (tx, _rx) = crossbeam_channel::unbounded();
            let manager = SessionManager::new(
                our_pubkey,
                our_keys.secret_key().to_secret_bytes(),
                "test-device".to_string(),
                our_pubkey,
                tx,
                Some(storage.clone()),
                None,
            );

            let group_id = "g1".to_string();
            let sender_owner_pubkey = Keys::generate().public_key();
            let sender_device_pubkey = Keys::generate().public_key();
            let sender_event_keys = Keys::generate();

            let dist = SenderKeyDistribution {
                group_id,
                key_id: 1,
                chain_key: [3u8; 32],
                iteration: 0,
                created_at: 1,
                sender_event_pubkey: Some(hex::encode(sender_event_keys.public_key().to_bytes())),
            };
            let dist_rumor = nostr::EventBuilder::new(
                nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
                &serde_json::to_string(&dist).unwrap(),
            )
            .tag(Tag::parse(&["l".to_string(), dist.group_id.clone()]).unwrap())
            .custom_created_at(nostr::Timestamp::from(1))
            .build(sender_device_pubkey);

            manager
                .maybe_handle_group_sender_key_distribution(
                    sender_owner_pubkey,
                    Some(sender_device_pubkey),
                    &dist_rumor,
                )
                .unwrap();
        }

        let (tx, rx) = crossbeam_channel::unbounded();
        let manager = SessionManager::new(
            our_pubkey,
            our_keys.secret_key().to_secret_bytes(),
            "test-device".to_string(),
            our_pubkey,
            tx,
            Some(storage),
            None,
        );
        manager.init().unwrap();

        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| matches!(ev, SessionManagerEvent::Subscribe { subid, .. } if subid.starts_with("group-sender-event-"))),
            "expected group sender-key subscription on init"
        );
    }

    #[test]
    fn queued_message_survives_restart_and_flushes_after_session_creation() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_keys = Keys::generate();
        let bob_pubkey = bob_keys.public_key();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

        let (tx1, _rx1) = crossbeam_channel::unbounded();
        let manager1 = SessionManager::new(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            tx1,
            Some(storage.clone()),
            None,
        );
        manager1.init().unwrap();

        let (inner_id, published_ids) = manager1
            .send_text_with_inner_id(bob_pubkey, "queued before restart".to_string(), None)
            .unwrap();
        assert!(published_ids.is_empty());
        assert!(
            !storage.list("v1/discovery-queue/").unwrap().is_empty(),
            "expected discovery queue entries when recipient devices are unknown"
        );

        drop(manager1);

        let (tx2, rx2) = crossbeam_channel::unbounded();
        let manager2 = SessionManager::new(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            tx2,
            Some(storage.clone()),
            None,
        );
        manager2.init().unwrap();
        let _ = drain_events(&rx2);

        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
        let app_keys_event = app_keys
            .get_event(bob_pubkey)
            .sign_with_keys(&bob_keys)
            .unwrap();
        manager2.process_received_event(app_keys_event);

        let bob_device_id = bob_pubkey.to_hex();
        let queued_keys = storage.list("v1/message-queue/").unwrap();
        assert!(
            queued_keys
                .iter()
                .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
            "expected discovery entry to expand into message queue for bob device"
        );

        let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
        let invite_event = invite
            .get_event()
            .unwrap()
            .sign_with_keys(&bob_keys)
            .unwrap();
        manager2.process_received_event(invite_event);

        let events = drain_events(&rx2);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected queued message to be published after session creation"
        );

        let remaining_keys = storage.list("v1/message-queue/").unwrap();
        assert!(
            !remaining_keys
                .iter()
                .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
            "expected queue entry to be removed after successful publish"
        );
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
