use super::*;

impl SessionManager {
    pub(super) fn resolve_to_owner(&self, pubkey: &PublicKey) -> PublicKey {
        self.delegate_to_owner
            .lock()
            .unwrap()
            .get(pubkey)
            .copied()
            .unwrap_or(*pubkey)
    }

    pub(super) fn update_delegate_mapping(&self, owner_pubkey: PublicKey, app_keys: &AppKeys) {
        let new_identities: HashSet<String> = app_keys
            .get_all_devices()
            .into_iter()
            .map(|d| hex::encode(d.identity_pubkey.to_bytes()))
            .collect();

        let old_identities = self.with_user_records({
            let new_identity_list = new_identities.iter().cloned().collect::<Vec<_>>();
            move |records| {
                let user_record = records
                    .entry(owner_pubkey)
                    .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
                let old_identities = user_record.known_device_identities.clone();
                user_record.known_device_identities = new_identity_list;
                old_identities
            }
        });

        // Remove stale mappings
        for identity_hex in old_identities.iter() {
            if !new_identities.contains(identity_hex) {
                if let Ok(pk) = crate::utils::pubkey_from_hex(identity_hex) {
                    self.delegate_to_owner.lock().unwrap().remove(&pk);
                }
                let _ = self.message_queue.remove_for_target(identity_hex);
            }
        }

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

        let _ = self.store_user_record(&owner_pubkey);
    }

    pub(super) fn is_device_authorized(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
    ) -> bool {
        if owner_pubkey == device_pubkey {
            return true;
        }

        if let Some(app_keys) = self.cached_app_keys.lock().unwrap().get(&owner_pubkey) {
            return app_keys.get_device(&device_pubkey).is_some();
        }

        self.with_user_records(move |records| {
            records
                .get(&owner_pubkey)
                .map(|record| {
                    let device_hex = hex::encode(device_pubkey.to_bytes());
                    record.known_device_identities.contains(&device_hex)
                })
                .unwrap_or(false)
        })
    }

    pub(super) fn queue_pending_invite_response(&self, event: nostr::Event) {
        let mut pending = self.pending_invite_responses.lock().unwrap();
        if pending.iter().any(|existing| existing.id == event.id) {
            return;
        }
        pending.push_back(event);
        if pending.len() > MAX_PENDING_INVITE_RESPONSES {
            pending.pop_front();
        }
    }

    pub(super) fn install_invite_response_session(
        &self,
        event_id: String,
        response: crate::InviteResponse,
    ) -> bool {
        if response.invitee_identity == self.our_public_key {
            return false;
        }

        let owner_pubkey = response
            .owner_public_key
            .unwrap_or_else(|| self.resolve_to_owner(&response.invitee_identity));

        if !self.is_device_authorized(owner_pubkey, response.invitee_identity) {
            return false;
        }

        self.record_known_device_identity(owner_pubkey, response.invitee_identity);

        let device_id = response
            .device_id
            .unwrap_or_else(|| hex::encode(response.invitee_identity.to_bytes()));

        let session = response.session;

        self.with_user_records({
            let device_id = device_id.clone();
            move |records| {
                let user_record = records
                    .entry(owner_pubkey)
                    .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
                SessionManager::upsert_device_record(user_record, &device_id);
                user_record.upsert_session(Some(&device_id), session);
            }
        });

        let _ = self.store_user_record(&owner_pubkey);
        self.send_message_history(owner_pubkey, &device_id);
        let _ = self.flush_message_queue(&device_id);

        self.processed_invite_responses
            .lock()
            .unwrap()
            .insert(event_id.clone());

        self.pending_invite_responses
            .lock()
            .unwrap()
            .retain(|event| event.id.to_string() != event_id);

        true
    }

    pub(super) fn retry_pending_invite_responses(&self, owner_pubkey: PublicKey) {
        let Some((invite, our_identity_key)) = self
            .invite_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|state| (state.invite.clone(), state.our_identity_key))
        else {
            return;
        };

        let pending_events: Vec<nostr::Event> = self
            .pending_invite_responses
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();

