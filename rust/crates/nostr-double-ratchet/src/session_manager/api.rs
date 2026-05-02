use super::*;

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
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(crate::pubsub::DedupingPubSub::new(pubsub));
        let storage = storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new()));
        let message_queue = MessageQueue::new(storage.clone(), "v1/message-queue/");
        let discovery_queue = MessageQueue::new(storage.clone(), "v1/discovery-queue/");
        Self {
            user_records: SessionBookActor::new(),
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            storage,
            pubsub,
            initialized: Arc::new(Mutex::new(false)),
            invite_states: Arc::new(Mutex::new(Vec::new())),
            provided_invite: invite,
            delegate_to_owner: Arc::new(Mutex::new(HashMap::new())),
            cached_app_keys: Arc::new(Mutex::new(HashMap::new())),
            processed_invite_responses: Arc::new(Mutex::new(HashSet::new())),
            pending_invite_responses: Arc::new(Mutex::new(VecDeque::new())),
            message_history: Arc::new(Mutex::new(HashMap::new())),
            latest_app_keys_created_at: Arc::new(Mutex::new(HashMap::new())),
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
        self.with_user_records({
            let owner_public_key = self.owner_public_key;
            let device_id = self.device_id.clone();
            move |records| {
                let record = records
                    .entry(owner_public_key)
                    .or_insert_with(|| UserRecord::new(hex::encode(owner_public_key.to_bytes())));
                if !record.device_records.contains_key(&device_id) {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    record.device_records.insert(
                        device_id.clone(),
                        crate::DeviceRecord {
                            device_id: device_id.clone(),
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
            }
        });

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

        self.register_invite_inner(invite.clone(), true)?;

        let active_device_ids = self.with_user_records({
            move |records| {
                records
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
                    .collect::<Vec<_>>()
            }
        });

        for device_id in active_device_ids {
            let _ = self.flush_message_queue(&device_id);
        }

        // Start listening for AppKeys for our owner (to discover sibling devices)
        self.setup_user(self.owner_public_key);

        Ok(())
    }

    pub fn reload_from_storage(&self) -> Result<()> {
        self.load_all_user_records()
    }

    fn register_invite_inner(&self, invite: Invite, publish: bool) -> Result<()> {
        if invite.inviter_ephemeral_private_key.is_none() {
            return Err(crate::Error::Invite(
                "Invite missing ephemeral keys".to_string(),
            ));
        }

        let response_pubkey = invite.inviter_ephemeral_public_key;
        let should_subscribe = {
            let mut states = self.invite_states.lock().unwrap();
            if let Some(existing) = states
                .iter_mut()
                .find(|state| state.invite.inviter_ephemeral_public_key == response_pubkey)
            {
                existing.invite = invite.clone();
                false
            } else {
                states.push(InviteState {
                    invite: invite.clone(),
                    our_identity_key: self.our_identity_key,
                });
                true
            }
        };

        if should_subscribe {
            invite.listen_with_pubsub(self.pubsub.as_ref())?;
        }

        if publish {
            if let Ok(unsigned) = invite.get_event() {
                let keys = Keys::new(nostr::SecretKey::from_slice(&self.our_identity_key)?);
                if let Ok(signed) = unsigned.sign_with_keys(&keys) {
                    let _ = self.pubsub.publish_signed(signed);
                }
            }
        }

        Ok(())
    }

    /// Register an additional local invite for response handling without
    /// publishing it as a relay-discoverable invite event.
    pub fn register_invite(&self, invite: Invite) -> Result<()> {
        self.init()?;
        self.register_invite_inner(invite, false)
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

    /// Remove discovery queue entries older than `max_age_ms` milliseconds.
    pub fn cleanup_discovery_queue(&self, max_age_ms: u64) -> Result<usize> {
        self.discovery_queue.remove_expired(max_age_ms)
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
        let mut tags: Vec<Tag> = Vec::new();
        crate::append_expiration_tag(&mut tags, &options, Self::current_unix_seconds())?;

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
        let payload = Self::chat_settings_payload(message_ttl_seconds);
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
        let opts = Self::send_options_for_chat_ttl(message_ttl_seconds);
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
        crate::append_expiration_tag(&mut tags, &options, Self::current_unix_seconds())?;

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
        let mut tags: Vec<Tag> = Vec::new();
        crate::append_expiration_tag(&mut tags, &options, Self::current_unix_seconds())?;

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
        crate::append_expiration_tag(&mut tags, &options, Self::current_unix_seconds())?;

        let event = self.build_message_event(recipient, crate::REACTION_KIND, emoji, tags)?;

        self.send_event(recipient, event)
    }

    pub fn get_device_id(&self) -> &str {
        &self.device_id
    }

    pub fn current_device_invite_response_pubkey(&self) -> Option<PublicKey> {
        self.current_device_invite_response_pubkeys()
            .into_iter()
            .next()
    }

    pub fn current_device_invite_response_pubkeys(&self) -> Vec<PublicKey> {
        self.invite_states
            .lock()
            .unwrap()
            .iter()
            .map(|state| state.invite.inviter_ephemeral_public_key)
            .collect()
    }

    pub fn get_user_pubkeys(&self) -> Vec<PublicKey> {
        self.with_user_records(|records| records.keys().copied().collect())
    }

    pub fn known_peer_owner_pubkeys(&self) -> Vec<PublicKey> {
        let owner_public_key = self.owner_public_key;
        let mut pubkeys = self.with_user_records(move |records| {
            records
                .keys()
                .copied()
                .filter(|pubkey| *pubkey != owner_public_key)
                .collect::<HashSet<_>>()
        });

        if let Ok(stored_keys) = self.storage.list(&self.user_record_key_prefix()) {
            let prefix = self.user_record_key_prefix();
            for key in stored_keys {
                let Some(pubkey_hex) = key.strip_prefix(&prefix) else {
                    continue;
                };
                let Ok(pubkey) = crate::utils::pubkey_from_hex(pubkey_hex) else {
                    continue;
                };
                if pubkey != owner_public_key {
                    pubkeys.insert(pubkey);
                }
            }
        }

        let mut pubkeys: Vec<PublicKey> = pubkeys.into_iter().collect();
        pubkeys.sort_by_key(|pubkey| pubkey.to_hex());
        pubkeys
    }

    pub(crate) fn known_device_identity_pubkeys_for_owners(
        &self,
        owners: impl IntoIterator<Item = PublicKey>,
    ) -> Vec<PublicKey> {
        let owners = owners.into_iter().collect::<HashSet<_>>();
        if owners.is_empty() {
            return Vec::new();
        }

        let mut devices = HashSet::new();
        {
            let cached_app_keys = self.cached_app_keys.lock().unwrap();
            for owner in &owners {
                if let Some(app_keys) = cached_app_keys.get(owner) {
                    devices.extend(
                        app_keys
                            .get_all_devices()
                            .into_iter()
                            .map(|device| device.identity_pubkey),
                    );
                }
            }
        }

        let stored_devices = self.with_user_records({
            let owners = owners.clone();
            move |records| {
                let mut devices = HashSet::new();
                for owner in &owners {
                    let Some(record) = records.get(owner) else {
                        continue;
                    };
                    for identity_hex in &record.known_device_identities {
                        if let Ok(pubkey) = crate::utils::pubkey_from_hex(identity_hex) {
                            devices.insert(pubkey);
                        }
                    }
                    for device_id in record.device_records.keys() {
                        if let Ok(pubkey) = crate::utils::pubkey_from_hex(device_id) {
                            devices.insert(pubkey);
                        }
                    }
                }
                devices
            }
        });
        devices.extend(stored_devices);

        let mut devices = devices.into_iter().collect::<Vec<_>>();
        devices.sort_by_key(|pubkey| pubkey.to_hex());
        devices
    }

    pub fn known_device_identity_pubkeys_for_owner(
        &self,
        owner_pubkey: PublicKey,
    ) -> Vec<PublicKey> {
        self.known_device_identity_pubkeys_for_owners([owner_pubkey])
    }

    pub fn get_stored_user_record_json(
        &self,
        peer_owner_pubkey: PublicKey,
    ) -> Result<Option<String>> {
        if let Some(json) = self.with_user_records(move |records| {
            records
                .get(&peer_owner_pubkey)
                .map(Self::stored_user_record_json)
                .transpose()
        })? {
            return Ok(Some(json));
        }

        let key = self.user_record_key(&peer_owner_pubkey);
        self.storage.get(&key)
    }

    pub fn get_message_push_author_pubkeys(&self, peer_owner_pubkey: PublicKey) -> Vec<PublicKey> {
        let mut pubkeys: Vec<PublicKey> = self
            .get_message_push_session_states(peer_owner_pubkey)
            .into_iter()
            .flat_map(|snapshot| snapshot.tracked_sender_pubkeys)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        pubkeys.sort_by_key(|pubkey| pubkey.to_hex());
        pubkeys
    }

    pub fn get_all_message_push_author_pubkeys(&self) -> Vec<PublicKey> {
        self.with_user_records(|records| Self::message_push_author_pubkeys_for_records(records))
    }

    pub fn get_message_push_session_states(
        &self,
        peer_owner_pubkey: PublicKey,
    ) -> Vec<MessagePushSessionStateSnapshot> {
        self.with_user_records(move |records| {
            records
                .get(&peer_owner_pubkey)
                .map(Self::message_push_session_snapshots)
                .unwrap_or_default()
        })
    }

    pub fn get_total_sessions(&self) -> usize {
        self.with_user_records(|records| {
            records
                .values()
                .map(|ur| {
                    ur.device_records
                        .values()
                        .filter(|dr| dr.active_session.is_some())
                        .count()
                })
                .sum()
        })
    }

    pub fn import_session_state(
        &self,
        peer_pubkey: PublicKey,
        device_id: Option<String>,
        state: crate::SessionState,
    ) -> Result<()> {
        let device_identity = device_id
            .as_deref()
            .and_then(|id| crate::utils::pubkey_from_hex(id).ok());
        let session = crate::Session::new(state, "imported".to_string());

        self.with_user_records(move |records| {
            let user_record = records
                .entry(peer_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(peer_pubkey.to_bytes())));
            user_record.upsert_session(device_id.as_deref(), session);
            if let Some(device_id) = device_id.as_deref() {
                if crate::utils::pubkey_from_hex(device_id).is_ok()
                    && !user_record
                        .known_device_identities
                        .iter()
                        .any(|known| known == device_id)
                {
                    user_record
                        .known_device_identities
                        .push(device_id.to_string());
                }
            }
        });

        if let Some(device_identity) = device_identity {
            self.delegate_to_owner
                .lock()
                .unwrap()
                .insert(device_identity, peer_pubkey);
        }
        let _ = self.store_user_record(&peer_pubkey);
        Ok(())
    }

    pub fn export_active_session_state(
        &self,
        peer_pubkey: PublicKey,
    ) -> Result<Option<crate::SessionState>> {
        Ok(self.with_user_records(move |records| {
            let user_record = records.get_mut(&peer_pubkey)?;

            let mut sessions = user_record.get_active_sessions_mut();
            sessions.first_mut().map(|session| session.state.clone())
        }))
    }

    pub fn export_active_sessions(&self) -> Vec<(PublicKey, String, crate::SessionState)> {
        self.with_user_records(|records| {
            let mut out = Vec::new();

            for (owner_pubkey, user_record) in records.iter() {
                for (device_id, device_record) in user_record.device_records.iter() {
                    if let Some(session) = &device_record.active_session {
                        out.push((*owner_pubkey, device_id.clone(), session.state.clone()));
                    }
                }
            }

            out
        })
    }

    pub fn debug_session_keys(&self) -> String {
        self.with_user_records(|records| {
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
                            &hex::encode(session.state.our_next_nostr_key.public_key.to_bytes())
                                [..16]
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
        })
    }

    pub fn get_our_pubkey(&self) -> PublicKey {
        self.our_public_key
    }

    pub fn get_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key
    }

    pub fn ingest_app_keys_snapshot(
        &self,
        owner_pubkey: PublicKey,
        app_keys: AppKeys,
        created_at: u64,
    ) {
        self.handle_app_keys_event(owner_pubkey, app_keys, created_at);
    }

    pub fn pending_invite_response_owner_pubkeys(&self) -> Vec<PublicKey> {
        let states = self.invite_states.lock().unwrap().clone();
        if states.is_empty() {
            return Vec::new();
        }

        let processed = self.processed_invite_responses.lock().unwrap().clone();
        let pending_events: Vec<nostr::Event> = self
            .pending_invite_responses
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();

        let mut owners = HashSet::new();
        for event in pending_events {
            if processed.contains(&event.id.to_string()) {
                continue;
            }

            for state in &states {
                let Ok(Some(response)) = state
                    .invite
                    .process_invite_response(&event, state.our_identity_key)
                else {
                    continue;
                };
                if !self.invite_response_has_capacity(
                    state.invite.inviter_ephemeral_public_key,
                    response.invitee_identity,
                ) {
                    continue;
                }

                owners.insert(
                    response
                        .owner_public_key
                        .unwrap_or_else(|| self.resolve_to_owner(&response.invitee_identity)),
                );
                break;
            }
        }

        let mut owners: Vec<PublicKey> = owners.into_iter().collect();
        owners.sort_by_key(|pubkey| pubkey.to_hex());
        owners
    }
}
