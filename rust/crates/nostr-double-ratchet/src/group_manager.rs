use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, SecretKey, Tag, Timestamp, UnsignedEvent};
use rand::random;

use crate::{
    group::{GroupData, GROUP_SENDER_KEY_DISTRIBUTION_KIND},
    one_to_many::OneToManyChannel,
    sender_key::{SenderKeyDistribution, SenderKeyState},
    Error, InMemoryStorage, Result, StorageAdapter, CHAT_MESSAGE_KIND,
};

pub struct GroupManagerOptions {
    pub our_owner_pubkey: PublicKey,
    pub our_device_pubkey: PublicKey,
    pub storage: Option<Arc<dyn StorageAdapter>>,
    pub one_to_many: Option<OneToManyChannel>,
}

#[derive(Debug, Clone)]
pub struct GroupSendEvent {
    pub kind: u32,
    pub content: String,
    pub tags: Vec<Vec<String>>,
}

impl GroupSendEvent {
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            kind: CHAT_MESSAGE_KIND,
            content: message.into(),
            tags: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GroupSendResult {
    pub outer: Event,
    pub inner: UnsignedEvent,
}

#[derive(Debug, Clone)]
pub struct GroupDecryptedEvent {
    pub group_id: String,
    pub sender_event_pubkey: PublicKey,
    pub sender_device_pubkey: PublicKey,
    pub sender_owner_pubkey: Option<PublicKey>,
    pub outer_event_id: String,
    pub outer_created_at: u64,
    pub key_id: u32,
    pub message_number: u32,
    pub inner: UnsignedEvent,
}

pub struct GroupManager {
    our_owner_pubkey: PublicKey,
    our_device_pubkey: PublicKey,
    storage: Arc<dyn StorageAdapter>,
    one_to_many: OneToManyChannel,

    groups: HashMap<String, GroupChannel>,
    sender_event_to_group: HashMap<PublicKey, String>,
    group_to_sender_events: HashMap<String, HashSet<PublicKey>>,
    pending_outer_by_sender_event: HashMap<PublicKey, Vec<Event>>,
    max_pending_per_sender_event: usize,
}

impl GroupManager {
    pub fn new(opts: GroupManagerOptions) -> Self {
        Self {
            our_owner_pubkey: opts.our_owner_pubkey,
            our_device_pubkey: opts.our_device_pubkey,
            storage: opts
                .storage
                .unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            one_to_many: opts.one_to_many.unwrap_or_default(),
            groups: HashMap::new(),
            sender_event_to_group: HashMap::new(),
            group_to_sender_events: HashMap::new(),
            pending_outer_by_sender_event: HashMap::new(),
            max_pending_per_sender_event: 128,
        }
    }

    pub fn upsert_group(&mut self, data: GroupData) -> Result<()> {
        let group_id = data.id.clone();

        if let Some(group) = self.groups.get_mut(&group_id) {
            group.set_data(data);
        } else {
            let group = GroupChannel::new(
                data,
                self.our_owner_pubkey,
                self.our_device_pubkey,
                self.storage.clone(),
                self.one_to_many.clone(),
            );
            self.groups.insert(group_id.clone(), group);
        }

        self.refresh_group_sender_mappings(&group_id);
        Ok(())
    }

    pub fn remove_group(&mut self, group_id: &str) {
        self.groups.remove(group_id);

        if let Some(sender_events) = self.group_to_sender_events.get(group_id) {
            for sender_event_pubkey in sender_events {
                if self
                    .sender_event_to_group
                    .get(sender_event_pubkey)
                    .is_some_and(|mapped| mapped == group_id)
                {
                    self.sender_event_to_group.remove(sender_event_pubkey);
                }
            }
        }
        self.group_to_sender_events.remove(group_id);
    }

    /// Return all sender-event pubkeys currently known across managed groups.
    ///
    /// This includes mappings learned from local sends and from incoming sender-key
    /// distribution rumors. The returned list is de-duplicated and sorted.
    pub fn known_sender_event_pubkeys(&mut self) -> Vec<PublicKey> {
        let group_ids: Vec<String> = self.groups.keys().cloned().collect();
        for group_id in group_ids {
            self.refresh_group_sender_mappings(&group_id);
        }

        let mut values: Vec<PublicKey> = self.sender_event_to_group.keys().copied().collect();
        values.sort_by_key(|pk| pk.to_hex());
        values.dedup();
        values
    }

