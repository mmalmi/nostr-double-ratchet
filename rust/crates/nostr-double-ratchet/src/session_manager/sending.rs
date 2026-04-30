use super::*;

impl SessionManager {
    pub(super) fn build_message_event(
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

    pub(super) fn send_event_internal(
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
            for owner in &owners {
                history.entry(*owner).or_default().push(event.clone());
            }
        }

        // Ensure all target owners are set up.
        for owner in &owners {
            self.setup_user(*owner);
        }

        // Gather known devices per owner.
        let owner_targets = self.with_user_records({
            let owners = owners.clone();
            let our_device_id = self.device_id.clone();
            move |records| {
                owners
                    .into_iter()
                    .map(|owner| {
                        let mut device_ids = Vec::new();
                        if let Some(record) = records.get(&owner) {
                            let mut seen = HashSet::new();
                            for identity_hex in &record.known_device_identities {
                                if identity_hex == &our_device_id {
                                    continue;
                                }
                                if seen.insert(identity_hex.clone()) {
                                    device_ids.push(identity_hex.clone());
                                }
                            }
                            for device_id in record.device_records.keys() {
                                if device_id != &our_device_id && seen.insert(device_id.clone()) {
                                    device_ids.push(device_id.clone());
                                }
                            }
                        }
                        (owner, device_ids)
                    })
                    .collect::<HashMap<_, _>>()
            }
        });

        // Queue for each target owner:
        // - known devices -> message queue per device
        // - no known devices -> discovery queue per owner
        for owner in &owners {
            let mut seen_for_owner = HashSet::new();
            let device_ids = owner_targets.get(owner).cloned().unwrap_or_default();
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
        for owner in &owners {
            if let Some(device_ids) = owner_targets.get(owner) {
                for device_id in device_ids {
                    if seen.insert(device_id.clone()) {
                        device_targets.push((*owner, device_id.clone()));
                    }
                }
            }
        }

        let mut event_ids = Vec::new();
        let inner_event_id = event.id.as_ref().map(|id| id.to_string());
        let mut published_device_ids: Vec<String> = Vec::new();

        for (owner, device_id) in device_targets {
            let cached_app_keys = self.cached_app_keys.lock().unwrap().get(&owner).cloned();
            let device_pubkey = crate::utils::pubkey_from_hex(&device_id).ok();
            let maybe_signed_event = self.with_user_records({
                let device_id = device_id.clone();
                let event = event.clone();
                move |records| {
                    let user_record = records.get_mut(&owner)?;

                    if let Some(device_pk) = device_pubkey {
                        let authorized = if owner == device_pk {
                            true
                        } else if let Some(app_keys) = cached_app_keys.as_ref() {
                            app_keys.get_device(&device_pk).is_some()
                        } else {
                            let device_hex = hex::encode(device_pk.to_bytes());
                            user_record.device_records.contains_key(&device_hex)
                                || user_record.known_device_identities.contains(&device_hex)
                        };

                        if !authorized {
                            return None;
                        }
                    }

                    user_record
                        .device_records
                        .get_mut(&device_id)
                        .and_then(|device_record| {
                            SessionManager::send_event_with_best_session(device_record, event)
                        })
                }
            });

            if let Some(signed_event) = maybe_signed_event {
                event_ids.push(signed_event.id.to_string());
                if self
                    .pubsub
                    .publish_signed_for_inner_event_to_device(
                        signed_event,
                        inner_event_id.clone(),
                        Some(device_id.clone()),
                    )
                    .is_ok()
                {
                    published_device_ids.push(device_id.clone());
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

    pub(super) fn delete_user_local(&self, owner_pubkey: PublicKey) -> Result<()> {
        if owner_pubkey == self.owner_public_key {
            return Ok(());
        }

        let (known_device_ids, known_identity_hexes) = self.with_user_records(move |records| {
            let Some(mut user_record) = records.remove(&owner_pubkey) else {
                return (Vec::new(), Vec::new());
            };

            let mut known_device_ids = Vec::new();
            for (device_id, device_record) in user_record.device_records.drain() {
                if let Some(session) = device_record.active_session {
                    session.close();
                }
                for session in device_record.inactive_sessions {
                    session.close();
                }
                known_device_ids.push(device_id);
            }

            (known_device_ids, user_record.known_device_identities)
        });

        let mut known_device_pubkeys: Vec<PublicKey> = Vec::new();
        for device_id in &known_device_ids {
            if let Ok(device_pk) = crate::utils::pubkey_from_hex(device_id) {
                known_device_pubkeys.push(device_pk);
            }
        }
        for identity_hex in known_identity_hexes {
            if let Ok(device_pk) = crate::utils::pubkey_from_hex(&identity_hex) {
                known_device_pubkeys.push(device_pk);
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
        self.latest_app_keys_created_at
            .lock()
            .unwrap()
            .remove(&owner_pubkey);
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
}