        for event in pending_events {
            if self
                .processed_invite_responses
                .lock()
                .unwrap()
                .contains(&event.id.to_string())
            {
                continue;
            }

            let Ok(Some(response)) = invite.process_invite_response(&event, our_identity_key)
            else {
                continue;
            };

            let resolved_owner = response
                .owner_public_key
                .unwrap_or_else(|| self.resolve_to_owner(&response.invitee_identity));
            if resolved_owner != owner_pubkey {
                continue;
            }

            let _ = self.install_invite_response_session(event.id.to_string(), response);
        }
    }

    pub(super) fn subscribe_to_app_keys(&self, owner_pubkey: PublicKey) {
        let mut subs = self.app_keys_subscriptions.lock().unwrap();
        if subs.contains(&owner_pubkey) {
            return;
        }
        subs.insert(owner_pubkey);
        drop(subs);

        let filter = nostr::Filter::new()
            .kind(nostr::Kind::Custom(crate::APP_KEYS_EVENT_KIND as u16))
            .authors(vec![owner_pubkey]);
        if let Ok(filter_json) = serde_json::to_string(&filter) {
            let subid = format!("app-keys-{}", uuid::Uuid::new_v4());
            let _ = self.pubsub.subscribe(subid, filter_json);
        }
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) {
        let owner_pubkey = self.resolve_to_owner(&user_pubkey);

        let known_identities = self.with_user_records(move |records| {
            records
                .entry(owner_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())))
                .known_device_identities
                .clone()
        });

        self.subscribe_to_app_keys(owner_pubkey);

        for identity_hex in known_identities {
            if let Ok(pk) = crate::utils::pubkey_from_hex(&identity_hex) {
                self.subscribe_to_device_invite(owner_pubkey, pk);
            }
        }
    }

    pub(super) fn subscribe_to_device_invite(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
    ) {
        let mut subs = self.invite_subscriptions.lock().unwrap();
        if subs.contains(&device_pubkey) {
            return;
        }
        subs.insert(device_pubkey);
        drop(subs);

        let has_active_session = self.with_user_records(move |records| {
            records
                .get(&owner_pubkey)
                .and_then(|record| {
                    let device_hex = hex::encode(device_pubkey.to_bytes());
                    record.device_records.get(&device_hex)
                })
                .and_then(|device_record| device_record.active_session.as_ref())
                .is_some()
        });
        if has_active_session {
            return;
        }

        let _ = Invite::from_user_with_pubsub(device_pubkey, self.pubsub.as_ref());
    }

    pub(super) fn upsert_device_record(record: &mut UserRecord, device_id: &str) {
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

    pub(super) fn record_known_device_identity(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
    ) {
        let identity_hex = hex::encode(device_pubkey.to_bytes());
        let updated = self.with_user_records(move |records| {
            let record = records
                .entry(owner_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
            if record.known_device_identities.contains(&identity_hex) {
                return false;
            }
            record.known_device_identities.push(identity_hex.clone());
            true
        });
        self.delegate_to_owner
            .lock()
            .unwrap()
            .insert(device_pubkey, owner_pubkey);
        if updated {
            let _ = self.store_user_record(&owner_pubkey);
        }
    }

    pub(super) fn flush_message_queue(&self, device_identity: &str) -> Result<()> {
        let entries = self.message_queue.get_for_target(device_identity)?;
        if entries.is_empty() {
            return Ok(());
        }

        let owner_pubkey = self.with_user_records({
            let device_identity = device_identity.to_string();
            move |records| {
                records.iter().find_map(|(owner, user_record)| {
                    user_record
                        .device_records
                        .contains_key(&device_identity)
                        .then_some(*owner)
                })
            }
        });
        let Some(owner_pubkey) = owner_pubkey else {
            return Ok(());
        };

        let mut sent: Vec<(String, Option<String>)> = Vec::new();
        let pending_publishes = self.with_user_records({
            let device_identity = device_identity.to_string();
            let entries = entries.clone();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_identity)
                else {
                    return Vec::new();
                };

                let mut pending = Vec::new();
                for entry in entries {
                    let maybe_event_id = entry
                        .event
                        .id
                        .as_ref()
                        .map(|id| id.to_string())
                        .or_else(|| entry.id.split('/').next().map(str::to_string));
                    if let Some(signed_event) =
                        SessionManager::send_event_with_best_session(device_record, entry.event)
                    {
                        pending.push((entry.id, maybe_event_id, signed_event));
                    }
                }
                pending
            }
        });

        for (entry_id, maybe_event_id, signed_event) in pending_publishes {
            if self
                .pubsub
                .publish_signed_for_inner_event(signed_event, maybe_event_id.clone())
                .is_ok()
            {
                sent.push((entry_id, maybe_event_id));
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

    pub(super) fn expand_discovery_queue(
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
            let mut expanded_for_all_devices = true;
            for device in devices {
                let device_id = device.identity_pubkey.to_hex();
                if device_id == self.device_id {
                    continue;
                }
                if self.message_queue.add(&device_id, &entry.event).is_err() {
                    expanded_for_all_devices = false;
                }
            }

            // Keep discovery entry when any per-device queue write fails so the next
            // AppKeys cycle can retry expansion without losing pending messages.
            if expanded_for_all_devices {
                let _ = self.discovery_queue.remove(&entry.id);
            }
        }

        Ok(())
    }

    pub(super) fn send_message_history(&self, owner_pubkey: PublicKey, device_id: &str) {
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

        let signed_history = self.with_user_records({
            let device_id = device_id.to_string();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_id) else {
                    return Vec::new();
                };
                let mut signed = Vec::new();
                for event in history {
                    if let Some(signed_event) =
                        SessionManager::send_event_with_best_session(device_record, event)
                    {
                        signed.push(signed_event);
                    }
                }
                signed
            }
        });

        for signed_event in signed_history {
            let _ = self.pubsub.publish_signed(signed_event);
        }
        let _ = self.store_user_record(&owner_pubkey);
    }

    pub(super) fn build_bootstrap_messages(&self, owner_pubkey: PublicKey) -> Vec<UnsignedEvent> {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + INVITE_BOOTSTRAP_EXPIRATION_SECONDS;
        let expiration =
            match Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()]) {
                Ok(tag) => tag,
                Err(_) => return Vec::new(),
            };

        let mut bootstrap_messages = Vec::new();
        for _ in INVITE_BOOTSTRAP_RETRY_DELAYS_MS {
            let Ok(bootstrap) = self.build_message_event(
                owner_pubkey,
                crate::TYPING_KIND,
                "typing".to_string(),
                vec![expiration.clone()],
            ) else {
                break;
            };
            bootstrap_messages.push(bootstrap);
        }

        bootstrap_messages
    }

    pub(super) fn sign_bootstrap_schedule(
        session: &mut crate::Session,
        bootstrap_messages: &[UnsignedEvent],
    ) -> Vec<nostr::Event> {
        let mut bootstrap_events = Vec::new();
        for bootstrap in bootstrap_messages {
            let Ok(signed_bootstrap) = session.send_event(bootstrap.clone()) else {
                break;
            };
            bootstrap_events.push(signed_bootstrap);
        }

        bootstrap_events
    }

    pub(super) fn publish_bootstrap_schedule(&self, bootstrap_events: Vec<nostr::Event>) {
        let Some((initial_event, retry_events)) = bootstrap_events.split_first() else {
            return;
        };

        let _ = self.pubsub.publish_signed(initial_event.clone());

        if retry_events.is_empty() {
            return;
        }

        let scheduled_retries: Vec<(u64, nostr::Event)> = retry_events
            .iter()
            .cloned()
            .zip(INVITE_BOOTSTRAP_RETRY_DELAYS_MS.iter().copied().skip(1))
            .map(|(event, delay_ms)| (delay_ms, event))
            .collect();
        let pubsub = self.pubsub.clone();
        std::thread::spawn(move || {
            for (delay_ms, event) in scheduled_retries {
                std::thread::sleep(Duration::from_millis(delay_ms));
                let _ = pubsub.publish_signed(event);
            }
        });
    }

    pub(super) fn send_link_bootstrap(&self, owner_pubkey: PublicKey, device_id: &str) {
        let bootstrap_messages = self.build_bootstrap_messages(owner_pubkey);
        let bootstrap_events = self.with_user_records({
            let device_id = device_id.to_string();
            let bootstrap_messages = bootstrap_messages.clone();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_id) else {
                    return Vec::new();
                };
                let mut signed = Vec::new();
                for bootstrap in bootstrap_messages {
                    let Some(signed_bootstrap) =
                        SessionManager::send_event_with_best_session(device_record, bootstrap)
                    else {
                        break;
                    };
                    signed.push(signed_bootstrap);
                }
                signed
            }
        });

        if !bootstrap_events.is_empty() {
            self.publish_bootstrap_schedule(bootstrap_events);
            let _ = self.store_user_record(&owner_pubkey);
        }
    }

    pub(super) fn cleanup_device(&self, owner_pubkey: PublicKey, device_id: &str) {
        let removed = self.with_user_records({
            let device_id = device_id.to_string();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return false;
                };

                if let Some(device_record) = user_record.device_records.remove(&device_id) {
                    if let Some(session) = device_record.active_session {
                        session.close();
                    }
                    for session in device_record.inactive_sessions {
                        session.close();
                    }
                    return true;
                }

                false
            }
        });
        if !removed {
            return;
        }

        if let Ok(device_pk) = crate::utils::pubkey_from_hex(device_id) {
            self.delegate_to_owner.lock().unwrap().remove(&device_pk);
        }

        let _ = self.message_queue.remove_for_target(device_id);

        let _ = self.store_user_record(&owner_pubkey);
    }
}
