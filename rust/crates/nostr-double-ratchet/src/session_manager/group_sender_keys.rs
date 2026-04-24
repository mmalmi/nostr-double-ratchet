use super::*;

impl SessionManager {
    pub(super) fn group_sender_key_state_key(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
    ) -> String {
        format!(
            "group-sender-key/state/{}/{}",
            hex::encode(sender_event_pubkey.to_bytes()),
            key_id
        )
    }

    pub(super) fn tag_value(tags: &nostr::Tags, key: &str) -> Option<String> {
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

    pub(super) fn load_group_sender_event_infos(&self) -> Result<()> {
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

    pub(super) fn subscribe_to_group_sender_event(
        &self,
        sender_event_pubkey: PublicKey,
    ) -> Result<()> {
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

    pub(super) fn load_group_sender_event_info(
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

    pub(super) fn load_sender_key_state(
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

    pub(super) fn store_sender_key_state(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
        state: &SenderKeyState,
    ) -> Result<()> {
        let key = self.group_sender_key_state_key(sender_event_pubkey, key_id);
        self.storage.put(&key, serde_json::to_string(state)?)?;
        Ok(())
    }

    pub(super) fn ensure_sender_key_state_from_distribution(
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

    pub(super) fn store_group_sender_event_info(
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

    pub(super) fn maybe_handle_group_sender_key_distribution(
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

    pub(super) fn try_decrypt_group_sender_key_outer(
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
}