    pub fn send_message<F, G>(
        &mut self,
        group_id: &str,
        message: &str,
        send_pairwise: &mut F,
        publish_outer: &mut G,
        now_ms: Option<u64>,
    ) -> Result<GroupSendResult>
    where
        F: FnMut(PublicKey, &UnsignedEvent) -> Result<()>,
        G: FnMut(&Event) -> Result<()>,
    {
        self.send_event(
            group_id,
            GroupSendEvent::message(message),
            send_pairwise,
            publish_outer,
            now_ms,
        )
    }

    pub fn send_event<F, G>(
        &mut self,
        group_id: &str,
        event: GroupSendEvent,
        send_pairwise: &mut F,
        publish_outer: &mut G,
        now_ms: Option<u64>,
    ) -> Result<GroupSendResult>
    where
        F: FnMut(PublicKey, &UnsignedEvent) -> Result<()>,
        G: FnMut(&Event) -> Result<()>,
    {
        let Some(group) = self.groups.get_mut(group_id) else {
            return Err(Error::InvalidEvent(format!("Unknown group: {group_id}")));
        };

        let result = group.send_event(event, send_pairwise, publish_outer, now_ms)?;
        self.refresh_group_sender_mappings(group_id);
        Ok(result)
    }

    pub fn rotate_sender_key<F>(
        &mut self,
        group_id: &str,
        send_pairwise: &mut F,
        now_ms: Option<u64>,
    ) -> Result<SenderKeyDistribution>
    where
        F: FnMut(PublicKey, &UnsignedEvent) -> Result<()>,
    {
        let Some(group) = self.groups.get_mut(group_id) else {
            return Err(Error::InvalidEvent(format!("Unknown group: {group_id}")));
        };

        let result = group.rotate_sender_key(send_pairwise, now_ms)?;
        self.refresh_group_sender_mappings(group_id);
        Ok(result)
    }

    pub fn handle_incoming_session_event(
        &mut self,
        event: &UnsignedEvent,
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
    ) -> Vec<GroupDecryptedEvent> {
        let mut group_id = first_tag_value(&event.tags, "l");
        let mut dist: Option<SenderKeyDistribution> = None;

        if event.kind == Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16) {
            dist = parse_sender_key_distribution(&event.content);
            if let Some(parsed) = dist.as_ref() {
                group_id = Some(parsed.group_id.clone());
            }
        }

        let Some(group_id) = group_id else {
            return Vec::new();
        };
        let Some(group) = self.groups.get_mut(&group_id) else {
            return Vec::new();
        };

        let mut drained = group.handle_incoming_session_event(
            event,
            from_owner_pubkey,
            from_sender_device_pubkey,
        );

        if let Some(sender_event_pubkey) = dist
            .as_ref()
            .and_then(|d| d.sender_event_pubkey.as_deref())
            .and_then(parse_pubkey_hex)
        {
            self.bind_sender_event_to_group(&group_id, sender_event_pubkey);

            if let Some(group) = self.groups.get_mut(&group_id) {
                let mut drained_from_manager = Self::drain_pending_outer_for_sender_event(
                    &mut self.pending_outer_by_sender_event,
                    &self.one_to_many,
                    sender_event_pubkey,
                    group,
                );
                drained.append(&mut drained_from_manager);
            }
        }

        self.refresh_group_sender_mappings(&group_id);
        drained
    }

    pub fn handle_outer_event(&mut self, outer: &Event) -> Option<GroupDecryptedEvent> {
        if outer.kind != Kind::Custom(self.one_to_many.outer_kind() as u16) {
            return None;
        }

        let sender_event_pubkey = outer.pubkey;
        let Some(group_id) = self
            .sender_event_to_group
            .get(&sender_event_pubkey)
            .cloned()
        else {
            self.queue_pending_outer(sender_event_pubkey, outer.clone());
            return None;
        };

        let Some(group) = self.groups.get_mut(&group_id) else {
            self.queue_pending_outer(sender_event_pubkey, outer.clone());
            return None;
        };

        group.handle_outer_event(outer)
    }

    fn bind_sender_event_to_group(&mut self, group_id: &str, sender_event_pubkey: PublicKey) {
        self.sender_event_to_group
            .insert(sender_event_pubkey, group_id.to_string());
        self.group_to_sender_events
            .entry(group_id.to_string())
            .or_default()
            .insert(sender_event_pubkey);
    }

