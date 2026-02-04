use crate::{pubsub::NostrPubSub, InMemoryStorage, Invite, Result, StorageAdapter, UserRecord};
use nostr::PublicKey;
use nostr::UnsignedEvent;
use std::collections::{HashMap, VecDeque};
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

struct PendingMessage {
    text: String,
    #[allow(dead_code)]
    timestamp: u64,
    first_sent_at: Option<u64>,
}

pub struct SessionManager {
    user_records: Arc<Mutex<HashMap<PublicKey, UserRecord>>>,
    our_public_key: PublicKey,
    our_identity_key: [u8; 32],
    device_id: String,
    storage: Arc<dyn StorageAdapter>,
    pubsub: Arc<dyn NostrPubSub>,
    initialized: Arc<Mutex<bool>>,
    invite_state: Arc<Mutex<Option<InviteState>>>,
    pending_invites: Arc<Mutex<HashMap<PublicKey, Invite>>>,
    pending_messages: Arc<Mutex<HashMap<PublicKey, VecDeque<PendingMessage>>>>,
}

impl SessionManager {
    pub fn new(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        event_tx: crossbeam_channel::Sender<SessionManagerEvent>,
        storage: Option<Arc<dyn StorageAdapter>>,
    ) -> Self {
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(event_tx);
        Self::new_with_pubsub(our_public_key, our_identity_key, device_id, pubsub, storage)
    }

