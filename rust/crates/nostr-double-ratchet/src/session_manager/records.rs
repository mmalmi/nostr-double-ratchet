use super::*;

impl SessionManager {
    pub(super) fn handle_app_keys_event(
        &self,
        owner_pubkey: PublicKey,
        app_keys: AppKeys,
        created_at: u64,
    ) {
        let effective_app_keys = {
            let existing = self
                .cached_app_keys
                .lock()
                .unwrap()
                .get(&owner_pubkey)
                .cloned();

            let mut latest = self.latest_app_keys_created_at.lock().unwrap();
            let latest_created_at = latest.get(&owner_pubkey).copied().unwrap_or(0);
            let applied = apply_app_keys_snapshot(
                existing.as_ref(),
                latest_created_at,
                &app_keys,
                created_at,
            );
            if applied.decision == AppKeysSnapshotDecision::Stale {
                return;
            }
            latest.insert(owner_pubkey, applied.created_at);
            applied.app_keys
        };

        self.apply_app_keys_device_roster(owner_pubkey, &effective_app_keys);
    }

    pub(super) fn merge_stored_device_record(
        mut existing: crate::StoredDeviceRecord,
        current: crate::StoredDeviceRecord,
    ) -> crate::StoredDeviceRecord {
        let mut inactive_sessions = Vec::new();

        for state in existing.inactive_sessions.drain(..) {
            Self::push_unique_session_state(&mut inactive_sessions, state);
        }
        for state in current.inactive_sessions {
            Self::push_unique_session_state(&mut inactive_sessions, state);
        }

        let active_session = match (existing.active_session.take(), current.active_session) {
            (Some(existing_state), Some(current_state)) => {
                if Self::session_state_priority(&existing_state)
                    > Self::session_state_priority(&current_state)
                {
                    Self::push_unique_session_state(&mut inactive_sessions, current_state);
                    Some(existing_state)
                } else {
                    Self::push_unique_session_state(&mut inactive_sessions, existing_state);
                    Some(current_state)
                }
            }
            (Some(existing_state), None) => Some(existing_state),
            (None, Some(current_state)) => Some(current_state),
            (None, None) => None,
        };

        if let Some(active) = active_session.as_ref() {
            inactive_sessions.retain(|state| state != active);
        }
        const MAX_INACTIVE: usize = 10;
        inactive_sessions.truncate(MAX_INACTIVE);

        crate::StoredDeviceRecord {
            device_id: current.device_id,
            active_session,
            inactive_sessions,
            created_at: match (existing.created_at, current.created_at) {
                (0, created_at) => created_at,
                (created_at, 0) => created_at,
                (existing_created, current_created) => existing_created.min(current_created),
            },
            is_stale: current.is_stale,
            stale_timestamp: current.stale_timestamp.or(existing.stale_timestamp),
            last_activity: current.last_activity.max(existing.last_activity),
        }
    }

    pub(super) fn merge_stored_user_record(
        mut existing: crate::StoredUserRecord,
        current: crate::StoredUserRecord,
    ) -> crate::StoredUserRecord {
        let mut existing_devices: HashMap<String, crate::StoredDeviceRecord> = existing
            .devices
            .drain(..)
            .map(|device| (device.device_id.clone(), device))
            .collect();

        let mut devices = Vec::new();
        for current_device in current.devices {
            let device =
                if let Some(existing_device) = existing_devices.remove(&current_device.device_id) {
                    Self::merge_stored_device_record(existing_device, current_device)
                } else {
                    current_device
                };
            devices.push(device);
        }
        devices.extend(existing_devices.into_values());
        devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));

        let mut known_device_identities = current.known_device_identities;
        for identity in existing.known_device_identities {
            if !known_device_identities.contains(&identity) {
                known_device_identities.push(identity);
            }
        }

        crate::StoredUserRecord {
            user_id: current.user_id,
            devices,
            known_device_identities,
        }
    }

    pub(super) fn store_user_record(&self, pubkey: &PublicKey) -> Result<()> {
        let stored = self.with_user_records({
            let pubkey = *pubkey;
            move |records| {
                records
                    .get(&pubkey)
                    .map(|user_record| user_record.to_stored())
            }
        });
        if let Some(stored) = stored {
            let key = self.user_record_key(pubkey);
            let stored = match self.storage.get(&key)? {
                Some(existing_json) => {
                    match serde_json::from_str::<crate::StoredUserRecord>(&existing_json) {
                        Ok(existing) => Self::merge_stored_user_record(existing, stored),
                        Err(_) => stored,
                    }
                }
                None => stored,
            };
            let json = serde_json::to_string(&stored)?;
            self.storage.put(&key, json)?;
        }
        Ok(())
    }

    pub(super) fn load_all_user_records(&self) -> Result<()> {
        let prefix = self.user_record_key_prefix();
        let keys = self.storage.list(&prefix)?;
        let mut loaded_records = Vec::new();

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
                    device_record.active_session = Some(crate::Session::new(
                        state,
                        format!("session-{}", device.device_id),
                    ));
                }

                for state in device.inactive_sessions {
                    let session = crate::Session::new(
                        state,
                        format!("session-{}-inactive", device.device_id),
                    );
                    device_record.inactive_sessions.push(session);
                }

                crate::UserRecord::compact_duplicate_sessions(&mut device_record);
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

            loaded_records.push((owner_pubkey, user_record));
        }

        self.with_user_records(move |records| {
            for (owner_pubkey, user_record) in loaded_records {
                records.insert(owner_pubkey, user_record);
            }
        });

        Ok(())
    }
}