    fn refresh_group_sender_mappings(&mut self, group_id: &str) {
        let Some(group) = self.groups.get_mut(group_id) else {
            return;
        };
        let Ok(next_sender_events) = group.list_sender_event_pubkeys() else {
            return;
        };

        let next: HashSet<PublicKey> = next_sender_events.into_iter().collect();
        let prev = self
            .group_to_sender_events
            .get(group_id)
            .cloned()
            .unwrap_or_default();

        for sender_event_pubkey in &prev {
            if next.contains(sender_event_pubkey) {
                continue;
            }
            if self
                .sender_event_to_group
                .get(sender_event_pubkey)
                .is_some_and(|mapped| mapped == group_id)
            {
                self.sender_event_to_group.remove(sender_event_pubkey);
            }
        }

        for sender_event_pubkey in &next {
            self.sender_event_to_group
                .insert(*sender_event_pubkey, group_id.to_string());
        }

        self.group_to_sender_events
            .insert(group_id.to_string(), next);
    }

    fn queue_pending_outer(&mut self, sender_event_pubkey: PublicKey, outer: Event) {
        let pending = self
            .pending_outer_by_sender_event
            .entry(sender_event_pubkey)
            .or_default();
        if pending.len() >= self.max_pending_per_sender_event {
            pending.remove(0);
        }
        pending.push(outer);
    }

    fn drain_pending_outer_for_sender_event(
        pending_outer_by_sender_event: &mut HashMap<PublicKey, Vec<Event>>,
        one_to_many: &OneToManyChannel,
        sender_event_pubkey: PublicKey,
        group: &mut GroupChannel,
    ) -> Vec<GroupDecryptedEvent> {
        let Some(pending) = pending_outer_by_sender_event.remove(&sender_event_pubkey) else {
            return Vec::new();
        };
        if pending.is_empty() {
            return Vec::new();
        }

        let mut with_message_number: Vec<(Event, u32)> = pending
            .into_iter()
            .map(|outer| {
                let message_number = one_to_many
                    .parse_outer_content(&outer.content)
                    .map(|parsed| parsed.message_number)
                    .unwrap_or(0);
                (outer, message_number)
            })
            .collect();
        with_message_number.sort_by_key(|(_, message_number)| *message_number);

        let mut decrypted = Vec::new();
        for (outer, _) in with_message_number {
            if let Some(event) = group.handle_outer_event(&outer) {
                decrypted.push(event);
            }
        }
        decrypted
    }
}

struct GroupChannel {
    data: GroupData,
    our_owner_pubkey: PublicKey,
    our_device_pubkey: PublicKey,
    member_owner_pubkeys: Vec<PublicKey>,
    storage: Arc<dyn StorageAdapter>,
    one_to_many: OneToManyChannel,

    initialized: bool,
    sender_device_to_event: HashMap<PublicKey, PublicKey>,
    sender_event_to_device: HashMap<PublicKey, PublicKey>,
    sender_device_to_owner: HashMap<PublicKey, PublicKey>,
    pending_outer: HashMap<(PublicKey, u32), Vec<Event>>,
}

impl GroupChannel {
    fn new(
        data: GroupData,
        our_owner_pubkey: PublicKey,
        our_device_pubkey: PublicKey,
        storage: Arc<dyn StorageAdapter>,
        one_to_many: OneToManyChannel,
    ) -> Self {
        let member_owner_pubkeys = data
            .members
            .iter()
            .filter_map(|hex| parse_pubkey_hex(hex))
            .collect();

        Self {
            data,
            our_owner_pubkey,
            our_device_pubkey,
            member_owner_pubkeys,
            storage,
            one_to_many,
            initialized: false,
            sender_device_to_event: HashMap::new(),
            sender_event_to_device: HashMap::new(),
            sender_device_to_owner: HashMap::new(),
            pending_outer: HashMap::new(),
        }
    }

    fn group_id(&self) -> &str {
        &self.data.id
    }

    fn set_data(&mut self, data: GroupData) {
        self.member_owner_pubkeys = data
            .members
            .iter()
            .filter_map(|hex| parse_pubkey_hex(hex))
            .collect();
        self.data = data;
    }

    fn list_sender_event_pubkeys(&mut self) -> Result<Vec<PublicKey>> {
        self.init()?;
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        for value in self.sender_device_to_event.values() {
            if seen.insert(*value) {
                values.push(*value);
            }
        }
        Ok(values)
    }