    pub fn new_with_pubsub(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        pubsub: Arc<dyn NostrPubSub>,
        storage: Option<Arc<dyn StorageAdapter>>,
    ) -> Self {
        Self {
            user_records: Arc::new(Mutex::new(HashMap::new())),
            our_public_key,
            our_identity_key,
            device_id,
            storage: storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            pubsub,
            initialized: Arc::new(Mutex::new(false)),
            invite_state: Arc::new(Mutex::new(None)),
            pending_invites: Arc::new(Mutex::new(HashMap::new())),
            pending_messages: Arc::new(Mutex::new(HashMap::new())),
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

        let device_invite_key = self.device_invite_key(&self.device_id);
        let invite = match self.storage.get(&device_invite_key)? {
            Some(data) => Invite::deserialize(&data)?,
            None => Invite::create_new(self.our_public_key, Some(self.device_id.clone()), None)?,
        };

        self.storage.put(&device_invite_key, invite.serialize()?)?;

        *self.invite_state.lock().unwrap() = Some(InviteState {
            invite: invite.clone(),
            our_identity_key: self.our_identity_key,
        });

        // Subscribe to invite responses using Invite's own filter (with #p tag)
        invite.listen_with_pubsub(self.pubsub.as_ref())?;

        let unsigned_event = invite.get_event()?;
        let keys = nostr::Keys::new(nostr::SecretKey::from_slice(&self.our_identity_key)?);
        let signed_event = unsigned_event
            .sign_with_keys(&keys)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?;
        self.pubsub.publish_signed(signed_event)?;

        // Sessions manage their own kind 1060 subscriptions
        // Load existing sessions and set up their subscriptions
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

        Ok(())
    }

    pub fn send_text(&self, recipient: PublicKey, text: String) -> Result<Vec<String>> {
        eprintln!(
            "🚀 send_text called: recipient={}, text_len={}",
            &hex::encode(recipient.to_bytes())[..16],
            text.len()
        );

        if text.trim().is_empty() {
            eprintln!("⚠️  Ignoring empty text send");
            return Ok(Vec::new());
        }

        let mut event_ids = Vec::new();
        let mut user_records = self.user_records.lock().unwrap();

        // Check if recipient has any active sessions
        let has_recipient_sessions = user_records
            .get(&recipient)
            .map(|record| {
                let count = record
                    .device_records
                    .values()
                    .filter(|dr| dr.active_session.is_some())
                    .count();
                eprintln!(
                    "  Recipient {} has {} active sessions",
                    &hex::encode(recipient.to_bytes())[..16],
                    count
                );
                count > 0
            })
            .unwrap_or_else(|| {
                eprintln!(
                    "  Recipient {} not in user_records yet",
                    &hex::encode(recipient.to_bytes())[..16]
                );
                false
            });

        // If no sessions exist, queue the message and setup user to fetch invites (if not already setup)
        if !has_recipient_sessions {
            drop(user_records);

            // Setup recipient to subscribe to their invites (only if not already done)
            let needs_setup = {
                let pending = self.pending_invites.lock().unwrap();
                !pending.contains_key(&recipient)
            };

            if needs_setup {
                if recipient != self.our_public_key {
                    let _ = self.setup_user(recipient);
                }
                let _ = self.setup_user(self.our_public_key);
            }

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let pending_msg = PendingMessage {
                text: text.clone(),
                timestamp,
                first_sent_at: None,
            };

            self.pending_messages
                .lock()
                .unwrap()
                .entry(recipient)
                .or_default()
                .push_back(pending_msg);

            eprintln!(
                "📮 Queued message to {} (no active sessions)",
                &hex::encode(recipient.to_bytes())[..16]
            );

            return Ok(event_ids);
        }

        // Build event with p-tag for recipient (iris-client compatibility)
        let event = nostr::EventBuilder::text_note(&text)
            .tag(
                nostr::Tag::parse(&["p".to_string(), hex::encode(recipient.to_bytes())])
                    .map_err(|e| crate::Error::InvalidEvent(e.to_string()))?,
            )
            .build(self.our_public_key);

        // Send to recipient's devices
        if let Some(recipient_record) = user_records.get_mut(&recipient) {
            for session in recipient_record.get_active_sessions_mut().iter_mut() {
                match session.send_event(event.clone()) {
                    Ok(signed_event) => {
                        event_ids.push(signed_event.id.to_string());
                        let _ = self.pubsub.publish_signed(signed_event);
                    }
                    Err(_) => continue,
                }
            }
        }

        // Send to own devices (for multi-device sync), unless sending to self
        if recipient != self.our_public_key {
            if let Some(self_record) = user_records.get_mut(&self.our_public_key) {
                for session in self_record.get_active_sessions_mut().iter_mut() {
                    match session.send_event(event.clone()) {
                        Ok(signed_event) => {
                            event_ids.push(signed_event.id.to_string());
                            let _ = self.pubsub.publish_signed(signed_event);
                        }
                        Err(_) => continue,
                    }
                }
            }
        }

        if !event_ids.is_empty() {
            eprintln!(
                "📤 Sent message to {} ({} sessions)",
                &hex::encode(recipient.to_bytes())[..16],
                event_ids.len()
            );
        }

        drop(user_records);
        let _ = self.store_user_record(&recipient);
        if recipient != self.our_public_key {
            let _ = self.store_user_record(&self.our_public_key);
        }

        // If we successfully sent to at least one session, try to flush any pending messages
        if !event_ids.is_empty() {
            self.flush_pending_messages(recipient);
        }

        Ok(event_ids)
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

    fn device_invite_key(&self, device_id: &str) -> String {
        format!("device-invite/{}", device_id)
    }

    fn load_all_user_records(&self) -> Result<()> {
        let mut keys = self.storage.list("user/")?;
        if keys.is_empty() {
            keys = self.storage.list("user_")?;
        }

        let mut records = self.user_records.lock().unwrap();

        for key in keys {
            let json = match self.storage.get(&key)? {
                Some(v) => v,
                None => continue,
            };

            let stored: crate::StoredUserRecord = serde_json::from_str(&json)?;
            let user_pubkey = crate::utils::pubkey_from_hex(&stored.user_id)?;
            let mut user_record = UserRecord::new(stored.user_id.clone());

            for device in stored.devices {
                let mut active_session = device
                    .active_session
                    .map(|state| crate::Session::new(state, "restored".to_string()));
                let inactive_sessions = device
                    .inactive_sessions
                    .into_iter()
                    .map(|state| crate::Session::new(state, "restored".to_string()))
                    .collect::<Vec<_>>();

                if let Some(ref mut session) = active_session {
                    session.set_pubsub(self.pubsub.clone());
                }

                let mut device_record = crate::DeviceRecord {
                    device_id: device.device_id.clone(),
                    public_key: stored.user_id.clone(),
                    active_session,
                    inactive_sessions,
                    is_stale: device.is_stale,
                    stale_timestamp: device.stale_timestamp,
                    last_activity: device.last_activity,
                };

                for session in device_record.inactive_sessions.iter_mut() {
                    session.set_pubsub(self.pubsub.clone());
                }

                user_record
                    .device_records
                    .insert(device_record.device_id.clone(), device_record);
            }

            records.insert(user_pubkey, user_record);
        }

        Ok(())
    }

    fn store_user_record(&self, pubkey: &PublicKey) -> Result<()> {
        let user_records = self.user_records.lock().unwrap();
        if let Some(user_record) = user_records.get(pubkey) {
            let stored = user_record.to_stored();
            let key = format!("user/{}", hex::encode(pubkey.to_bytes()));
            let json = serde_json::to_string(&stored)?;
            self.storage.put(&key, json)?;
        }
        Ok(())
    }

    fn flush_pending_messages(&self, recipient: PublicKey) {
        let mut pending_messages = self.pending_messages.lock().unwrap();

        if let Some(message_queue) = pending_messages.get_mut(&recipient) {
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let mut messages_to_process = Vec::new();

            // Process all pending messages
            while let Some(msg) = message_queue.pop_front() {
                messages_to_process.push(msg);
            }

            drop(pending_messages);

            // Try to send each pending message
            for mut pending_msg in messages_to_process {
                match self.send_text(recipient, pending_msg.text.clone()) {
                    Ok(event_ids) if !event_ids.is_empty() => {
                        // Successfully sent to at least one session
                        tracing::info!(
                            "Flushed pending message to {} (sent to {} sessions)",
                            hex::encode(recipient.to_bytes()),
                            event_ids.len()
                        );

                        // Mark first send time if not set
                        if pending_msg.first_sent_at.is_none() {
                            pending_msg.first_sent_at = Some(current_time);
                        }

                        // Keep in queue for 5 seconds after first send (for additional devices)
                        if let Some(first_sent) = pending_msg.first_sent_at {
                            if current_time - first_sent < 5 {
                                self.pending_messages
                                    .lock()
                                    .unwrap()
                                    .entry(recipient)
                                    .or_default()
                                    .push_back(pending_msg);
                            } else {
                                tracing::info!("Removing message from queue (sent 5+ seconds ago)");
                            }
                        }
                    }
                    _ => {
                        // Failed to send or no sessions - re-queue the message
                        self.pending_messages
                            .lock()
                            .unwrap()
                            .entry(recipient)
                            .or_default()
                            .push_back(pending_msg);
                    }
                }
            }
        }
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) -> Result<()> {
        // Check if already set up (has sessions or pending invite)
        {
            let user_records = self.user_records.lock().unwrap();
            if user_records.contains_key(&user_pubkey) {
                return Ok(());
            }
        }

        {
            let pending = self.pending_invites.lock().unwrap();
            if pending.contains_key(&user_pubkey) {
                return Ok(());
            }
        }

        crate::Invite::from_user_with_pubsub(user_pubkey, self.pubsub.as_ref())?;

        // Create a placeholder invite to track that we're fetching
        let placeholder = Invite {
            inviter_ephemeral_public_key: nostr::Keys::generate().public_key(),
            shared_secret: [0u8; 32],
            inviter: user_pubkey,
            inviter_ephemeral_private_key: None,
            device_id: None,
            max_uses: None,
            used_by: Vec::new(),
            created_at: 0,
        };
        self.pending_invites
            .lock()
            .unwrap()
            .insert(user_pubkey, placeholder);

        Ok(())
    }

    pub fn process_received_event(&self, event: nostr::Event) {
        if event.kind.as_u16() == crate::INVITE_RESPONSE_KIND as u16 {
            if let Some(state) = self.invite_state.lock().unwrap().as_ref() {
                match state
                    .invite
                    .process_invite_response(&event, state.our_identity_key)
                {
                    Ok(Some((mut sess, invitee_pubkey, device_id))) => {
                        tracing::info!(
                            "✅ Accepted invite response from {}",
                            &hex::encode(invitee_pubkey.to_bytes())[..16]
                        );

                        if let Some(ref dev_id) = device_id {
                            if dev_id != &self.device_id {
                                let acceptance_key = format!(
                                    "invite-accept/{}/{}",
                                    hex::encode(invitee_pubkey.to_bytes()),
                                    dev_id
                                );
                                if self.storage.get(&acceptance_key).ok().flatten().is_none() {
                                    let _ = self.storage.put(&acceptance_key, "1".to_string());

                                    sess.set_pubsub(self.pubsub.clone());
                                    let _ = sess.subscribe_to_messages();

                                    let mut records = self.user_records.lock().unwrap();
                                    let user_record =
                                        records.entry(invitee_pubkey).or_insert_with(|| {
                                            UserRecord::new(hex::encode(invitee_pubkey.to_bytes()))
                                        });
                                    user_record.upsert_session(Some(dev_id), sess);
                                    drop(records);

                                    let _ = self.store_user_record(&invitee_pubkey);

                                    // Try to flush any pending messages for this user
                                    self.flush_pending_messages(invitee_pubkey);
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
        } else if event.kind.as_u16() == crate::INVITE_EVENT_KIND as u16 {
            if let Ok(invite) = Invite::from_event(&event) {
                tracing::info!(
                    "📨 Received invite from {}",
                    &hex::encode(invite.inviter.to_bytes())[..16]
                );

                if let Some(ref dev_id) = invite.device_id {
                    let inviter = invite.inviter;

                    // Check if we already have a session with this user/device
                    let mut records = self.user_records.lock().unwrap();
                    let user_record = records
                        .entry(inviter)
                        .or_insert_with(|| UserRecord::new(hex::encode(inviter.to_bytes())));

                    // Only accept if we don't already have a session for this device
                    if !user_record.device_records.contains_key(dev_id) {
                        drop(records);

                        if let Ok((mut session, event)) = invite.accept(
                            self.our_public_key,
                            self.our_identity_key,
                            Some(self.device_id.clone()),
                        ) {
                            tracing::info!(
                                "✅ Accepting invite from {}",
                                &hex::encode(inviter.to_bytes())[..16]
                            );
                            let _ = self.pubsub.publish_signed(event);

                            session.set_pubsub(self.pubsub.clone());
                            let _ = session.subscribe_to_messages();

                            let mut records = self.user_records.lock().unwrap();
                            let user_record = records.entry(inviter).or_insert_with(|| {
                                UserRecord::new(hex::encode(inviter.to_bytes()))
                            });
                            user_record.upsert_session(Some(dev_id), session);
                            drop(records);

                            let _ = self.store_user_record(&inviter);

                            // Try to flush any pending messages for this user
                            self.flush_pending_messages(inviter);
                        }
                    }
                }
            }
        } else if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16 {
            let event_id = Some(event.id.to_string());
            let mut user_records = self.user_records.lock().unwrap();

            for (user_pubkey, user_record) in user_records.iter_mut() {
                let mut all_sessions = user_record.get_all_sessions_mut();

                for session in all_sessions.iter_mut() {
                    if let Ok(Some(plaintext)) = session.receive(&event) {
                        tracing::info!(
                            "💬 Decrypted message from {}: {}",
                            &hex::encode(user_pubkey.to_bytes())[..16],
                            &plaintext[..plaintext.len().min(50)]
                        );
                        let sender = *user_pubkey;
                        drop(user_records);
                        let _ = self.pubsub.decrypted_message(sender, plaintext, event_id);
                        let _ = self.store_user_record(&sender);
                        return;
                    }
                }
            }

            drop(user_records);
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

        let manager = SessionManager::new(pubkey, identity_key, device_id.clone(), tx, None);

        assert_eq!(manager.get_device_id(), device_id);
    }

    #[test]
    fn test_send_text_no_sessions() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (tx, _rx) = crossbeam_channel::unbounded();

        let manager = SessionManager::new(pubkey, identity_key, device_id, tx, None);

        let recipient = Keys::generate().public_key();
        let result = manager.send_text(recipient, "test".to_string());

        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }
}
