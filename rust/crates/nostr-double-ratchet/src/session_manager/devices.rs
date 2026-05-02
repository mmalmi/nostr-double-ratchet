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

    pub(super) fn apply_app_keys_device_roster(&self, owner_pubkey: PublicKey, app_keys: &AppKeys) {
        self.update_delegate_mapping(owner_pubkey, app_keys);

        let devices = app_keys.get_all_devices();
        let _ = self.expand_discovery_queue(owner_pubkey, &devices);
        let mut active_ids: HashSet<String> = devices
            .iter()
            .map(|d| hex::encode(d.identity_pubkey.to_bytes()))
            .collect();
        active_ids.insert(owner_pubkey.to_hex());

        let existing_devices = self.with_user_records(move |records| {
            records
                .get(&owner_pubkey)
                .map(|r| r.device_records.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });

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

        self.retry_pending_invite_responses(owner_pubkey);

        for device in &devices {
            let device_id = device.identity_pubkey.to_hex();
            if device_id == self.device_id {
                continue;
            }
            let has_active_session = self.with_user_records({
                let device_id = device_id.clone();
                move |records| {
                    records
                        .get(&owner_pubkey)
                        .and_then(|r| r.device_records.get(&device_id))
                        .and_then(|d| d.active_session.as_ref())
                        .is_some()
                }
            });
            if has_active_session {
                let _ = self.flush_message_queue(&device_id);
            }
        }
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