    fn rotate_sender_key<F>(
        &mut self,
        send_pairwise: &mut F,
        now_ms: Option<u64>,
    ) -> Result<SenderKeyDistribution>
    where
        F: FnMut(PublicKey, &UnsignedEvent) -> Result<()>,
    {
        self.init()?;

        let now_ms = now_ms.unwrap_or_else(now_millis);
        let now_seconds = now_ms / 1000;

        let (_, sender_event_pubkey, _) = self.ensure_our_sender_event_keys()?;
        let (sender_key_state, _) = self.ensure_our_sender_key_state(true)?;

        let distribution =
            self.build_distribution(now_seconds, sender_event_pubkey, &sender_key_state);
        let rumor = self.build_distribution_rumor(now_seconds, now_ms, &distribution)?;

        for member_owner in &self.member_owner_pubkeys {
            if member_owner == &self.our_owner_pubkey {
                continue;
            }
            send_pairwise(*member_owner, &rumor)?;
        }

        Ok(distribution)
    }

    fn send_event<F, G>(
        &mut self,
        event: GroupSendEvent,
        send_pairwise: &mut F,
        publish_outer: &mut G,
        now_ms: Option<u64>,
    ) -> Result<GroupSendResult>
    where
        F: FnMut(PublicKey, &UnsignedEvent) -> Result<()>,
        G: FnMut(&Event) -> Result<()>,
    {
        self.init()?;

        let now_ms = now_ms.unwrap_or_else(now_millis);
        let now_seconds = now_ms / 1000;

        let (sender_event_keys, sender_event_pubkey, sender_event_key_changed) =
            self.ensure_our_sender_event_keys()?;
        let (mut sender_key_state, sender_key_created) = self.ensure_our_sender_key_state(false)?;

        if sender_key_created || sender_event_key_changed {
            let distribution =
                self.build_distribution(now_seconds, sender_event_pubkey, &sender_key_state);
            let rumor = self.build_distribution_rumor(now_seconds, now_ms, &distribution)?;
            for member_owner in &self.member_owner_pubkeys {
                if member_owner == &self.our_owner_pubkey {
                    continue;
                }
                send_pairwise(*member_owner, &rumor)?;
            }
        }

        let inner = self.build_group_inner_rumor(now_seconds, now_ms, event)?;
        let inner_json = serde_json::to_string(&inner)?;
        let outer = self.one_to_many.encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_key_state,
            &inner_json,
            Timestamp::from(now_seconds),
        )?;

        self.save_sender_key_state(self.our_device_pubkey, &sender_key_state)?;
        publish_outer(&outer)?;

        Ok(GroupSendResult { outer, inner })
    }

    fn handle_incoming_session_event(
        &mut self,
        event: &UnsignedEvent,
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
    ) -> Vec<GroupDecryptedEvent> {
        if self.init().is_err() {
            return Vec::new();
        }

        if !self.member_owner_pubkeys.contains(&from_owner_pubkey) {
            return Vec::new();
        }

        let tagged_group_id = first_tag_value(&event.tags, "l");
        if tagged_group_id.as_deref() != Some(self.group_id()) {
            return Vec::new();
        }

        if event.kind != Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16) {
            return Vec::new();
        }

        let Some(dist) = parse_sender_key_distribution(&event.content) else {
            return Vec::new();
        };
        if dist.group_id != self.group_id() {
            return Vec::new();
        }

        let Some(sender_device_pubkey) = from_sender_device_pubkey else {
            return Vec::new();
        };
        if event.pubkey != sender_device_pubkey {
            return Vec::new();
        }

        self.sender_device_to_owner
            .insert(sender_device_pubkey, from_owner_pubkey);
        let _ = self.storage.put(
            &self.sender_owner_pubkey_key(sender_device_pubkey),
            pubkey_to_hex(&from_owner_pubkey),
        );

        let mut sender_event_pubkey = None;
        if let Some(parsed_sender_event) = dist
            .sender_event_pubkey
            .as_deref()
            .and_then(parse_pubkey_hex)
        {
            self.set_sender_event_mapping(sender_device_pubkey, parsed_sender_event);
            let _ = self.storage.put(
                &self.sender_event_pubkey_key(sender_device_pubkey),
                pubkey_to_hex(&parsed_sender_event),
            );
            sender_event_pubkey = Some(parsed_sender_event);
        }

        if self
            .load_sender_key_state(sender_device_pubkey, dist.key_id)
            .ok()
            .flatten()
            .is_none()
        {
            let state = SenderKeyState::new(dist.key_id, dist.chain_key, dist.iteration);
            let _ = self.save_sender_key_state(sender_device_pubkey, &state);
        }

        if let Some(sender_event_pubkey) = sender_event_pubkey {
            return self
                .drain_pending(sender_event_pubkey, dist.key_id)
                .unwrap_or_default();
        }

        Vec::new()
    }

    fn handle_outer_event(&mut self, outer: &Event) -> Option<GroupDecryptedEvent> {
        if self.init().is_err() {
            return None;
        }
        if outer.kind != Kind::Custom(self.one_to_many.outer_kind() as u16) {
            return None;
        }
        if outer.verify().is_err() {
            return None;
        }

        let parsed = self.one_to_many.parse_outer_content(&outer.content).ok()?;
        let sender_event_pubkey = outer.pubkey;

        let sender_device_pubkey = self
            .sender_event_to_device
            .get(&sender_event_pubkey)
            .copied()
            .or_else(|| {
                self.load_sender_device_from_storage(sender_event_pubkey)
                    .ok()
                    .flatten()
            });

        let Some(sender_device_pubkey) = sender_device_pubkey else {
            self.queue_pending(sender_event_pubkey, parsed.key_id, outer.clone());
            return None;
        };

        let mut state = self
            .load_sender_key_state(sender_device_pubkey, parsed.key_id)
            .ok()
            .flatten();
        let Some(mut state) = state.take() else {
            self.queue_pending(sender_event_pubkey, parsed.key_id, outer.clone());
            return None;
        };

        let plaintext = parsed.decrypt(&mut state).ok()?;
        let _ = self.save_sender_key_state(sender_device_pubkey, &state);

        let inner = self.parse_inner_rumor(&plaintext, sender_device_pubkey, outer.created_at);
        if let Some(inner_group_id) = first_tag_value(&inner.tags, "l") {
            if inner_group_id != self.group_id() {
                return None;
            }
        }

        let sender_owner_pubkey = self
            .sender_device_to_owner
            .get(&sender_device_pubkey)
            .copied()
            .or_else(|| {
                self.storage
                    .get(&self.sender_owner_pubkey_key(sender_device_pubkey))
                    .ok()
                    .flatten()
                    .and_then(|hex| parse_pubkey_hex(&hex))
            });

        Some(GroupDecryptedEvent {
            group_id: self.group_id().to_string(),
            sender_event_pubkey,
            sender_device_pubkey,
            sender_owner_pubkey,
            outer_event_id: outer.id.to_string(),
            outer_created_at: outer.created_at.as_u64(),
            key_id: parsed.key_id,
            message_number: parsed.message_number,
            inner,
        })
    }

    fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;

        let group_prefix = format!(
            "{}/group/{}/sender/",
            self.version_prefix(),
            self.group_id()
        );
        let keys = self.storage.list(&group_prefix)?;

        for key in keys {
            let Some(rest) = key.strip_prefix(&group_prefix) else {
                continue;
            };
            let Some((sender_device_hex, suffix)) = rest.split_once('/') else {
                continue;
            };
            let Some(sender_device_pubkey) = parse_pubkey_hex(sender_device_hex) else {
                continue;
            };

            if suffix == "sender-event-pubkey" {
                let Some(value) = self.storage.get(&key)? else {
                    continue;
                };
                if let Some(sender_event_pubkey) = parse_pubkey_hex(&value) {
                    self.set_sender_event_mapping(sender_device_pubkey, sender_event_pubkey);
                }
                continue;
            }

            if suffix == "sender-owner-pubkey" {
                let Some(value) = self.storage.get(&key)? else {
                    continue;
                };
                if let Some(sender_owner_pubkey) = parse_pubkey_hex(&value) {
                    self.sender_device_to_owner
                        .insert(sender_device_pubkey, sender_owner_pubkey);
                }
            }
        }

        Ok(())
    }

    fn version_prefix(&self) -> &'static str {
        "v1/broadcast-channel"
    }

    fn group_sender_prefix(&self, sender_device_pubkey: PublicKey) -> String {
        format!(
            "{}/group/{}/sender/{}",
            self.version_prefix(),
            self.group_id(),
            pubkey_to_hex(&sender_device_pubkey)
        )
    }

    fn sender_event_secret_key_key(&self, sender_device_pubkey: PublicKey) -> String {
        format!(
            "{}/sender-event-secret-key",
            self.group_sender_prefix(sender_device_pubkey)
        )
    }

    fn sender_event_pubkey_key(&self, sender_device_pubkey: PublicKey) -> String {
        format!(
            "{}/sender-event-pubkey",
            self.group_sender_prefix(sender_device_pubkey)
        )
    }

    fn sender_owner_pubkey_key(&self, sender_device_pubkey: PublicKey) -> String {
        format!(
            "{}/sender-owner-pubkey",
            self.group_sender_prefix(sender_device_pubkey)
        )
    }

    fn latest_key_id_key(&self, sender_device_pubkey: PublicKey) -> String {
        format!(
            "{}/latest-key-id",
            self.group_sender_prefix(sender_device_pubkey)
        )
    }

    fn sender_key_state_key(&self, sender_device_pubkey: PublicKey, key_id: u32) -> String {
        format!(
            "{}/key/{}",
            self.group_sender_prefix(sender_device_pubkey),
            key_id
        )
    }

    fn set_sender_event_mapping(
        &mut self,
        sender_device_pubkey: PublicKey,
        sender_event_pubkey: PublicKey,
    ) {
        if let Some(prev_sender_event_pubkey) = self
            .sender_device_to_event
            .insert(sender_device_pubkey, sender_event_pubkey)
        {
            if prev_sender_event_pubkey != sender_event_pubkey {
                self.sender_event_to_device
                    .remove(&prev_sender_event_pubkey);
            }
        }
        self.sender_event_to_device
            .insert(sender_event_pubkey, sender_device_pubkey);
    }

    fn queue_pending(&mut self, sender_event_pubkey: PublicKey, key_id: u32, outer: Event) {
        self.pending_outer
            .entry((sender_event_pubkey, key_id))
            .or_default()
            .push(outer);
    }

    fn drain_pending(
        &mut self,
        sender_event_pubkey: PublicKey,
        key_id: u32,
    ) -> Result<Vec<GroupDecryptedEvent>> {
        let Some(pending) = self.pending_outer.remove(&(sender_event_pubkey, key_id)) else {
            return Ok(Vec::new());
        };
        if pending.is_empty() {
            return Ok(Vec::new());
        }

        let mut with_message_number: Vec<(Event, u32)> = pending
            .into_iter()
            .map(|outer| {
                let message_number = self
                    .one_to_many
                    .parse_outer_content(&outer.content)
                    .map(|parsed| parsed.message_number)
                    .unwrap_or(0);
                (outer, message_number)
            })
            .collect();
        with_message_number.sort_by_key(|(_, message_number)| *message_number);

        let mut results = Vec::new();
        for (outer, _) in with_message_number {
            if let Some(decrypted) = self.handle_outer_event(&outer) {
                results.push(decrypted);
            }
        }
        Ok(results)
    }

    fn ensure_our_sender_event_keys(&mut self) -> Result<(Keys, PublicKey, bool)> {
        self.init()?;

        if let Some(stored_secret_hex) = self
            .storage
            .get(&self.sender_event_secret_key_key(self.our_device_pubkey))?
        {
            if let Ok(secret_bytes) = hex::decode(stored_secret_hex) {
                if secret_bytes.len() == 32 {
                    if let Ok(secret_key) = SecretKey::from_slice(&secret_bytes) {
                        let keys = Keys::new(secret_key);
                        let sender_event_pubkey = keys.public_key();
                        self.set_sender_event_mapping(self.our_device_pubkey, sender_event_pubkey);
                        self.storage.put(
                            &self.sender_event_pubkey_key(self.our_device_pubkey),
                            pubkey_to_hex(&sender_event_pubkey),
                        )?;
                        return Ok((keys, sender_event_pubkey, false));
                    }
                }
            }
        }

        let keys = Keys::generate();
        let sender_event_pubkey = keys.public_key();
        self.storage.put(
            &self.sender_event_secret_key_key(self.our_device_pubkey),
            hex::encode(keys.secret_key().to_secret_bytes()),
        )?;
        self.storage.put(
            &self.sender_event_pubkey_key(self.our_device_pubkey),
            pubkey_to_hex(&sender_event_pubkey),
        )?;
        self.set_sender_event_mapping(self.our_device_pubkey, sender_event_pubkey);

        Ok((keys, sender_event_pubkey, true))
    }

    fn load_sender_key_state(
        &self,
        sender_device_pubkey: PublicKey,
        key_id: u32,
    ) -> Result<Option<SenderKeyState>> {
        let Some(data) = self
            .storage
            .get(&self.sender_key_state_key(sender_device_pubkey, key_id))?
        else {
            return Ok(None);
        };
        let state: SenderKeyState = serde_json::from_str(&data)?;
        Ok(Some(state))
    }

    fn save_sender_key_state(
        &self,
        sender_device_pubkey: PublicKey,
        state: &SenderKeyState,
    ) -> Result<()> {
        let serialized = serde_json::to_string(state)?;
        self.storage.put(
            &self.sender_key_state_key(sender_device_pubkey, state.key_id),
            serialized,
        )?;
        Ok(())
    }

    fn ensure_our_sender_key_state(
        &mut self,
        force_rotate: bool,
    ) -> Result<(SenderKeyState, bool)> {
        self.init()?;

        if force_rotate {
            let key_id = random::<u32>();
            let chain_key = random::<[u8; 32]>();
            let state = SenderKeyState::new(key_id, chain_key, 0);
            self.save_sender_key_state(self.our_device_pubkey, &state)?;
            self.storage.put(
                &self.latest_key_id_key(self.our_device_pubkey),
                key_id.to_string(),
            )?;
            return Ok((state, true));
        }

        if let Some(latest_key_id) = self
            .storage
            .get(&self.latest_key_id_key(self.our_device_pubkey))?
            .and_then(|v| v.parse::<u32>().ok())
        {
            if let Some(existing) =
                self.load_sender_key_state(self.our_device_pubkey, latest_key_id)?
            {
                return Ok((existing, false));
            }
        }

        let key_id = random::<u32>();
        let chain_key = random::<[u8; 32]>();
        let state = SenderKeyState::new(key_id, chain_key, 0);
        self.save_sender_key_state(self.our_device_pubkey, &state)?;
        self.storage.put(
            &self.latest_key_id_key(self.our_device_pubkey),
            key_id.to_string(),
        )?;
        Ok((state, true))
    }

    fn build_distribution(
        &self,
        now_seconds: u64,
        sender_event_pubkey: PublicKey,
        sender_key: &SenderKeyState,
    ) -> SenderKeyDistribution {
        SenderKeyDistribution {
            group_id: self.group_id().to_string(),
            key_id: sender_key.key_id,
            chain_key: sender_key.chain_key(),
            iteration: sender_key.iteration(),
            created_at: now_seconds,
            sender_event_pubkey: Some(pubkey_to_hex(&sender_event_pubkey)),
        }
    }

    fn build_distribution_rumor(
        &self,
        now_seconds: u64,
        now_ms: u64,
        dist: &SenderKeyDistribution,
    ) -> Result<UnsignedEvent> {
        let tags = vec![
            parse_tag(&["l".to_string(), self.group_id().to_string()])?,
            parse_tag(&["key".to_string(), dist.key_id.to_string()])?,
            parse_tag(&["ms".to_string(), now_ms.to_string()])?,
        ];

        Ok(EventBuilder::new(
            Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            serde_json::to_string(dist)?,
        )
        .tags(tags)
        .custom_created_at(Timestamp::from(now_seconds))
        .build(self.our_device_pubkey))
    }

    fn build_group_inner_rumor(
        &self,
        now_seconds: u64,
        now_ms: u64,
        event: GroupSendEvent,
    ) -> Result<UnsignedEvent> {
        let mut has_group_tag = false;
        let mut has_ms_tag = false;
        let mut tags: Vec<Tag> = event
            .tags
            .iter()
            .filter_map(|parts| {
                if parts.first().map(|v| v.as_str()) == Some("l")
                    && parts.get(1).map(|v| v.as_str()) == Some(self.group_id())
                {
                    has_group_tag = true;
                }
                if parts.first().map(|v| v.as_str()) == Some("ms") {
                    has_ms_tag = true;
                }
                Tag::parse(parts).ok()
            })
            .collect();

        if !has_group_tag {
            tags.insert(
                0,
                parse_tag(&["l".to_string(), self.group_id().to_string()])?,
            );
        }
        if !has_ms_tag {
            tags.push(parse_tag(&["ms".to_string(), now_ms.to_string()])?);
        }

        Ok(
            EventBuilder::new(Kind::Custom(event.kind as u16), event.content)
                .tags(tags)
                .custom_created_at(Timestamp::from(now_seconds))
                .build(self.our_device_pubkey),
        )
    }

    fn parse_inner_rumor(
        &self,
        plaintext: &str,
        sender_device_pubkey: PublicKey,
        fallback_created_at: Timestamp,
    ) -> UnsignedEvent {
        if let Ok(inner) = serde_json::from_str::<UnsignedEvent>(plaintext) {
            return inner;
        }

        if let Some(minimal) =
            self.parse_minimal_rumor_json(plaintext, sender_device_pubkey, fallback_created_at)
        {
            return minimal;
        }

        EventBuilder::new(Kind::Custom(CHAT_MESSAGE_KIND as u16), plaintext)
            .tags(vec![Tag::parse(&[
                "l".to_string(),
                self.group_id().to_string(),
            ])
            .expect("group tag should be valid")])
            .custom_created_at(fallback_created_at)
            .build(sender_device_pubkey)
    }

    fn parse_minimal_rumor_json(
        &self,
        plaintext: &str,
        sender_device_pubkey: PublicKey,
        fallback_created_at: Timestamp,
    ) -> Option<UnsignedEvent> {
        let value: serde_json::Value = serde_json::from_str(plaintext).ok()?;
        let obj = value.as_object()?;

        let kind_u64 = obj.get("kind")?.as_u64()?;
        if kind_u64 > u16::MAX as u64 {
            return None;
        }
        let kind = Kind::Custom(kind_u64 as u16);
        let content = obj.get("content")?.as_str()?.to_string();

        let mut tags: Vec<Tag> = obj
            .get("tags")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let arr = v.as_array()?;
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect();
                if parts.len() != arr.len() {
                    return None;
                }
                Tag::parse(&parts).ok()
            })
            .collect();

        if !tags.iter().any(|tag| {
            let parts = tag.clone().to_vec();
            parts.first().map(|s| s.as_str()) == Some("l")
        }) {
            tags.push(
                Tag::parse(&["l".to_string(), self.group_id().to_string()])
                    .expect("group tag should be valid"),
            );
        }

        let created_at = obj
            .get("created_at")
            .and_then(|v| v.as_u64())
            .map(Timestamp::from)
            .unwrap_or(fallback_created_at);

        let pubkey = obj
            .get("pubkey")
            .and_then(|v| v.as_str())
            .and_then(parse_pubkey_hex)
            .unwrap_or(sender_device_pubkey);

        Some(
            EventBuilder::new(kind, content)
                .tags(tags)
                .custom_created_at(created_at)
                .build(pubkey),
        )
    }

    fn load_sender_device_from_storage(
        &mut self,
        sender_event_pubkey: PublicKey,
    ) -> Result<Option<PublicKey>> {
        let group_prefix = format!(
            "{}/group/{}/sender/",
            self.version_prefix(),
            self.group_id()
        );
        let keys = self.storage.list(&group_prefix)?;

        for key in keys {
            let Some(rest) = key.strip_prefix(&group_prefix) else {
                continue;
            };
            let Some((sender_device_hex, suffix)) = rest.split_once('/') else {
                continue;
            };
            if suffix != "sender-event-pubkey" {
                continue;
            }
            let Some(stored_sender_event_hex) = self.storage.get(&key)? else {
                continue;
            };
            let Some(stored_sender_event_pubkey) = parse_pubkey_hex(&stored_sender_event_hex)
            else {
                continue;
            };
            if stored_sender_event_pubkey != sender_event_pubkey {
                continue;
            }
            let Some(sender_device_pubkey) = parse_pubkey_hex(sender_device_hex) else {
                continue;
            };
            self.set_sender_event_mapping(sender_device_pubkey, sender_event_pubkey);
            return Ok(Some(sender_device_pubkey));
        }

        Ok(None)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn parse_sender_key_distribution(content: &str) -> Option<SenderKeyDistribution> {
    let dist: SenderKeyDistribution = serde_json::from_str(content).ok()?;
    if dist.group_id.is_empty() {
        return None;
    }
    if dist
        .sender_event_pubkey
        .as_deref()
        .is_some_and(|hex| parse_pubkey_hex(hex).is_none())
    {
        return None;
    }
    Some(dist)
}

fn parse_pubkey_hex(hex: &str) -> Option<PublicKey> {
    crate::utils::pubkey_from_hex(hex).ok()
}

fn pubkey_to_hex(pubkey: &PublicKey) -> String {
    hex::encode(pubkey.to_bytes())
}

fn first_tag_value(tags: &nostr::Tags, key: &str) -> Option<String> {
    tags.iter().find_map(|tag| {
        let parts = tag.clone().to_vec();
        if parts.first().map(|s| s.as_str()) == Some(key) {
            parts.get(1).cloned()
        } else {
            None
        }
    })
}

fn parse_tag(parts: &[String]) -> Result<Tag> {
    Tag::parse(parts).map_err(|e| Error::InvalidEvent(e.to_string()))
}
