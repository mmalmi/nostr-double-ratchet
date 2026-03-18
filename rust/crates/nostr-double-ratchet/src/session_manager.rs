use crate::{
    apply_app_keys_snapshot, is_app_keys_event, resolve_invite_owner_routing, AppKeys,
    AppKeysSnapshotDecision, DeviceEntry, Invite, InviteAcceptInput, InviteProcessResponseInput,
    InviteProcessResponseResult, MessageQueue, OneToManyChannel, Result, SenderKeyDistribution,
    SenderKeyState, Session, SessionReceiveInput, SessionReceiveResult, SessionSendInput,
    StorageAdapter, UserRecord,
    GROUP_SENDER_KEY_DISTRIBUTION_KIND,
};
use nostr::{Keys, PublicKey, Tag, UnsignedEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub enum SessionManagerEffect {
    Publish(UnsignedEvent),
    PublishSigned(nostr::Event),
    Subscribe { subid: String, filter_json: String },
    Unsubscribe { subid: String },
    SchedulePublishSigned { delay_ms: u64, event: nostr::Event },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionManagerStorageEffect {
    Get { key: String },
    Put { key: String, value: String },
    Delete { key: String },
    List { prefix: String },
}

#[derive(Debug, Clone, Default)]
pub struct SessionManagerStorageResults {
    pub gets: HashMap<String, Option<String>>,
    pub lists: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionManagerNotification {
    DecryptedMessage {
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ManagerOutput<T> {
    pub value: T,
    pub effects: Vec<SessionManagerEffect>,
    pub storage_effects: Vec<SessionManagerStorageEffect>,
    pub notifications: Vec<SessionManagerNotification>,
}

impl<T> ManagerOutput<T> {
    pub fn empty(value: T) -> Self {
        Self {
            value,
            effects: Vec::new(),
            storage_effects: Vec::new(),
            notifications: Vec::new(),
        }
    }

    pub fn with_effects(value: T, effects: Vec<SessionManagerEffect>) -> Self {
        Self {
            value,
            effects,
            storage_effects: Vec::new(),
            notifications: Vec::new(),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> ManagerOutput<U> {
        ManagerOutput {
            value: f(self.value),
            effects: self.effects,
            storage_effects: self.storage_effects,
            notifications: self.notifications,
        }
    }

    pub fn append_unit(&mut self, other: ManagerOutput<()>) {
        self.effects.extend(other.effects);
        self.storage_effects.extend(other.storage_effects);
        self.notifications.extend(other.notifications);
    }

    pub fn push_effect(&mut self, effect: SessionManagerEffect) {
        self.effects.push(effect);
    }

    pub fn push_notification(&mut self, notification: SessionManagerNotification) {
        self.notifications.push(notification);
    }

    pub fn push_storage_effect(&mut self, effect: SessionManagerStorageEffect) {
        self.storage_effects.push(effect);
    }
}

pub fn emit_session_manager_output<T>(
    tx: &crossbeam_channel::Sender<SessionManagerEvent>,
    output: ManagerOutput<T>,
) -> Result<T> {
    for effect in output.effects {
        match effect {
            SessionManagerEffect::Publish(unsigned) => tx
                .send(SessionManagerEvent::Publish(unsigned))
                .map_err(|e| crate::Error::Storage(e.to_string()))?,
            SessionManagerEffect::PublishSigned(signed) => tx
                .send(SessionManagerEvent::PublishSigned(signed))
                .map_err(|e| crate::Error::Storage(e.to_string()))?,
            SessionManagerEffect::Subscribe { subid, filter_json } => tx
                .send(SessionManagerEvent::Subscribe { subid, filter_json })
                .map_err(|e| crate::Error::Storage(e.to_string()))?,
            SessionManagerEffect::Unsubscribe { subid } => tx
                .send(SessionManagerEvent::Unsubscribe(subid))
                .map_err(|e| crate::Error::Storage(e.to_string()))?,
            SessionManagerEffect::SchedulePublishSigned { delay_ms, event } => {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    let _ = tx.send(SessionManagerEvent::PublishSigned(event));
                });
            }
        }
    }

    for notification in output.notifications {
        match notification {
            SessionManagerNotification::DecryptedMessage {
                sender,
                sender_device,
                content,
                event_id,
            } => tx
                .send(SessionManagerEvent::DecryptedMessage {
                    sender,
                    sender_device,
                    content,
                    event_id,
                })
                .map_err(|e| crate::Error::Storage(e.to_string()))?,
        }
    }

    Ok(output.value)
}

pub fn resolve_session_manager_storage_reads(
    storage: &dyn StorageAdapter,
    effects: &[SessionManagerStorageEffect],
) -> Result<SessionManagerStorageResults> {
    let mut results = SessionManagerStorageResults::default();
    for effect in effects {
        match effect {
            SessionManagerStorageEffect::Get { key } => {
                results.gets.insert(key.clone(), storage.get(key)?);
            }
            SessionManagerStorageEffect::List { prefix } => {
                results.lists.insert(prefix.clone(), storage.list(prefix)?);
            }
            SessionManagerStorageEffect::Put { .. } | SessionManagerStorageEffect::Delete { .. } => {
            }
        }
    }
    Ok(results)
}

pub fn apply_session_manager_storage_writes(
    storage: &dyn StorageAdapter,
    effects: &[SessionManagerStorageEffect],
) -> Result<()> {
    for effect in effects {
        match effect {
            SessionManagerStorageEffect::Put { key, value } => storage.put(key, value.clone())?,
            SessionManagerStorageEffect::Delete { key } => storage.del(key)?,
            SessionManagerStorageEffect::Get { .. } | SessionManagerStorageEffect::List { .. } => {}
        }
    }
    Ok(())
}

pub fn persist_session_manager_output<T>(
    storage: &dyn StorageAdapter,
    output: &mut ManagerOutput<T>,
) -> Result<()> {
    apply_session_manager_storage_writes(storage, &output.storage_effects)?;
    output.storage_effects.clear();
    Ok(())
}

pub fn persist_and_emit_session_manager_output<T>(
    storage: &dyn StorageAdapter,
    tx: &crossbeam_channel::Sender<SessionManagerEvent>,
    mut output: ManagerOutput<T>,
) -> Result<T> {
    persist_session_manager_output(storage, &mut output)?;
    emit_session_manager_output(tx, output)
}

pub fn initialize_session_manager(
    storage: &dyn StorageAdapter,
    manager: &SessionManager,
) -> Result<ManagerOutput<()>> {
    let begin = manager.begin_init()?;
    let mut reads = resolve_session_manager_storage_reads(storage, &begin.storage_effects)?;
    let listed_keys: Vec<String> = reads
        .lists
        .values()
        .flat_map(|keys| keys.iter().cloned())
        .collect();
    for key in listed_keys {
        if !reads.gets.contains_key(&key) {
            reads.gets.insert(key.clone(), storage.get(&key)?);
        }
    }

    let mut output = manager.finish_init(reads)?;
    persist_session_manager_output(storage, &mut output)?;
    Ok(output)
}

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

type GroupSenderKeySlot = (PublicKey, u32);
type GroupSenderKeyPending = HashMap<GroupSenderKeySlot, Vec<nostr::Event>>;
type SessionBookTask = Box<dyn FnOnce(&mut HashMap<PublicKey, UserRecord>) + Send + 'static>;

const INVITE_BOOTSTRAP_EXPIRATION_SECONDS: u64 = 60;
const INVITE_BOOTSTRAP_RETRY_DELAYS_MS: [u64; 3] = [0, 500, 1500];
const MAX_PENDING_INVITE_RESPONSES: usize = 1_000;

#[derive(Clone)]
struct SessionBookActor {
    tx: crossbeam_channel::Sender<SessionBookTask>,
}

impl SessionBookActor {
    fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<SessionBookTask>();
        std::thread::spawn(move || {
            let mut records = HashMap::<PublicKey, UserRecord>::new();
            while let Ok(task) = rx.recv() {
                task(&mut records);
            }
        });

        Self { tx }
    }

    fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut HashMap<PublicKey, UserRecord>) -> R + Send + 'static,
    ) -> R {
        let (result_tx, result_rx) = crossbeam_channel::bounded(1);
        self.tx
            .send(Box::new(move |records| {
                let result = f(records);
                let _ = result_tx.send(result);
            }))
            .expect("session book actor should accept tasks");
        result_rx
            .recv()
            .expect("session book actor should return a result")
    }
}

pub struct SessionManager {
    user_records: SessionBookActor,
    our_public_key: PublicKey,
    our_identity_key: [u8; 32],
    device_id: String,
    owner_public_key: PublicKey,
    initialized: Arc<Mutex<bool>>,
    invite_state: Arc<Mutex<Option<InviteState>>>,
    provided_invite: Option<Invite>,
    delegate_to_owner: Arc<Mutex<HashMap<PublicKey, PublicKey>>>,
    cached_app_keys: Arc<Mutex<HashMap<PublicKey, AppKeys>>>,
    processed_invite_responses: Arc<Mutex<HashSet<String>>>,
    pending_invite_responses: Arc<Mutex<VecDeque<nostr::Event>>>,
    message_history: Arc<Mutex<HashMap<PublicKey, Vec<UnsignedEvent>>>>,
    latest_app_keys_created_at: Arc<Mutex<HashMap<PublicKey, u64>>>,
    message_queue: MessageQueue,
    discovery_queue: MessageQueue,
    invite_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    app_keys_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
    session_subscriptions: Arc<Mutex<HashMap<PublicKey, String>>>,
    pending_acceptances: Arc<Mutex<HashSet<PublicKey>>>,
    default_send_options: Arc<Mutex<Option<crate::SendOptions>>>,
    peer_send_options: Arc<Mutex<HashMap<PublicKey, crate::SendOptions>>>,
    group_send_options: Arc<Mutex<HashMap<String, crate::SendOptions>>>,
    auto_adopt_chat_settings: Arc<Mutex<bool>>,
    group_sender_events: Arc<Mutex<HashMap<PublicKey, GroupSenderEventInfo>>>,
    group_sender_key_states: Arc<Mutex<HashMap<(PublicKey, u32), SenderKeyState>>>,
    group_sender_key_pending: Arc<Mutex<GroupSenderKeyPending>>,
    group_sender_event_subscriptions: Arc<Mutex<HashSet<PublicKey>>>,
}

impl SessionManager {
    fn session_can_receive(session: &Session) -> bool {
        session.state.receiving_chain_key.is_some()
            || session.state.their_current_nostr_public_key.is_some()
            || session.state.receiving_chain_message_number > 0
    }

    fn with_user_records<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut HashMap<PublicKey, UserRecord>) -> R + Send + 'static,
    ) -> R {
        self.user_records.call(f)
    }

    fn session_author_pubkeys(session: &Session) -> HashSet<PublicKey> {
        let mut authors = HashSet::new();
        if let Some(pk) = session.state.their_current_nostr_public_key {
            authors.insert(pk);
        }
        if let Some(pk) = session.state.their_next_nostr_public_key {
            authors.insert(pk);
        }
        authors
    }

    fn refresh_session_subscriptions(&self) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let wanted = self.with_user_records(|records| {
            let mut authors = HashSet::new();
            for user_record in records.values() {
                for device_record in user_record.device_records.values() {
                    if let Some(session) = device_record.active_session.as_ref() {
                        authors.extend(Self::session_author_pubkeys(session));
                    }
                    for session in &device_record.inactive_sessions {
                        authors.extend(Self::session_author_pubkeys(session));
                    }
                }
            }
            authors
        });

        let mut subscriptions = self.session_subscriptions.lock().unwrap();
        let existing: Vec<PublicKey> = subscriptions.keys().copied().collect();

        for pubkey in existing {
            if wanted.contains(&pubkey) {
                continue;
            }

            if let Some(subid) = subscriptions.remove(&pubkey) {
                output.push_effect(SessionManagerEffect::Unsubscribe { subid });
            }
        }

        for pubkey in wanted {
            if subscriptions.contains_key(&pubkey) {
                continue;
            }

            let filter = crate::pubsub::build_filter()
                .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                .authors(vec![pubkey])
                .build();

            let Ok(filter_json) = serde_json::to_string(&filter) else {
                continue;
            };
            let subid = format!("session-{}", uuid::Uuid::new_v4());
            output.push_effect(SessionManagerEffect::Subscribe {
                subid: subid.clone(),
                filter_json,
            });
            subscriptions.insert(pubkey, subid);
        }

        output
    }

    fn subscribe_to_invite_responses(&self, invite: &Invite) -> Result<ManagerOutput<String>> {
        let filter = crate::pubsub::build_filter()
            .kinds(vec![crate::INVITE_RESPONSE_KIND as u64])
            .pubkeys(vec![invite.inviter_ephemeral_public_key])
            .build();

        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-response-{}", uuid::Uuid::new_v4());
        Ok(ManagerOutput::with_effects(
            subid.clone(),
            vec![SessionManagerEffect::Subscribe { subid, filter_json }],
        ))
    }

    fn subscribe_to_user_invites(&self, user_pubkey: PublicKey) -> Result<ManagerOutput<String>> {
        let filter = nostr::Filter::new()
            .kind(nostr::Kind::from(crate::INVITE_EVENT_KIND as u16))
            .authors(vec![user_pubkey])
            .custom_tag(
                nostr::types::filter::SingleLetterTag::lowercase(nostr::types::filter::Alphabet::L),
                ["double-ratchet/invites"],
            );

        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-user-{}", uuid::Uuid::new_v4());
        Ok(ManagerOutput::with_effects(
            subid.clone(),
            vec![SessionManagerEffect::Subscribe { subid, filter_json }],
        ))
    }

    fn send_with_session(session: &mut Session, event: UnsignedEvent) -> Result<nostr::Event> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let output = session.send_event(SessionSendInput {
            inner_event: event,
            now_secs: now.as_secs(),
            now_ms: now.as_millis() as u64,
        })?;
        *session = output.next;
        Ok(output.outer_event)
    }

    fn receive_with_session(session: &mut Session, event: &nostr::Event) -> Result<Option<String>> {
        let replacement_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
        match session.receive_event(SessionReceiveInput {
            outer_event: event.clone(),
            replacement_next_nostr_private_key,
        }) {
            SessionReceiveResult::NotForThisSession { .. } => Ok(None),
            SessionReceiveResult::Decrypted {
                next, plaintext, ..
            } => {
                *session = next;
                Ok(Some(plaintext))
            }
            SessionReceiveResult::InvalidRelevant { error, .. } => Err(error),
        }
    }

    fn accept_invite_session(&self, invite: &Invite) -> Result<(Session, nostr::Event)> {
        let invitee_session_key = Keys::generate().secret_key().to_secret_bytes();
        let invitee_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
        let envelope_sender_private_key = Keys::generate().secret_key().to_secret_bytes();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let two_days = 2 * 24 * 60 * 60;
        let response_created_at = now - (rand::random::<u64>() % two_days);

        let accepted = invite.accept(InviteAcceptInput {
            invitee_public_key: self.our_public_key,
            invitee_identity_private_key: self.our_identity_key,
            invitee_session_private_key: invitee_session_key,
            invitee_next_nostr_private_key,
            envelope_sender_private_key,
            response_created_at,
            device_id: Some(self.device_id.clone()),
            owner_public_key: Some(self.owner_public_key),
        })?;

        Ok((accepted.session, accepted.response_event))
    }

    pub fn set_default_send_options(
        &self,
        options: Option<crate::SendOptions>,
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        *self.default_send_options.lock().unwrap() = options.clone();

        let key = self.send_options_default_key();
        match options {
            Some(o) => output.push_storage_effect(SessionManagerStorageEffect::Put {
                key,
                value: serde_json::to_string(&o)?,
            }),
            None => output.push_storage_effect(SessionManagerStorageEffect::Delete { key }),
        }
        Ok(output)
    }

    pub fn set_peer_send_options(
        &self,
        peer_pubkey: PublicKey,
        options: Option<crate::SendOptions>,
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        let owner = self.resolve_to_owner(&peer_pubkey);
        let key = self.send_options_peer_key(&owner);

        if let Some(o) = options.clone() {
            self.peer_send_options
                .lock()
                .unwrap()
                .insert(owner, o.clone());
            output.push_storage_effect(SessionManagerStorageEffect::Put {
                key,
                value: serde_json::to_string(&o)?,
            });
        } else {
            self.peer_send_options.lock().unwrap().remove(&owner);
            output.push_storage_effect(SessionManagerStorageEffect::Delete { key });
        }
        Ok(output)
    }

    pub fn set_group_send_options(
        &self,
        group_id: String,
        options: Option<crate::SendOptions>,
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        let key = self.send_options_group_key(&group_id);

        if let Some(o) = options.clone() {
            self.group_send_options
                .lock()
                .unwrap()
                .insert(group_id.clone(), o.clone());
            output.push_storage_effect(SessionManagerStorageEffect::Put {
                key,
                value: serde_json::to_string(&o)?,
            });
        } else {
            self.group_send_options.lock().unwrap().remove(&group_id);
            output.push_storage_effect(SessionManagerStorageEffect::Delete { key });
        }
        Ok(output)
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
    pub fn delete_chat(&self, user_pubkey: PublicKey) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        let owner_pubkey = self.resolve_to_owner(&user_pubkey);
        if owner_pubkey == self.owner_public_key {
            return Ok(output);
        }
        output.append_unit(self.delete_user_local(owner_pubkey)?);
        Ok(output)
    }

    pub fn new(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        _storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        let message_queue = MessageQueue::new("v1/message-queue/");
        let discovery_queue = MessageQueue::new("v1/discovery-queue/");
        Self {
            user_records: SessionBookActor::new(),
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            initialized: Arc::new(Mutex::new(false)),
            invite_state: Arc::new(Mutex::new(None)),
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
            session_subscriptions: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn begin_init(&self) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        if *self.initialized.lock().unwrap() {
            return Ok(output);
        }
        if self.provided_invite.is_none() {
            output.push_storage_effect(SessionManagerStorageEffect::Get {
                key: self.device_invite_key(&self.device_id),
            });
        }
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.user_record_key_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::Get {
            key: self.send_options_default_key(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.send_options_peer_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.send_options_group_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.group_sender_event_info_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.group_sender_key_state_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.message_queue_prefix(),
        });
        output.push_storage_effect(SessionManagerStorageEffect::List {
            prefix: self.discovery_queue_prefix(),
        });
        Ok(output)
    }

    pub fn finish_init(&self, reads: SessionManagerStorageResults) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        {
            let initialized = self.initialized.lock().unwrap();
            if *initialized {
                return Ok(output);
            }
        }

        self.load_all_user_records_from_results(&reads)?;
        self.load_send_options_from_results(&reads);
        output.append_unit(self.load_group_sender_state_from_results(&reads)?);
        self.load_queue_from_results(&self.message_queue, &reads, &self.message_queue_prefix());
        self.load_queue_from_results(
            &self.discovery_queue,
            &reads,
            &self.discovery_queue_prefix(),
        );

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
            match reads.gets.get(&device_invite_key).cloned().flatten() {
                Some(data) => Invite::deserialize(&data)?,
                None => Invite::create_new(self.our_public_key, Some(self.device_id.clone()), None)?,
            }
        };

        output.push_storage_effect(SessionManagerStorageEffect::Put {
            key: device_invite_key,
            value: invite.serialize()?,
        });

        if invite.inviter_ephemeral_private_key.is_none() {
            return Err(crate::Error::Invite(
                "Invite missing ephemeral keys".to_string(),
            ));
        }

        *self.invite_state.lock().unwrap() = Some(InviteState {
            invite: invite.clone(),
            our_identity_key: self.our_identity_key,
        });

        output.append_unit(self.subscribe_to_invite_responses(&invite)?.map(|_| ()));

        if let Ok(unsigned) = invite.get_event() {
            let keys = Keys::new(nostr::SecretKey::from_slice(&self.our_identity_key)?);
            if let Ok(signed) = unsigned.sign_with_keys(&keys) {
                output.push_effect(SessionManagerEffect::PublishSigned(signed));
            }
        }

        output.append_unit(self.refresh_session_subscriptions());

        let active_device_ids = self.with_user_records(move |records| {
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
        });

        for device_id in active_device_ids {
            if let Ok(flush_output) = self.flush_message_queue(&device_id) {
                output.append_unit(flush_output);
            }
        }

        output.append_unit(self.setup_user(self.owner_public_key));
        *self.initialized.lock().unwrap() = true;
        Ok(output)
    }

    pub fn send_text(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<crate::SendOptions>,
    ) -> Result<ManagerOutput<Vec<String>>> {
        if text.trim().is_empty() {
            return Ok(ManagerOutput::empty(Vec::new()));
        }

        Ok(self
            .send_text_with_inner_id(recipient, text, options)?
            .map(|(_, event_ids)| event_ids))
    }

    /// Remove discovery queue entries older than `max_age_ms` milliseconds.
    pub fn cleanup_discovery_queue(&self, max_age_ms: u64) -> Result<usize> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Ok(self.discovery_queue.remove_expired(now_ms, max_age_ms)?.len())
    }

    #[deprecated(
        note = "use send_text(recipient, text, Some(SendOptions{ expires_at: Some(...) }))"
    )]
    pub fn send_text_with_expiration(
        &self,
        recipient: PublicKey,
        text: String,
        expires_at: u64,
    ) -> Result<ManagerOutput<Vec<String>>> {
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
    ) -> Result<ManagerOutput<(String, Vec<String>)>> {
        if text.trim().is_empty() {
            return Ok(ManagerOutput::empty((String::new(), Vec::new())));
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

        Ok(self.send_event(recipient, event)?.map(|event_ids| (inner_id, event_ids)))
    }

    /// Send an encrypted 1:1 chat settings event (inner kind 10448).
    ///
    /// Settings events themselves should never expire; they are sent without a NIP-40 expiration tag.
    pub fn send_chat_settings(
        &self,
        recipient: PublicKey,
        message_ttl_seconds: u64,
    ) -> Result<ManagerOutput<Vec<String>>> {
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
    ) -> Result<ManagerOutput<Vec<String>>> {
        let opts = if message_ttl_seconds == 0 {
            crate::SendOptions::default()
        } else {
            crate::SendOptions {
                ttl_seconds: Some(message_ttl_seconds),
                expires_at: None,
            }
        };
        let mut output = self.set_peer_send_options(peer_pubkey, Some(opts))?;
        let ManagerOutput {
            value: event_ids,
            effects,
            storage_effects,
            notifications,
        } = self.send_chat_settings(peer_pubkey, message_ttl_seconds)?;
        output.effects.extend(effects);
        output.storage_effects.extend(storage_effects);
        output.notifications.extend(notifications);
        Ok(output.map(|_| event_ids))
    }

    pub fn send_receipt(
        &self,
        recipient: PublicKey,
        receipt_type: &str,
        message_ids: Vec<String>,
        options: Option<crate::SendOptions>,
    ) -> Result<ManagerOutput<Vec<String>>> {
        if message_ids.is_empty() {
            return Ok(ManagerOutput::empty(Vec::new()));
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
    ) -> Result<ManagerOutput<Vec<String>>> {
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
    ) -> Result<ManagerOutput<Vec<String>>> {
        if message_id.trim().is_empty() || emoji.trim().is_empty() {
            return Ok(ManagerOutput::empty(Vec::new()));
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
        self.with_user_records(|records| records.keys().copied().collect())
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
    ) -> Result<ManagerOutput<()>> {
        let session = Session::new(state);

        self.with_user_records(move |records| {
            let user_record = records
                .entry(peer_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(peer_pubkey.to_bytes())));
            user_record.upsert_session(device_id.as_deref(), session);
        });

        let mut output = self.refresh_session_subscriptions();
        output.append_unit(self.store_user_record(&peer_pubkey)?);
        Ok(output)
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
    ) -> ManagerOutput<()> {
        self.handle_app_keys_event(owner_pubkey, app_keys, created_at)
    }

    pub fn pending_invite_response_owner_pubkeys(&self) -> Vec<PublicKey> {
        let Some((invite, our_identity_key)) = self
            .invite_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|state| (state.invite.clone(), state.our_identity_key))
        else {
            return Vec::new();
        };

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

            let inviter_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
            let InviteProcessResponseResult::Accepted { meta, .. } =
                invite.process_response(InviteProcessResponseInput {
                    event: event.clone(),
                    inviter_identity_private_key: our_identity_key,
                    inviter_next_nostr_private_key,
                })
            else {
                continue;
            };

            owners.insert(
                meta.owner_public_key
                    .unwrap_or_else(|| self.resolve_to_owner(&meta.invitee_identity)),
            );
        }

        let mut owners: Vec<PublicKey> = owners.into_iter().collect();
        owners.sort_by_key(|pubkey| pubkey.to_hex());
        owners
    }

    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<ManagerOutput<AcceptInviteResult>> {
        let inviter_device_pubkey = invite.inviter;
        if inviter_device_pubkey == self.our_public_key {
            return Err(crate::Error::Invite(
                "Cannot accept invite from this device".to_string(),
            ));
        }

        let claimed_owner_pubkey = owner_pubkey_hint
            .or(invite.owner_public_key)
            .unwrap_or_else(|| self.resolve_to_owner(&inviter_device_pubkey));
        let mut owner_pubkey = claimed_owner_pubkey;
        let mut used_link_bootstrap_exception = false;

        if claimed_owner_pubkey != inviter_device_pubkey {
            let cached_app_keys = self
                .cached_app_keys
                .lock()
                .unwrap()
                .get(&claimed_owner_pubkey)
                .cloned();
            if let Some(app_keys) = cached_app_keys {
                let routing = resolve_invite_owner_routing(
                    inviter_device_pubkey,
                    claimed_owner_pubkey,
                    invite.purpose.as_deref(),
                    self.owner_public_key,
                    Some(&app_keys),
                );
                owner_pubkey = routing.owner_pubkey;
                used_link_bootstrap_exception = routing.used_link_bootstrap_exception;
                if owner_pubkey == claimed_owner_pubkey {
                    self.update_delegate_mapping(claimed_owner_pubkey, &app_keys);
                }
            } else {
                let known_device_identities = self.with_user_records(move |records| {
                    records
                        .get(&claimed_owner_pubkey)
                        .map(|record| record.known_device_identities.clone())
                        .unwrap_or_default()
                });

                let stored_app_keys = (!known_device_identities.is_empty()).then(|| {
                    AppKeys::new(
                        known_device_identities
                            .iter()
                            .filter_map(|identity_hex| {
                                crate::utils::pubkey_from_hex(identity_hex)
                                    .ok()
                                    .map(|pubkey| DeviceEntry::new(pubkey, 0))
                            })
                            .collect(),
                    )
                });
                let routing = resolve_invite_owner_routing(
                    inviter_device_pubkey,
                    claimed_owner_pubkey,
                    invite.purpose.as_deref(),
                    self.owner_public_key,
                    stored_app_keys.as_ref(),
                );
                owner_pubkey = routing.owner_pubkey;
                used_link_bootstrap_exception = routing.used_link_bootstrap_exception;
                if owner_pubkey == claimed_owner_pubkey {
                    if let Some(app_keys) = stored_app_keys.as_ref() {
                        self.update_delegate_mapping(claimed_owner_pubkey, app_keys);
                    }
                }
            }
        }

        let device_id = invite
            .device_id
            .clone()
            .unwrap_or_else(|| hex::encode(inviter_device_pubkey.to_bytes()));

        let existing_device_session_info = self.with_user_records({
            let device_id = device_id.clone();
            move |records| {
                let device_record = records
                    .get(&owner_pubkey)
                    .and_then(|r| r.device_records.get(&device_id))?;

                let active_session = device_record.active_session.as_ref().map(|session| {
                    (
                        session.can_send(),
                        SessionManager::session_can_receive(session),
                        session.state.sending_chain_message_number,
                        session.state.receiving_chain_message_number,
                    )
                });

                let mut any_send_capable = active_session
                    .as_ref()
                    .is_some_and(|(can_send, _, _, _)| *can_send);
                let mut any_receive_capable = active_session
                    .as_ref()
                    .is_some_and(|(_, can_receive, _, _)| *can_receive);
                let mut any_session_has_activity = active_session.as_ref().is_some_and(
                    |(_, _, sent_messages, received_messages)| {
                        *sent_messages > 0 || *received_messages > 0
                    },
                );

                for session in &device_record.inactive_sessions {
                    if session.can_send() {
                        any_send_capable = true;
                    }
                    if SessionManager::session_can_receive(session) {
                        any_receive_capable = true;
                    }
                    if session.state.sending_chain_message_number > 0
                        || session.state.receiving_chain_message_number > 0
                    {
                        any_session_has_activity = true;
                    }
                }

                Some((
                    active_session,
                    any_send_capable,
                    any_receive_capable,
                    any_session_has_activity,
                ))
            }
        });
        let self_owner_authorized_device_replay = owner_pubkey == self.owner_public_key
            && inviter_device_pubkey != self.our_public_key
            && existing_device_session_info.is_some()
            && self.is_device_authorized(owner_pubkey, inviter_device_pubkey);

        if self_owner_authorized_device_replay
            || existing_device_session_info.is_some_and(
                |(_, any_send_capable, any_receive_capable, any_session_has_activity)| {
                    any_send_capable && (any_receive_capable || any_session_has_activity)
                },
            )
        {
            let output = self
                .record_known_device_identity(owner_pubkey, inviter_device_pubkey)
                .map(|_| AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: false,
            });
            return Ok(output);
        }
        let replace_existing_active_session = existing_device_session_info.is_some_and(
            |(active_session, _, _, any_session_has_activity)| {
                active_session.is_some_and(
                    |(can_send, can_receive, sent_messages, received_messages)| {
                        can_send
                            && !can_receive
                            && sent_messages == 0
                            && received_messages == 0
                            && !any_session_has_activity
                    },
                )
            },
        );
        let replace_receive_only_active_session =
            existing_device_session_info.is_some_and(|(active_session, _, _, _)| {
                active_session.is_some_and(
                    |(can_send, can_receive, sent_messages, received_messages)| {
                        !can_send && can_receive && sent_messages == 0 && received_messages == 0
                    },
                )
            });

        let replace_existing_active_session =
            replace_existing_active_session || replace_receive_only_active_session;

        {
            let mut pending = self.pending_acceptances.lock().unwrap();
            if pending.contains(&inviter_device_pubkey) {
                return Err(crate::Error::Invite(
                    "Invite acceptance already in progress".to_string(),
                ));
            }
            pending.insert(inviter_device_pubkey);
        }

        let result = (|| -> Result<ManagerOutput<AcceptInviteResult>> {
            let mut output = ManagerOutput::empty(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id: device_id.clone(),
                created_new_session: true,
            });
            let (mut session, response_event) = self.accept_invite_session(invite)?;
            output.push_effect(SessionManagerEffect::PublishSigned(response_event));

            let invite_bootstrap_messages = self.build_bootstrap_messages(owner_pubkey);
            let invite_bootstrap_events =
                SessionManager::sign_bootstrap_schedule(&mut session, &invite_bootstrap_messages);

            self.with_user_records({
                let device_id = device_id.clone();
                move |records| {
                    let user_record = records
                        .entry(owner_pubkey)
                        .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
                    SessionManager::upsert_device_record(user_record, &device_id);

                    if replace_existing_active_session {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let device_record = user_record
                            .device_records
                            .get_mut(&device_id)
                            .expect("device record should exist");

                        if let Some(active) = device_record.active_session.take() {
                            device_record.inactive_sessions.insert(0, active);
                        }
                        device_record.active_session = Some(session);
                        crate::UserRecord::compact_duplicate_sessions(device_record);

                        const MAX_INACTIVE: usize = 10;
                        if device_record.inactive_sessions.len() > MAX_INACTIVE {
                            device_record.inactive_sessions.truncate(MAX_INACTIVE);
                        }
                        device_record.last_activity = Some(now);
                    } else {
                        // Preserve an already-used active session so repeated invite replays
                        // don't clobber the established sending/receiving path for this device.
                        user_record.upsert_session(Some(&device_id), session);
                    }
                }
            });

            output.append_unit(self.refresh_session_subscriptions());
            output.append_unit(self.record_known_device_identity(owner_pubkey, inviter_device_pubkey));
            output.append_unit(self.store_user_record(&owner_pubkey)?);
            output.append_unit(self.send_message_history(owner_pubkey, &device_id));
            if !invite_bootstrap_events.is_empty() {
                output.append_unit(self.publish_bootstrap_schedule(invite_bootstrap_events));
            }
            if used_link_bootstrap_exception {
                output.append_unit(self.send_link_bootstrap(owner_pubkey, &device_id));
            }
            if let Ok(flush_output) = self.flush_message_queue(&device_id) {
                output.append_unit(flush_output);
            }

            Ok(output)
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
    ) -> Result<ManagerOutput<Vec<String>>> {
        let mut output = ManagerOutput::empty(Vec::new());
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
            output.append_unit(self.setup_user(*owner));
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
        let queued_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        for owner in &owners {
            let mut seen_for_owner = HashSet::new();
            let device_ids = owner_targets.get(owner).cloned().unwrap_or_default();
            let mut queued_any_device = false;
            for device_id in device_ids {
                if !seen_for_owner.insert(device_id.clone()) {
                    continue;
                }
                queued_any_device = true;
                if let Ok(entry) = self.message_queue.add(&device_id, &event, queued_at_ms) {
                    if let Ok(effect) = Self::queue_put_effect(&self.message_queue, &entry) {
                        output.push_storage_effect(effect);
                    }
                }
            }
            if !queued_any_device {
                if let Ok(entry) = self.discovery_queue.add(&owner.to_hex(), &event, queued_at_ms)
                {
                    if let Ok(effect) = Self::queue_put_effect(&self.discovery_queue, &entry) {
                        output.push_storage_effect(effect);
                    }
                }
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
        let mut updated_sessions = false;

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
                            user_record.known_device_identities.contains(&device_hex)
                        };

                        if !authorized {
                            return None;
                        }
                    }

                    user_record
                        .device_records
                        .get_mut(&device_id)
                        .and_then(|device_record| device_record.active_session.as_mut())
                        .and_then(|session| SessionManager::send_with_session(session, event).ok())
                }
            });

            if let Some(signed_event) = maybe_signed_event {
                updated_sessions = true;
                event_ids.push(signed_event.id.to_string());
                output.push_effect(SessionManagerEffect::PublishSigned(signed_event));
                published_device_ids.push(device_id.clone());
            }
        }

        if updated_sessions {
            output.append_unit(self.refresh_session_subscriptions());
        }

        if let Some(ref id) = inner_event_id {
            let mut seen = HashSet::new();
            for device_id in published_device_ids {
                if !seen.insert(device_id.clone()) {
                    continue;
                }
                if let Ok(Some(removed)) = self
                    .message_queue
                    .remove_by_target_and_event_id(&device_id, id)
                {
                    output.push_storage_effect(Self::queue_delete_effect(
                        &self.message_queue,
                        &removed.id,
                    ));
                }
                if let Ok(flush_output) = self.flush_message_queue(&device_id) {
                    output.append_unit(flush_output);
                }
            }
        }

        if !event_ids.is_empty() {
            output.append_unit(self.store_user_record(&recipient_owner)?);
            if include_owner_sync && self.owner_public_key != recipient_owner {
                output.append_unit(self.store_user_record(&self.owner_public_key)?);
            }
        }

        output.value = event_ids;
        Ok(output)
    }

    pub fn send_event(
        &self,
        recipient: PublicKey,
        event: UnsignedEvent,
    ) -> Result<ManagerOutput<Vec<String>>> {
        let recipient_owner = self.resolve_to_owner(&recipient);
        self.send_event_internal(recipient_owner, event, true)
    }

    pub fn send_event_recipient_only(
        &self,
        recipient: PublicKey,
        event: UnsignedEvent,
    ) -> Result<ManagerOutput<Vec<String>>> {
        let recipient_owner = self.resolve_to_owner(&recipient);
        self.send_event_internal(recipient_owner, event, false)
    }

    fn delete_user_local(&self, owner_pubkey: PublicKey) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        if owner_pubkey == self.owner_public_key {
            return Ok(output);
        }

        let (known_device_ids, known_identity_hexes) = self.with_user_records(move |records| {
            let Some(mut user_record) = records.remove(&owner_pubkey) else {
                return (Vec::new(), Vec::new());
            };

            let mut known_device_ids = Vec::new();
            for (device_id, device_record) in user_record.device_records.drain() {
                let _ = device_record.active_session;
                let _ = device_record.inactive_sessions;
                known_device_ids.push(device_id);
            }

            (known_device_ids, user_record.known_device_identities)
        });

        output.append_unit(self.refresh_session_subscriptions());

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

        for entry in self.discovery_queue.remove_for_target(&owner_pubkey.to_hex())? {
            output.push_storage_effect(Self::queue_delete_effect(
                &self.discovery_queue,
                &entry.id,
            ));
        }
        for device_id in known_device_ids {
            for entry in self.message_queue.remove_for_target(&device_id)? {
                output.push_storage_effect(Self::queue_delete_effect(
                    &self.message_queue,
                    &entry.id,
                ));
            }
        }

        output.push_storage_effect(SessionManagerStorageEffect::Delete {
            key: self.send_options_peer_key(&owner_pubkey),
        });
        output.push_storage_effect(SessionManagerStorageEffect::Delete {
            key: self.user_record_key(&owner_pubkey),
        });
        Ok(output)
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

    fn message_queue_prefix(&self) -> String {
        "v1/message-queue/".to_string()
    }

    fn discovery_queue_prefix(&self) -> String {
        "v1/discovery-queue/".to_string()
    }

    fn queue_put_effect(queue: &MessageQueue, entry: &crate::QueueEntry) -> Result<SessionManagerStorageEffect> {
        Ok(SessionManagerStorageEffect::Put {
            key: queue.key(&entry.id),
            value: serde_json::to_string(entry)?,
        })
    }

    fn queue_delete_effect(queue: &MessageQueue, id: &str) -> SessionManagerStorageEffect {
        SessionManagerStorageEffect::Delete {
            key: queue.key(id),
        }
    }

    fn load_send_options_from_results(&self, reads: &SessionManagerStorageResults) {
        *self.default_send_options.lock().unwrap() = None;
        self.peer_send_options.lock().unwrap().clear();
        self.group_send_options.lock().unwrap().clear();

        if let Some(Some(data)) = reads.gets.get(&self.send_options_default_key()) {
            if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(data) {
                *self.default_send_options.lock().unwrap() = Some(opts);
            }
        }

        for k in reads
            .lists
            .get(&self.send_options_peer_prefix())
            .cloned()
            .unwrap_or_default()
        {
            let hex_pk = k
                .strip_prefix(&self.send_options_peer_prefix())
                .unwrap_or("");
            if hex_pk.is_empty() {
                continue;
            }
            let Ok(pk) = crate::utils::pubkey_from_hex(hex_pk) else {
                continue;
            };
            if let Some(Some(data)) = reads.gets.get(&k) {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(data) {
                    self.peer_send_options.lock().unwrap().insert(pk, opts);
                }
            }
        }

        for k in reads
            .lists
            .get(&self.send_options_group_prefix())
            .cloned()
            .unwrap_or_default()
        {
            let group_id = k
                .strip_prefix(&self.send_options_group_prefix())
                .unwrap_or("")
                .to_string();
            if group_id.is_empty() {
                continue;
            }
            if let Some(Some(data)) = reads.gets.get(&k) {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(data) {
                    self.group_send_options
                        .lock()
                        .unwrap()
                        .insert(group_id, opts);
                }
            }
        }
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

    fn maybe_auto_adopt_chat_settings(
        &self,
        from_owner_pubkey: PublicKey,
        rumor: &UnsignedEvent,
    ) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        if !*self.auto_adopt_chat_settings.lock().unwrap() {
            return output;
        }

        if rumor.kind.as_u16() != crate::CHAT_SETTINGS_KIND as u16 {
            return output;
        }

        let payload = match serde_json::from_str::<serde_json::Value>(&rumor.content) {
            Ok(v) => v,
            Err(_) => return output,
        };

        let typ = payload.get("type").and_then(|v| v.as_str());
        let v = payload.get("v").and_then(|v| v.as_u64());
        if typ != Some("chat-settings") || v != Some(1) {
            return output;
        }

        let Some(peer_pubkey) = self.chat_settings_peer_pubkey(from_owner_pubkey, rumor) else {
            return output;
        };

        match payload.get("messageTtlSeconds") {
            // Missing: clear per-peer override (fall back to global default).
            None => {
                if let Ok(set_output) = self.set_peer_send_options(peer_pubkey, None) {
                    output.append_unit(set_output);
                }
            }
            // Null: disable per-peer expiration (even if a global default exists).
            Some(serde_json::Value::Null) => {
                if let Ok(set_output) =
                    self.set_peer_send_options(peer_pubkey, Some(crate::SendOptions::default()))
                {
                    output.append_unit(set_output);
                }
            }
            Some(serde_json::Value::Number(n)) => {
                let Some(ttl) = n.as_u64() else {
                    return output;
                };
                let opts = if ttl == 0 {
                    crate::SendOptions::default()
                } else {
                    crate::SendOptions {
                        ttl_seconds: Some(ttl),
                        expires_at: None,
                    }
                };
                if let Ok(set_output) = self.set_peer_send_options(peer_pubkey, Some(opts)) {
                    output.append_unit(set_output);
                }
            }
            _ => {}
        }
        output
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

    fn group_sender_key_state_prefix(&self) -> String {
        "group-sender-key/state/".to_string()
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

    fn load_group_sender_state_from_results(
        &self,
        reads: &SessionManagerStorageResults,
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        self.group_sender_events.lock().unwrap().clear();
        self.group_sender_key_states.lock().unwrap().clear();

        let prefix = self.group_sender_event_info_prefix();
        for key in reads.lists.get(&prefix).cloned().unwrap_or_default() {
            let Some(hex_pk) = key.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(sender_event_pubkey) = crate::utils::pubkey_from_hex(hex_pk) else {
                continue;
            };
            let Some(Some(data)) = reads.gets.get(&key) else {
                continue;
            };

            let stored: StoredGroupSenderEventInfo = match serde_json::from_str(data) {
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

            if let Ok(subscribe_output) = self.subscribe_to_group_sender_event(sender_event_pubkey) {
                output.append_unit(subscribe_output);
            }
        }

        let state_prefix = self.group_sender_key_state_prefix();
        for key in reads.lists.get(&state_prefix).cloned().unwrap_or_default() {
            let Some(rest) = key.strip_prefix(&state_prefix) else {
                continue;
            };
            let Some((sender_hex, key_id_str)) = rest.split_once('/') else {
                continue;
            };
            let Ok(sender_event_pubkey) = crate::utils::pubkey_from_hex(sender_hex) else {
                continue;
            };
            let Ok(key_id) = key_id_str.parse::<u32>() else {
                continue;
            };
            let Some(Some(data)) = reads.gets.get(&key) else {
                continue;
            };
            let Ok(state) = serde_json::from_str::<SenderKeyState>(data) else {
                continue;
            };
            self.group_sender_key_states
                .lock()
                .unwrap()
                .insert((sender_event_pubkey, key_id), state);
        }

        Ok(output)
    }

    fn subscribe_to_group_sender_event(
        &self,
        sender_event_pubkey: PublicKey,
    ) -> Result<ManagerOutput<()>> {
        {
            let mut subs = self.group_sender_event_subscriptions.lock().unwrap();
            if subs.contains(&sender_event_pubkey) {
                return Ok(ManagerOutput::empty(()));
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
        Ok(ManagerOutput::with_effects(
            (),
            vec![SessionManagerEffect::Subscribe { subid, filter_json }],
        ))
    }

    fn load_group_sender_event_info(
        &self,
        sender_event_pubkey: &PublicKey,
    ) -> Option<GroupSenderEventInfo> {
        self.group_sender_events
            .lock()
            .unwrap()
            .get(sender_event_pubkey)
            .cloned()
    }

    fn load_sender_key_state(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
    ) -> Option<SenderKeyState> {
        self.group_sender_key_states
            .lock()
            .unwrap()
            .get(&(*sender_event_pubkey, key_id))
            .cloned()
    }

    fn store_sender_key_state(
        &self,
        sender_event_pubkey: &PublicKey,
        key_id: u32,
        state: &SenderKeyState,
    ) -> Result<ManagerOutput<()>> {
        let key = self.group_sender_key_state_key(sender_event_pubkey, key_id);
        self.group_sender_key_states
            .lock()
            .unwrap()
            .insert((*sender_event_pubkey, key_id), state.clone());
        Ok(ManagerOutput {
            value: (),
            effects: Vec::new(),
            storage_effects: vec![SessionManagerStorageEffect::Put {
                key,
                value: serde_json::to_string(state)?,
            }],
            notifications: Vec::new(),
        })
    }

    fn ensure_sender_key_state_from_distribution(
        &self,
        sender_event_pubkey: PublicKey,
        dist: &SenderKeyDistribution,
    ) -> Result<ManagerOutput<()>> {
        if self
            .load_sender_key_state(&sender_event_pubkey, dist.key_id)
            .is_some()
        {
            return Ok(ManagerOutput::empty(()));
        }

        let state = SenderKeyState::new(dist.key_id, dist.chain_key, dist.iteration);
        self.store_sender_key_state(&sender_event_pubkey, dist.key_id, &state)
    }

    fn store_group_sender_event_info(
        &self,
        sender_event_pubkey: PublicKey,
        info: &GroupSenderEventInfo,
    ) -> Result<ManagerOutput<()>> {
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
        let mut output = self.subscribe_to_group_sender_event(sender_event_pubkey)?;
        output.push_storage_effect(SessionManagerStorageEffect::Put {
            key,
            value: serde_json::to_string(&stored)?,
        });
        Ok(output)
    }

    fn maybe_handle_group_sender_key_distribution(
        &self,
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
        rumor: &UnsignedEvent,
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        if rumor.kind.as_u16() != GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16 {
            return Ok(output);
        }

        let tag_group_id = Self::tag_value(&rumor.tags, "l");
        let dist: SenderKeyDistribution = serde_json::from_str(&rumor.content)?;

        if let Some(ref gid) = tag_group_id {
            if dist.group_id != *gid {
                return Ok(output);
            }
        }

        let Some(sender_event_hex) = dist.sender_event_pubkey.as_deref() else {
            return Ok(output);
        };
        let Ok(sender_event_pubkey) = crate::utils::pubkey_from_hex(sender_event_hex) else {
            return Ok(output);
        };

        let info = GroupSenderEventInfo {
            group_id: dist.group_id.clone(),
            sender_owner_pubkey: from_owner_pubkey,
            // Sender device must come from authenticated session context.
            // Never trust inner rumor `pubkey` for identity attribution.
            sender_device_pubkey: from_sender_device_pubkey,
        };
        output.append_unit(self.store_group_sender_event_info(sender_event_pubkey, &info)?);
        output.append_unit(self.ensure_sender_key_state_from_distribution(sender_event_pubkey, &dist)?);

        // Decrypt any queued outer events that were waiting for this sender key id.
        let pending = {
            let mut map = self.group_sender_key_pending.lock().unwrap();
            map.remove(&(sender_event_pubkey, dist.key_id))
                .unwrap_or_default()
        };
        if pending.is_empty() {
            return Ok(output);
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
            if let Some((sender, sender_device, plaintext, event_id, storage_effects)) =
                self.try_decrypt_group_sender_key_outer(&outer, Some(info.clone()))
            {
                output.storage_effects.extend(storage_effects);
                output.push_notification(SessionManagerNotification::DecryptedMessage {
                    sender,
                    sender_device,
                    content: plaintext,
                    event_id,
                });
            }
        }

        Ok(output)
    }

    fn try_decrypt_group_sender_key_outer(
        &self,
        outer: &nostr::Event,
        info_hint: Option<GroupSenderEventInfo>,
    ) -> Option<(
        PublicKey,
        Option<PublicKey>,
        String,
        Option<String>,
        Vec<SessionManagerStorageEffect>,
    )> {
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
        let storage_effects = self
            .store_sender_key_state(&sender_event_pubkey, key_id, &state)
            .ok()
            .map(|output| output.storage_effects)
            .unwrap_or_default();

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
            storage_effects,
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

    fn update_delegate_mapping(
        &self,
        owner_pubkey: PublicKey,
        app_keys: &AppKeys,
    ) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
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
                if let Ok(removed_entries) = self.message_queue.remove_for_target(identity_hex) {
                    for entry in removed_entries {
                        output.push_storage_effect(Self::queue_delete_effect(
                            &self.message_queue,
                            &entry.id,
                        ));
                    }
                }
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

        if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
            output.append_unit(store_output);
        }
        output
    }

    fn is_device_authorized(&self, owner_pubkey: PublicKey, device_pubkey: PublicKey) -> bool {
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

    fn queue_pending_invite_response(&self, event: nostr::Event) {
        let mut pending = self.pending_invite_responses.lock().unwrap();
        if pending.iter().any(|existing| existing.id == event.id) {
            return;
        }
        pending.push_back(event);
        if pending.len() > MAX_PENDING_INVITE_RESPONSES {
            pending.pop_front();
        }
    }

    fn install_invite_response_session(
        &self,
        event_id: String,
        session: Session,
        meta: crate::InviteResponseMeta,
    ) -> ManagerOutput<bool> {
        let mut output = ManagerOutput::empty(false);
        if meta.invitee_identity == self.our_public_key {
            return output;
        }

        let owner_pubkey = meta
            .owner_public_key
            .unwrap_or_else(|| self.resolve_to_owner(&meta.invitee_identity));

        if !self.is_device_authorized(owner_pubkey, meta.invitee_identity) {
            return output;
        }

        output.append_unit(self.record_known_device_identity(owner_pubkey, meta.invitee_identity));

        let device_id = meta
            .device_id
            .unwrap_or_else(|| hex::encode(meta.invitee_identity.to_bytes()));

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

        output.append_unit(self.refresh_session_subscriptions());
        if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
            output.append_unit(store_output);
        }
        output.append_unit(self.send_message_history(owner_pubkey, &device_id));
        if let Ok(flush_output) = self.flush_message_queue(&device_id) {
            output.append_unit(flush_output);
        }

        self.processed_invite_responses
            .lock()
            .unwrap()
            .insert(event_id.clone());

        self.pending_invite_responses
            .lock()
            .unwrap()
            .retain(|event| event.id.to_string() != event_id);

        output.value = true;
        output
    }

    fn retry_pending_invite_responses(&self, owner_pubkey: PublicKey) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let Some((invite, our_identity_key)) = self
            .invite_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|state| (state.invite.clone(), state.our_identity_key))
        else {
            return output;
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

            let inviter_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
            let InviteProcessResponseResult::Accepted { session, meta, .. } = invite
                .process_response(InviteProcessResponseInput {
                    event: event.clone(),
                    inviter_identity_private_key: our_identity_key,
                    inviter_next_nostr_private_key,
                })
            else {
                continue;
            };

            let resolved_owner = meta
                .owner_public_key
                .unwrap_or_else(|| self.resolve_to_owner(&meta.invitee_identity));
            if resolved_owner != owner_pubkey {
                continue;
            }

            let installed_output =
                self.install_invite_response_session(event.id.to_string(), session, meta);
            let installed = installed_output.value;
            output.effects.extend(installed_output.effects);
            output.notifications.extend(installed_output.notifications);
            eprintln!(
                "[sm] retry_pending_invite_response event={} owner={} installed={}",
                event.id,
                owner_pubkey.to_hex(),
                installed
            );
        }

        output
    }

    fn subscribe_to_app_keys(&self, owner_pubkey: PublicKey) -> ManagerOutput<()> {
        let mut subs = self.app_keys_subscriptions.lock().unwrap();
        if subs.contains(&owner_pubkey) {
            return ManagerOutput::empty(());
        }
        subs.insert(owner_pubkey);
        drop(subs);

        let filter = nostr::Filter::new()
            .kind(nostr::Kind::Custom(crate::APP_KEYS_EVENT_KIND as u16))
            .authors(vec![owner_pubkey]);
        if let Ok(filter_json) = serde_json::to_string(&filter) {
            let subid = format!("app-keys-{}", uuid::Uuid::new_v4());
            return ManagerOutput::with_effects(
                (),
                vec![SessionManagerEffect::Subscribe { subid, filter_json }],
            );
        }

        ManagerOutput::empty(())
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let owner_pubkey = self.resolve_to_owner(&user_pubkey);

        let known_identities = self.with_user_records(move |records| {
            records
                .entry(owner_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())))
                .known_device_identities
                .clone()
        });

        output.append_unit(self.subscribe_to_app_keys(owner_pubkey));

        for identity_hex in known_identities {
            if let Ok(pk) = crate::utils::pubkey_from_hex(&identity_hex) {
                output.append_unit(self.subscribe_to_device_invite(owner_pubkey, pk));
            }
        }

        output
    }

    fn subscribe_to_device_invite(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
    ) -> ManagerOutput<()> {
        let mut subs = self.invite_subscriptions.lock().unwrap();
        if subs.contains(&device_pubkey) {
            return ManagerOutput::empty(());
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
            return ManagerOutput::empty(());
        }

        self.subscribe_to_user_invites(device_pubkey)
            .map(|output| output.map(|_| ()))
            .unwrap_or_else(|_| ManagerOutput::empty(()))
    }

    fn upsert_device_record(record: &mut UserRecord, device_id: &str) {
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

    fn record_known_device_identity(
        &self,
        owner_pubkey: PublicKey,
        device_pubkey: PublicKey,
    ) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
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
            if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
                output.append_unit(store_output);
            }
        }
        output
    }

    fn flush_message_queue(&self, device_identity: &str) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        let entries = self.message_queue.get_for_target(device_identity)?;
        if entries.is_empty() {
            return Ok(output);
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
            return Ok(output);
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
                let Some(session) = device_record.active_session.as_mut() else {
                    return Vec::new();
                };

                let mut pending = Vec::new();
                for entry in entries {
                    let maybe_event_id = entry.event.id.as_ref().map(|id| id.to_string());
                    if let Ok(signed_event) =
                        SessionManager::send_with_session(session, entry.event)
                    {
                        pending.push((entry.id, maybe_event_id, signed_event));
                    }
                }
                pending
            }
        });

        for (entry_id, maybe_event_id, signed_event) in pending_publishes {
            output.push_effect(SessionManagerEffect::PublishSigned(signed_event));
            sent.push((entry_id, maybe_event_id));
        }

        let any_sent = !sent.is_empty();
        for (entry_id, maybe_event_id) in sent {
            if let Some(event_id) = maybe_event_id {
                if let Ok(Some(removed)) = self
                    .message_queue
                    .remove_by_target_and_event_id(device_identity, &event_id)
                {
                    output.push_storage_effect(Self::queue_delete_effect(
                        &self.message_queue,
                        &removed.id,
                    ));
                }
            } else {
                if let Ok(Some(removed)) = self.message_queue.remove(&entry_id) {
                    output.push_storage_effect(Self::queue_delete_effect(
                        &self.message_queue,
                        &removed.id,
                    ));
                }
            }
        }

        if any_sent {
            output.append_unit(self.refresh_session_subscriptions());
        }
        output.append_unit(self.store_user_record(&owner_pubkey)?);
        Ok(output)
    }

    fn expand_discovery_queue(
        &self,
        owner_pubkey: PublicKey,
        devices: &[DeviceEntry],
    ) -> Result<ManagerOutput<()>> {
        let mut output = ManagerOutput::empty(());
        let entries = self
            .discovery_queue
            .get_for_target(&owner_pubkey.to_hex())?;
        if entries.is_empty() {
            return Ok(output);
        }

        for entry in entries {
            let mut expanded_for_all_devices = true;
            for device in devices {
                let device_id = device.identity_pubkey.to_hex();
                if device_id == self.device_id {
                    continue;
                }
                if let Ok(queued_entry) = self
                    .message_queue
                    .add(&device_id, &entry.event, entry.created_at)
                {
                    if let Ok(effect) = Self::queue_put_effect(&self.message_queue, &queued_entry) {
                        output.push_storage_effect(effect);
                    }
                } else {
                    expanded_for_all_devices = false;
                }
            }

            // Keep discovery entry when any per-device queue write fails so the next
            // AppKeys cycle can retry expansion without losing pending messages.
            if expanded_for_all_devices {
                if let Ok(Some(removed)) = self.discovery_queue.remove(&entry.id) {
                    output.push_storage_effect(Self::queue_delete_effect(
                        &self.discovery_queue,
                        &removed.id,
                    ));
                }
            }
        }

        Ok(output)
    }

    fn send_message_history(&self, owner_pubkey: PublicKey, device_id: &str) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let history = {
            self.message_history
                .lock()
                .unwrap()
                .get(&owner_pubkey)
                .cloned()
                .unwrap_or_default()
        };

        if history.is_empty() {
            return output;
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
                let Some(session) = device_record.active_session.as_mut() else {
                    return Vec::new();
                };

                history
                    .into_iter()
                    .filter_map(|event| SessionManager::send_with_session(session, event).ok())
                    .collect::<Vec<_>>()
            }
        });

        let any_signed_history = !signed_history.is_empty();
        for signed_event in signed_history {
            output.push_effect(SessionManagerEffect::PublishSigned(signed_event));
        }
        if any_signed_history {
            output.append_unit(self.refresh_session_subscriptions());
        }
        if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
            output.append_unit(store_output);
        }
        output
    }

    fn build_bootstrap_messages(&self, owner_pubkey: PublicKey) -> Vec<UnsignedEvent> {
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

    fn sign_bootstrap_schedule(
        session: &mut Session,
        bootstrap_messages: &[UnsignedEvent],
    ) -> Vec<nostr::Event> {
        let mut bootstrap_events = Vec::new();
        for bootstrap in bootstrap_messages {
            let Ok(signed_bootstrap) =
                SessionManager::send_with_session(session, bootstrap.clone())
            else {
                break;
            };
            bootstrap_events.push(signed_bootstrap);
        }

        bootstrap_events
    }

    fn publish_bootstrap_schedule(&self, bootstrap_events: Vec<nostr::Event>) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let Some((initial_event, retry_events)) = bootstrap_events.split_first() else {
            return output;
        };

        output.push_effect(SessionManagerEffect::PublishSigned(initial_event.clone()));

        if retry_events.is_empty() {
            return output;
        }

        let scheduled_retries: Vec<(u64, nostr::Event)> = retry_events
            .iter()
            .cloned()
            .zip(INVITE_BOOTSTRAP_RETRY_DELAYS_MS.iter().copied().skip(1))
            .map(|(event, delay_ms)| (delay_ms, event))
            .collect();
        for (delay_ms, event) in scheduled_retries {
            output.push_effect(SessionManagerEffect::SchedulePublishSigned { delay_ms, event });
        }
        output
    }

    fn send_link_bootstrap(&self, owner_pubkey: PublicKey, device_id: &str) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
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
                let Some(session) = device_record.active_session.as_mut() else {
                    return Vec::new();
                };
                SessionManager::sign_bootstrap_schedule(session, &bootstrap_messages)
            }
        });

        if !bootstrap_events.is_empty() {
            output.append_unit(self.refresh_session_subscriptions());
            output.append_unit(self.publish_bootstrap_schedule(bootstrap_events));
            if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
                output.append_unit(store_output);
            }
        }
        output
    }

    fn cleanup_device(&self, owner_pubkey: PublicKey, device_id: &str) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        let removed = self.with_user_records({
            let device_id = device_id.to_string();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return false;
                };

                user_record.device_records.remove(&device_id).is_some()
            }
        });
        if !removed {
            return output;
        }

        output.append_unit(self.refresh_session_subscriptions());

        if let Ok(device_pk) = crate::utils::pubkey_from_hex(device_id) {
            self.delegate_to_owner.lock().unwrap().remove(&device_pk);
        }

        if let Ok(removed_entries) = self.message_queue.remove_for_target(device_id) {
            for entry in removed_entries {
                output.push_storage_effect(Self::queue_delete_effect(
                    &self.message_queue,
                    &entry.id,
                ));
            }
        }

        if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
            output.append_unit(store_output);
        }
        output
    }

    fn handle_app_keys_event(
        &self,
        owner_pubkey: PublicKey,
        app_keys: AppKeys,
        created_at: u64,
    ) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
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
                return output;
            }
            latest.insert(owner_pubkey, applied.created_at);
            applied.app_keys
        };

        output.append_unit(self.update_delegate_mapping(owner_pubkey, &effective_app_keys));

        let devices = effective_app_keys.get_all_devices();
        if let Ok(expand_output) = self.expand_discovery_queue(owner_pubkey, &devices) {
            output.append_unit(expand_output);
        }
        let active_ids: HashSet<String> = devices
            .iter()
            .map(|d| hex::encode(d.identity_pubkey.to_bytes()))
            .collect();

        // Cleanup revoked devices
        let existing_devices = self.with_user_records(move |records| {
            records
                .get(&owner_pubkey)
                .map(|r| r.device_records.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        });

        for device_id in existing_devices {
            if !active_ids.contains(&device_id) {
                output.append_unit(self.cleanup_device(owner_pubkey, &device_id));
                self.invite_subscriptions
                    .lock()
                    .unwrap()
                    .retain(|pk| hex::encode(pk.to_bytes()) != device_id);
            }
        }

        for device in &devices {
            output.append_unit(self.subscribe_to_device_invite(owner_pubkey, device.identity_pubkey));
        }

        output.append_unit(self.retry_pending_invite_responses(owner_pubkey));

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
                if let Ok(flush_output) = self.flush_message_queue(&device_id) {
                    output.append_unit(flush_output);
                }
            }
        }

        output
    }

    fn store_user_record(&self, pubkey: &PublicKey) -> Result<ManagerOutput<()>> {
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
            let json = serde_json::to_string(&stored)?;
            return Ok(ManagerOutput {
                value: (),
                effects: Vec::new(),
                storage_effects: vec![SessionManagerStorageEffect::Put { key, value: json }],
                notifications: Vec::new(),
            });
        }
        Ok(ManagerOutput::empty(()))
    }

    fn load_all_user_records_from_results(
        &self,
        reads: &SessionManagerStorageResults,
    ) -> Result<()> {
        let prefix = self.user_record_key_prefix();
        let keys = reads.lists.get(&prefix).cloned().unwrap_or_default();
        let mut loaded_records = Vec::new();

        for key in keys {
            let Some(Some(data)) = reads.gets.get(&key) else {
                continue;
            };

            let stored: crate::StoredUserRecord = match serde_json::from_str(data) {
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
                    device_record.active_session = Some(Session::new(state));
                }

                for state in device.inactive_sessions {
                    device_record.inactive_sessions.push(Session::new(state));
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

    fn load_queue_from_results(
        &self,
        queue: &MessageQueue,
        reads: &SessionManagerStorageResults,
        prefix: &str,
    ) {
        let mut entries = Vec::new();
        for key in reads.lists.get(prefix).cloned().unwrap_or_default() {
            let Some(Some(data)) = reads.gets.get(&key) else {
                continue;
            };
            let Ok(entry) = serde_json::from_str::<crate::QueueEntry>(data) else {
                continue;
            };
            entries.push(entry);
        }
        queue.import_entries(entries);
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

    pub fn process_received_event(&self, event: nostr::Event) -> ManagerOutput<()> {
        let mut output = ManagerOutput::empty(());
        if is_app_keys_event(&event) {
            if let Ok(app_keys) = AppKeys::from_event(&event) {
                output.append_unit(self.handle_app_keys_event(
                    event.pubkey,
                    app_keys,
                    event.created_at.as_u64(),
                ));
            }
            return output;
        }

        if event.kind.as_u16() == crate::INVITE_RESPONSE_KIND as u16 {
            if self
                .processed_invite_responses
                .lock()
                .unwrap()
                .contains(&event.id.to_string())
            {
                return output;
            }

            if let Some(state) = self.invite_state.lock().unwrap().as_ref() {
                let inviter_next_nostr_private_key =
                    Keys::generate().secret_key().to_secret_bytes();
                match state.invite.process_response(InviteProcessResponseInput {
                    event: event.clone(),
                    inviter_identity_private_key: state.our_identity_key,
                    inviter_next_nostr_private_key,
                }) {
                    InviteProcessResponseResult::Accepted { session, meta, .. } => {
                        let install_output = self.install_invite_response_session(
                            event.id.to_string(),
                            session,
                            meta,
                        );
                        if !install_output.value {
                            self.queue_pending_invite_response(event.clone());
                        }
                        output.effects.extend(install_output.effects);
                        output.notifications.extend(install_output.notifications);
                    }
                    InviteProcessResponseResult::NotForThisInvite { .. } => {}
                    InviteProcessResponseResult::InvalidRelevant { .. } => {}
                }
            }
            return output;
        }

        if event.kind.as_u16() == crate::INVITE_EVENT_KIND as u16 {
            if let Ok(invite) = Invite::from_event(&event) {
                if let Ok(accept_output) = self.accept_invite(&invite, None) {
                    output.append_unit(accept_output.map(|_| ()));
                }
            }
            return output;
        }

        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16 {
            let event_id = Some(event.id.to_string());
            let decrypted = self.with_user_records({
                let event = event.clone();
                move |records| {
                    for (owner_pubkey, user_record) in records.iter_mut() {
                        let device_ids: Vec<String> =
                            user_record.device_records.keys().cloned().collect();

                        for device_id in device_ids {
                            let Some(device_record) =
                                user_record.device_records.get_mut(&device_id)
                            else {
                                continue;
                            };

                            if let Some(ref mut session) = device_record.active_session {
                                if let Ok(Some(plaintext)) =
                                    SessionManager::receive_with_session(session, &event)
                                {
                                    return Some((*owner_pubkey, plaintext, device_id.clone()));
                                }
                            }

                            for idx in 0..device_record.inactive_sessions.len() {
                                let plaintext_opt = {
                                    let session = &mut device_record.inactive_sessions[idx];
                                    SessionManager::receive_with_session(session, &event)
                                        .ok()
                                        .flatten()
                                };

                                if let Some(plaintext) = plaintext_opt {
                                    SessionManager::promote_session_to_active(
                                        user_record,
                                        &device_id,
                                        idx,
                                    );
                                    return Some((*owner_pubkey, plaintext, device_id.clone()));
                                }
                            }
                        }
                    }

                    None
                }
            });

            if let Some((owner_pubkey, plaintext, device_id)) = decrypted {
                output.append_unit(self.refresh_session_subscriptions());
                let sender_device = if let Ok(sender_pk) = crate::utils::pubkey_from_hex(&device_id)
                {
                    let sender_owner = self.resolve_to_owner(&sender_pk);
                    if sender_owner != sender_pk
                        && !self.is_device_authorized(sender_owner, sender_pk)
                    {
                        return output;
                    }
                    Some(sender_pk)
                } else {
                    None
                };

                if let Ok(rumor) = serde_json::from_str::<UnsignedEvent>(&plaintext) {
                    output.append_unit(self.maybe_auto_adopt_chat_settings(owner_pubkey, &rumor));
                    if let Ok(group_output) = self.maybe_handle_group_sender_key_distribution(
                        owner_pubkey,
                        sender_device,
                        &rumor,
                    ) {
                        output.append_unit(group_output);
                    }
                }

                if let Ok(store_output) = self.store_user_record(&owner_pubkey) {
                    output.append_unit(store_output);
                }
                output.push_notification(SessionManagerNotification::DecryptedMessage {
                    sender: owner_pubkey,
                    sender_device,
                    content: plaintext,
                    event_id,
                });
                if let Ok(flush_output) = self.flush_message_queue(&device_id) {
                    output.append_unit(flush_output);
                }
            } else if let Some((sender, sender_device, plaintext, event_id, storage_effects)) =
                self.try_decrypt_group_sender_key_outer(&event, None)
            {
                output.storage_effects.extend(storage_effects);
                output.push_notification(SessionManagerNotification::DecryptedMessage {
                    sender,
                    sender_device,
                    content: plaintext,
                    event_id,
                });
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStorage;
    use nostr::Keys;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FailFirstMessageQueuePutStorage {
        inner: Arc<dyn StorageAdapter>,
        failed: Arc<Mutex<bool>>,
    }

    impl FailFirstMessageQueuePutStorage {
        fn new(inner: Arc<dyn StorageAdapter>) -> Self {
            Self {
                inner,
                failed: Arc::new(Mutex::new(false)),
            }
        }
    }

    impl StorageAdapter for FailFirstMessageQueuePutStorage {
        fn get(&self, key: &str) -> Result<Option<String>> {
            self.inner.get(key)
        }

        fn put(&self, key: &str, value: String) -> Result<()> {
            if key.starts_with("v1/message-queue/") {
                let mut failed = self.failed.lock().unwrap();
                if !*failed {
                    *failed = true;
                    return Err(crate::Error::Storage(
                        "injected message-queue put failure".to_string(),
                    ));
                }
            }
            self.inner.put(key, value)
        }

        fn del(&self, key: &str) -> Result<()> {
            self.inner.del(key)
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            self.inner.list(prefix)
        }
    }

    fn count_queue_entries(
        storage: &Arc<dyn StorageAdapter>,
        prefix: &str,
        target_key: &str,
        event_id: &str,
    ) -> usize {
        let mut count = 0usize;
        let keys = storage.list(prefix).unwrap();
        for key in keys {
            let Some(raw) = storage.get(&key).unwrap() else {
                continue;
            };
            let Ok(entry) = serde_json::from_str::<crate::QueueEntry>(&raw) else {
                continue;
            };
            if entry.target_key == target_key
                && entry.event.id.as_ref().map(|id| id.to_string()) == Some(event_id.to_string())
            {
                count += 1;
            }
        }
        count
    }

    fn drain_events(
        rx: &crossbeam_channel::Receiver<SessionManagerEvent>,
    ) -> Vec<SessionManagerEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn emit<T>(
        tx: &ManagedTx,
        output: ManagerOutput<T>,
    ) -> T {
        persist_and_emit_session_manager_output(tx.storage.as_ref(), &tx.tx, output).unwrap()
    }

    fn try_emit<T>(tx: &ManagedTx, output: ManagerOutput<T>) -> Result<T> {
        persist_and_emit_session_manager_output(tx.storage.as_ref(), &tx.tx, output)
    }

    struct ManagedTx {
        tx: crossbeam_channel::Sender<SessionManagerEvent>,
        storage: Arc<dyn StorageAdapter>,
    }

    impl std::ops::Deref for ManagedTx {
        type Target = crossbeam_channel::Sender<SessionManagerEvent>;

        fn deref(&self) -> &Self::Target {
            &self.tx
        }
    }

    fn new_manager(
        our_pubkey: PublicKey,
        identity_key: [u8; 32],
        device_id: String,
        owner_pubkey: PublicKey,
        storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> (
        SessionManager,
        ManagedTx,
        crossbeam_channel::Receiver<SessionManagerEvent>,
    ) {
        let storage = storage.unwrap_or_else(|| Arc::new(crate::InMemoryStorage::new()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let manager = SessionManager::new(
            our_pubkey,
            identity_key,
            device_id,
            owner_pubkey,
            Some(storage.clone()),
            invite,
        );
        (manager, ManagedTx { tx, storage }, rx)
    }

    fn sign_app_keys_event_with_created_at(
        app_keys: &AppKeys,
        owner_pubkey: PublicKey,
        owner_keys: &Keys,
        created_at: u64,
    ) -> nostr::Event {
        let mut tags = Vec::new();
        tags.push(
            nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()]).unwrap(),
        );
        tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
        for device in app_keys.get_all_devices() {
            tags.push(
                nostr::Tag::parse(&[
                    "device".to_string(),
                    hex::encode(device.identity_pubkey.to_bytes()),
                    device.created_at.to_string(),
                ])
                .unwrap(),
            );
        }

        nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
            .tags(tags)
            .custom_created_at(nostr::Timestamp::from(created_at))
            .build(owner_pubkey)
            .sign_with_keys(owner_keys)
            .unwrap()
    }

    fn test_session_state() -> crate::SessionState {
        let their_current = Keys::generate().public_key();
        let their_next = Keys::generate().public_key();
        let our_current = Keys::generate();
        let our_next = Keys::generate();

        crate::SessionState {
            root_key: [0u8; 32],
            their_current_nostr_public_key: Some(their_current),
            their_next_nostr_public_key: Some(their_next),
            our_current_nostr_key: Some(crate::SerializableKeyPair {
                public_key: our_current.public_key(),
                private_key: our_current.secret_key().to_secret_bytes(),
            }),
            our_next_nostr_key: crate::SerializableKeyPair {
                public_key: our_next.public_key(),
                private_key: our_next.secret_key().to_secret_bytes(),
            },
            receiving_chain_key: Some([1u8; 32]),
            sending_chain_key: Some([2u8; 32]),
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: HashMap::new(),
        }
    }

    #[test]
    fn test_session_manager_new() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (manager, _tx, _rx) =
            new_manager(pubkey, identity_key, device_id.clone(), pubkey, None, None);

        assert_eq!(manager.get_device_id(), device_id);
    }

    #[test]
    fn test_send_text_no_sessions() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (manager, _tx, _rx) = new_manager(pubkey, identity_key, device_id, pubkey, None, None);

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

        let (manager, _tx, _rx) = new_manager(pubkey, identity_key, device_id, pubkey, None, None);

        let recipient = Keys::generate().public_key();
        manager.send_typing(recipient, None).unwrap();

        let history = manager.message_history.lock().unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn init_compacts_duplicate_stored_sessions_and_only_subscribes_once_per_filter() {
        let our_keys = Keys::generate();
        let peer = Keys::generate().public_key();
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

        let state = test_session_state();
        let stored = crate::StoredUserRecord {
            user_id: hex::encode(peer.to_bytes()),
            devices: vec![crate::StoredDeviceRecord {
                device_id: "peer-device".to_string(),
                active_session: Some(state.clone()),
                inactive_sessions: vec![state.clone(), state],
                created_at: 1,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(1),
            }],
            known_device_identities: Vec::new(),
        };

        storage
            .put(
                &format!("user/{}", hex::encode(peer.to_bytes())),
                serde_json::to_string(&stored).unwrap(),
            )
            .unwrap();

        let (manager, tx, rx) = new_manager(
            our_keys.public_key(),
            our_keys.secret_key().to_secret_bytes(),
            "test-device".to_string(),
            our_keys.public_key(),
            Some(storage),
            None,
        );

        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());

        let subscribe_count = drain_events(&rx)
            .into_iter()
            .filter(|event| {
                matches!(event, SessionManagerEvent::Subscribe { subid, .. } if subid.starts_with("session-"))
            })
            .count();
        assert_eq!(subscribe_count, 2);

        let (active_count, inactive_count) = manager.with_user_records({
            let peer = peer;
            move |records| {
                let device_record = records
                    .get(&peer)
                    .and_then(|record| record.device_records.get("peer-device"))
                    .unwrap();
                (
                    usize::from(device_record.active_session.is_some()),
                    device_record.inactive_sessions.len(),
                )
            }
        });
        assert_eq!(active_count, 1);
        assert_eq!(inactive_count, 0);
    }

    #[test]
    fn test_delete_chat_removes_local_state_and_allows_reinit() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();

        let (manager, tx, _rx) = new_manager(pubkey, identity_key, device_id, pubkey, None, None);
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());

        let peer = Keys::generate().public_key();
        emit(&tx, manager.setup_user(peer));
        assert!(manager.get_user_pubkeys().contains(&peer));

        emit(&tx, manager.delete_chat(peer).unwrap());
        assert!(!manager.get_user_pubkeys().contains(&peer));

        emit(&tx, manager.send_text(peer, "reinit".to_string(), None).unwrap());
        assert!(manager.get_user_pubkeys().contains(&peer));
    }

    #[test]
    fn group_sender_key_distribution_allows_decrypting_one_to_many_outer_messages() {
        let our_keys = Keys::generate();
        let our_pubkey = our_keys.public_key();
        let identity_key = our_keys.secret_key().to_secret_bytes();

        let storage = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            our_pubkey,
            identity_key,
            "test-device".to_string(),
            our_pubkey,
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

        emit(
            &tx,
            manager
                .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist_rumor,
            )
                .unwrap(),
        );

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

        emit(&tx, manager.process_received_event(outer.clone()));

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
        let (manager, tx, rx) = new_manager(
            our_pubkey,
            identity_key,
            "test-device".to_string(),
            our_pubkey,
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
            serde_json::to_string(&dist1).unwrap(),
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(1))
        .build(sender_device_pubkey);
        emit(
            &tx,
            manager
                .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist1_rumor,
            )
                .unwrap(),
        );
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

        emit(&tx, manager.process_received_event(outer.clone()));
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
            serde_json::to_string(&dist2).unwrap(),
        )
        .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(2))
        .build(sender_device_pubkey);
        emit(
            &tx,
            manager
                .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist2_rumor,
            )
                .unwrap(),
        );

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
            let (manager, tx, _rx) = new_manager(
                our_pubkey,
                our_keys.secret_key().to_secret_bytes(),
                "test-device".to_string(),
                our_pubkey,
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
                serde_json::to_string(&dist).unwrap(),
            )
            .tag(Tag::parse(&["l".to_string(), dist.group_id.clone()]).unwrap())
            .custom_created_at(nostr::Timestamp::from(1))
            .build(sender_device_pubkey);

            emit(
                &tx,
                manager
                    .maybe_handle_group_sender_key_distribution(
                    sender_owner_pubkey,
                    Some(sender_device_pubkey),
                    &dist_rumor,
                )
                    .unwrap(),
            );
        }

        let (manager, tx, rx) = new_manager(
            our_pubkey,
            our_keys.secret_key().to_secret_bytes(),
            "test-device".to_string(),
            our_pubkey,
            Some(storage),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());

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

        let (manager1, tx1, _rx1) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx1, initialize_session_manager(tx1.storage.as_ref(), &manager1).unwrap());

        let (inner_id, published_ids) = emit(
            &tx1,
            manager1
                .send_text_with_inner_id(bob_pubkey, "queued before restart".to_string(), None)
                .unwrap(),
        );
        assert!(published_ids.is_empty());
        assert!(
            !storage.list("v1/discovery-queue/").unwrap().is_empty(),
            "expected discovery queue entries when recipient devices are unknown"
        );

        drop(manager1);

        let (manager2, tx2, rx2) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx2, initialize_session_manager(tx2.storage.as_ref(), &manager2).unwrap());
        let _ = drain_events(&rx2);

        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
        let app_keys_event = app_keys
            .get_event(bob_pubkey)
            .sign_with_keys(&bob_keys)
            .unwrap();
        emit(&tx2, manager2.process_received_event(app_keys_event));

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
        emit(&tx2, manager2.process_received_event(invite_event));

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
    fn queued_message_for_known_appkeys_device_flushes_without_new_appkeys_event() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_keys = Keys::generate();
        let bob_pubkey = bob_keys.public_key();
        let bob_device_id = bob_pubkey.to_hex();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        // Learn recipient devices first (AppKeys known) but don't establish a session yet.
        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
        let app_keys_event = app_keys
            .get_event(bob_pubkey)
            .sign_with_keys(&bob_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(app_keys_event));
        let _ = drain_events(&rx);

        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(bob_pubkey, "queued with known appkeys".to_string(), None)
                .unwrap(),
        );
        assert!(
            published_ids.is_empty(),
            "without an active session, send should queue for later"
        );

        // TS parity: this should be queued directly per known device.
        let queued_keys = storage.list("v1/message-queue/").unwrap();
        assert!(
            queued_keys
                .iter()
                .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
            "expected recipient message queue entry when AppKeys are already known"
        );

        // Ensure we did not put this recipient message back into discovery queue.
        let mut bob_discovery_count = 0usize;
        for key in storage.list("v1/discovery-queue/").unwrap() {
            let Some(raw) = storage.get(&key).unwrap() else {
                continue;
            };
            let Ok(entry) = serde_json::from_str::<crate::QueueEntry>(&raw) else {
                continue;
            };
            if entry.target_key == bob_pubkey.to_hex()
                && entry.event.id.as_ref().map(|id| id.to_string()) == Some(inner_id.clone())
            {
                bob_discovery_count += 1;
            }
        }
        assert_eq!(
            bob_discovery_count, 0,
            "recipient should not rely on discovery queue after AppKeys are known"
        );

        // Accept invite for that known device without sending another AppKeys event.
        let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
        let invite_event = invite
            .get_event()
            .unwrap()
            .sign_with_keys(&bob_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(invite_event));

        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected queued message to flush immediately after session creation"
        );

        let remaining_keys = storage.list("v1/message-queue/").unwrap();
        assert!(
            !remaining_keys
                .iter()
                .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
            "expected recipient queue entry removal after successful publish"
        );
    }

    #[test]
    fn owner_side_link_invite_accepts_new_device_not_yet_in_cached_appkeys() {
        let owner_keys = Keys::generate();
        let owner_pubkey = owner_keys.public_key();
        let known_device_keys = Keys::generate();
        let known_device_pubkey = known_device_keys.public_key();
        let new_device_keys = Keys::generate();
        let new_device_pubkey = new_device_keys.public_key();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            owner_pubkey,
            owner_keys.secret_key().to_secret_bytes(),
            owner_pubkey.to_hex(),
            owner_pubkey,
            Some(storage),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(owner_pubkey, 1));
        app_keys.add_device(DeviceEntry::new(known_device_pubkey, 2));
        let app_keys_event = app_keys
            .get_event(owner_pubkey)
            .sign_with_keys(&owner_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(app_keys_event));

        let mut link_invite =
            Invite::create_new(new_device_pubkey, Some(new_device_pubkey.to_hex()), Some(1))
                .unwrap();
        link_invite.purpose = Some("link".to_string());
        link_invite.owner_public_key = Some(owner_pubkey);

        let accepted = manager.accept_invite(&link_invite, Some(owner_pubkey));
        assert!(
            accepted.is_ok(),
            "owner-side link invite should allow pre-registration acceptance"
        );
        assert!(emit(&tx, accepted.unwrap()).created_new_session);
    }

    #[test]
    fn accept_invite_publishes_bootstrap_message_event() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_keys = Keys::generate();
        let bob_pubkey = bob_keys.public_key();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let invite = Invite::create_new(bob_pubkey, Some(bob_pubkey.to_hex()), Some(1)).unwrap();
        let accepted = emit(&tx, manager.accept_invite(&invite, Some(bob_pubkey)).unwrap());
        assert!(accepted.created_new_session);

        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected a bootstrap message event after invite acceptance"
        );
    }

    #[test]
    fn accept_invite_retries_bootstrap_message_event_with_future_expiration() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_keys = Keys::generate();
        let bob_pubkey = bob_keys.public_key();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let invite = Invite::create_new(bob_pubkey, Some(bob_pubkey.to_hex()), Some(1)).unwrap();
        emit(
            &tx,
            manager
                .accept_invite(&invite, Some(bob_pubkey))
                .expect("accept_invite should succeed for single-device peer"),
        );

        let initial_events = drain_events(&rx);
        assert!(
            initial_events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected immediate bootstrap publish"
        );

        std::thread::sleep(Duration::from_millis(2_100));
        let retry_events = drain_events(&rx);
        let retry_count = retry_events
            .iter()
            .filter(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            })
            .count();
        assert!(
            retry_count >= 2,
            "expected delayed bootstrap retries after invite acceptance"
        );
    }

    #[test]
    fn owner_side_link_invite_accepts_new_device_not_yet_in_stored_appkeys_after_restart() {
        let owner_keys = Keys::generate();
        let owner_pubkey = owner_keys.public_key();
        let known_device_keys = Keys::generate();
        let known_device_pubkey = known_device_keys.public_key();
        let new_device_keys = Keys::generate();
        let new_device_pubkey = new_device_keys.public_key();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager1, tx1, rx1) = new_manager(
            owner_pubkey,
            owner_keys.secret_key().to_secret_bytes(),
            owner_pubkey.to_hex(),
            owner_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx1, initialize_session_manager(tx1.storage.as_ref(), &manager1).unwrap());
        let _ = drain_events(&rx1);

        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(owner_pubkey, 1));
        app_keys.add_device(DeviceEntry::new(known_device_pubkey, 2));
        let app_keys_event = app_keys
            .get_event(owner_pubkey)
            .sign_with_keys(&owner_keys)
            .unwrap();
        emit(&tx1, manager1.process_received_event(app_keys_event));

        drop(manager1);

        let (manager2, tx2, rx2) = new_manager(
            owner_pubkey,
            owner_keys.secret_key().to_secret_bytes(),
            owner_pubkey.to_hex(),
            owner_pubkey,
            Some(storage),
            None,
        );
        emit(&tx2, initialize_session_manager(tx2.storage.as_ref(), &manager2).unwrap());
        let _ = drain_events(&rx2);

        let mut link_invite =
            Invite::create_new(new_device_pubkey, Some(new_device_pubkey.to_hex()), Some(1))
                .unwrap();
        link_invite.purpose = Some("link".to_string());
        link_invite.owner_public_key = Some(owner_pubkey);

        let accepted = manager2.accept_invite(&link_invite, Some(owner_pubkey));
        assert!(
            accepted.is_ok(),
            "owner-side link invite should allow pre-registration acceptance after restart"
        );
        assert!(emit(&tx2, accepted.unwrap()).created_new_session);
    }

    #[test]
    fn discovery_entry_retained_when_discovery_expansion_partially_fails() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_keys = Keys::generate();
        let bob_pubkey = bob_keys.public_key();
        let bob_device_id = bob_pubkey.to_hex();

        let base_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let flaky_storage: Arc<dyn StorageAdapter> =
            Arc::new(FailFirstMessageQueuePutStorage::new(base_storage.clone()));

        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(flaky_storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(
                    bob_pubkey,
                    "retry after partial discovery expansion".to_string(),
                    None,
                )
                .unwrap(),
        );
        assert!(published_ids.is_empty());

        let discovery_count_before = count_queue_entries(
            &flaky_storage,
            "v1/discovery-queue/",
            &bob_pubkey.to_hex(),
            &inner_id,
        );
        assert!(
            discovery_count_before > 0,
            "expected discovery entry before appkeys expansion"
        );

        let mut app_keys = AppKeys::new(vec![]);
        app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
        let app_keys_event = app_keys
            .get_event(bob_pubkey)
            .sign_with_keys(&bob_keys)
            .unwrap();
        let _ = try_emit(&tx, manager.process_received_event(app_keys_event.clone()));

        let discovery_count_after_first = count_queue_entries(
            &flaky_storage,
            "v1/discovery-queue/",
            &bob_pubkey.to_hex(),
            &inner_id,
        );
        assert!(
            discovery_count_after_first > 0,
            "discovery entry should be retained when expansion only partially succeeds"
        );

        // Recreate manager from durable state, then retry after the injected one-time queue
        // failure has been consumed.
        drop(manager);

        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(flaky_storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        emit(&tx, manager.process_received_event(app_keys_event));
        let queued_count_after_retry = count_queue_entries(
            &flaky_storage,
            "v1/message-queue/",
            &bob_device_id,
            &inner_id,
        );
        assert!(
            queued_count_after_retry > 0,
            "expected retry expansion to enqueue message per device"
        );

        let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
        let invite_event = invite
            .get_event()
            .unwrap()
            .sign_with_keys(&bob_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(invite_event));

        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected queued message to publish after retry and session creation"
        );
    }

    #[test]
    fn appkeys_replacement_cleans_revoked_device_queue_entries() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_owner_keys = Keys::generate();
        let bob_owner_pubkey = bob_owner_keys.public_key();
        let bob_device1_keys = Keys::generate();
        let bob_device2_keys = Keys::generate();
        let bob_device1_id = bob_device1_keys.public_key().to_hex();
        let bob_device2_id = bob_device2_keys.public_key().to_hex();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        // Learn two recipient devices first; no sessions yet.
        let mut app_keys_two = AppKeys::new(vec![]);
        app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
        let app_keys_two_event = sign_app_keys_event_with_created_at(
            &app_keys_two,
            bob_owner_pubkey,
            &bob_owner_keys,
            1,
        );
        let _ = try_emit(&tx, manager.process_received_event(app_keys_two_event));
        let _ = drain_events(&rx);

        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(
                    bob_owner_pubkey,
                    "queued for two devices pre-revoke".to_string(),
                    None,
                )
                .unwrap(),
        );
        assert!(
            published_ids.is_empty(),
            "without sessions, message should queue per known device"
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
            1
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
            1
        );

        // Replace AppKeys with only device1 (device2 revoked).
        let mut app_keys_one = AppKeys::new(vec![]);
        app_keys_one.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 3));
        let app_keys_one_event = sign_app_keys_event_with_created_at(
            &app_keys_one,
            bob_owner_pubkey,
            &bob_owner_keys,
            2,
        );
        emit(&tx, manager.process_received_event(app_keys_one_event));

        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
            0,
            "revoked device queue entries should be purged on appkeys replacement"
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
            1,
            "still-authorized device queue entries should remain"
        );

        // Authorized sibling can still establish session and receive flush.
        let invite = Invite::create_new(
            bob_device1_keys.public_key(),
            Some(bob_device1_id.clone()),
            None,
        )
        .unwrap();
        let invite_event = invite
            .get_event()
            .unwrap()
            .sign_with_keys(&bob_device1_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(invite_event));
        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "expected queued message to publish for still-authorized device"
        );
    }

    #[test]
    fn stale_appkeys_replay_does_not_remove_newer_devices() {
        fn sign_app_keys_event(
            app_keys: &AppKeys,
            owner_pubkey: PublicKey,
            owner_keys: &Keys,
            created_at: u64,
        ) -> nostr::Event {
            let mut tags = Vec::new();
            tags.push(
                nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()])
                    .unwrap(),
            );
            tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
            for device in app_keys.get_all_devices() {
                tags.push(
                    nostr::Tag::parse(&[
                        "device".to_string(),
                        hex::encode(device.identity_pubkey.to_bytes()),
                        device.created_at.to_string(),
                    ])
                    .unwrap(),
                );
            }

            nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
                .tags(tags)
                .custom_created_at(nostr::Timestamp::from(created_at))
                .build(owner_pubkey)
                .sign_with_keys(owner_keys)
                .unwrap()
        }

        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_owner_keys = Keys::generate();
        let bob_owner_pubkey = bob_owner_keys.public_key();
        let bob_device1_keys = Keys::generate();
        let bob_device2_keys = Keys::generate();
        let bob_device1_id = bob_device1_keys.public_key().to_hex();
        let bob_device2_id = bob_device2_keys.public_key().to_hex();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let mut app_keys_two = AppKeys::new(vec![]);
        app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
        emit(
            &tx,
            manager.process_received_event(sign_app_keys_event(
                &app_keys_two,
                bob_owner_pubkey,
                &bob_owner_keys,
                2,
            )),
        );
        let _ = drain_events(&rx);

        let mut stale_one_device = AppKeys::new(vec![]);
        stale_one_device.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        emit(
            &tx,
            manager.process_received_event(sign_app_keys_event(
                &stale_one_device,
                bob_owner_pubkey,
                &bob_owner_keys,
                1,
            )),
        );
        let _ = drain_events(&rx);

        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(
                    bob_owner_pubkey,
                    "stale appkeys replay should not collapse fanout".to_string(),
                    None,
                )
                .unwrap(),
        );
        assert!(
            published_ids.is_empty(),
            "without established sessions, message should queue per known device"
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
            1
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
            1,
            "older appkeys replay must not revoke the newer second device"
        );
    }

    #[test]
    fn same_timestamp_appkeys_replay_preserves_known_devices() {
        fn sign_app_keys_event(
            app_keys: &AppKeys,
            owner_pubkey: PublicKey,
            owner_keys: &Keys,
            created_at: u64,
        ) -> nostr::Event {
            let mut tags = Vec::new();
            tags.push(
                nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()])
                    .unwrap(),
            );
            tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
            for device in app_keys.get_all_devices() {
                tags.push(
                    nostr::Tag::parse(&[
                        "device".to_string(),
                        hex::encode(device.identity_pubkey.to_bytes()),
                        device.created_at.to_string(),
                    ])
                    .unwrap(),
                );
            }

            nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
                .tags(tags)
                .custom_created_at(nostr::Timestamp::from(created_at))
                .build(owner_pubkey)
                .sign_with_keys(owner_keys)
                .unwrap()
        }

        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_owner_keys = Keys::generate();
        let bob_owner_pubkey = bob_owner_keys.public_key();
        let bob_device1_keys = Keys::generate();
        let bob_device2_keys = Keys::generate();
        let bob_device1_id = bob_device1_keys.public_key().to_hex();
        let bob_device2_id = bob_device2_keys.public_key().to_hex();

        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let mut app_keys_two = AppKeys::new(vec![]);
        app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
        emit(
            &tx,
            manager.process_received_event(sign_app_keys_event(
                &app_keys_two,
                bob_owner_pubkey,
                &bob_owner_keys,
                5,
            )),
        );
        let _ = drain_events(&rx);

        let mut same_second_subset = AppKeys::new(vec![]);
        same_second_subset.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        emit(
            &tx,
            manager.process_received_event(sign_app_keys_event(
                &same_second_subset,
                bob_owner_pubkey,
                &bob_owner_keys,
                5,
            )),
        );
        let _ = drain_events(&rx);

        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(
                    bob_owner_pubkey,
                    "same-second replay should not collapse fanout".to_string(),
                    None,
                )
                .unwrap(),
        );
        assert!(published_ids.is_empty());
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
            1
        );
        assert_eq!(
            count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
            1,
            "same-second appkeys replay should preserve previously known devices"
        );
    }

    #[test]
    fn transient_expansion_failure_then_revocation_keeps_only_authorized_retry_path() {
        let alice_keys = Keys::generate();
        let alice_pubkey = alice_keys.public_key();
        let bob_owner_keys = Keys::generate();
        let bob_owner_pubkey = bob_owner_keys.public_key();
        let bob_device1_keys = Keys::generate();
        let bob_device2_keys = Keys::generate();
        let bob_device1_id = bob_device1_keys.public_key().to_hex();
        let bob_device2_id = bob_device2_keys.public_key().to_hex();

        let base_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let flaky_storage: Arc<dyn StorageAdapter> =
            Arc::new(FailFirstMessageQueuePutStorage::new(base_storage.clone()));

        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(flaky_storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        // Queue in discovery first (unknown recipient devices).
        let (inner_id, published_ids) = emit(
            &tx,
            manager
                .send_text_with_inner_id(
                    bob_owner_pubkey,
                    "queued before appkeys/revocation".to_string(),
                    None,
                )
                .unwrap(),
        );
        assert!(published_ids.is_empty());

        // AppKeys with two devices: first expansion attempt will partially fail.
        let mut app_keys_two = AppKeys::new(vec![]);
        app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
        app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
        let app_keys_two_event = sign_app_keys_event_with_created_at(
            &app_keys_two,
            bob_owner_pubkey,
            &bob_owner_keys,
            1,
        );
        let _ = try_emit(&tx, manager.process_received_event(app_keys_two_event));
        assert!(
            count_queue_entries(
                &flaky_storage,
                "v1/discovery-queue/",
                &bob_owner_pubkey.to_hex(),
                &inner_id
            ) > 0,
            "discovery entry should survive partial expansion failure"
        );

        // Revoke device2 by AppKeys replacement. Retry path should keep only device1.
        drop(manager);

        let (manager, tx, rx) = new_manager(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            alice_pubkey.to_hex(),
            alice_pubkey,
            Some(flaky_storage.clone()),
            None,
        );
        emit(&tx, initialize_session_manager(tx.storage.as_ref(), &manager).unwrap());
        let _ = drain_events(&rx);

        let mut app_keys_one = AppKeys::new(vec![]);
        app_keys_one.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 3));
        let app_keys_one_event = sign_app_keys_event_with_created_at(
            &app_keys_one,
            bob_owner_pubkey,
            &bob_owner_keys,
            2,
        );
        emit(&tx, manager.process_received_event(app_keys_one_event.clone()));
        emit(&tx, manager.process_received_event(app_keys_one_event));

        assert_eq!(
            count_queue_entries(
                &flaky_storage,
                "v1/message-queue/",
                &bob_device2_id,
                &inner_id
            ),
            0,
            "revoked device should not keep retryable queue entries"
        );
        assert!(
            count_queue_entries(
                &flaky_storage,
                "v1/message-queue/",
                &bob_device1_id,
                &inner_id
            ) > 0,
            "authorized sibling should retain retryable queue entry"
        );

        let invite = Invite::create_new(
            bob_device1_keys.public_key(),
            Some(bob_device1_id.clone()),
            None,
        )
        .unwrap();
        let invite_event = invite
            .get_event()
            .unwrap()
            .sign_with_keys(&bob_device1_keys)
            .unwrap();
        emit(&tx, manager.process_received_event(invite_event));
        let events = drain_events(&rx);
        assert!(
            events.iter().any(|ev| {
                matches!(
                    ev,
                    SessionManagerEvent::PublishSigned(event)
                        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
                )
            }),
            "authorized device should receive queued message after retry path"
        );
    }

    #[test]
    fn test_auto_adopt_chat_settings_sender_copy_uses_p_tag_peer() {
        let keys = Keys::generate();
        let pubkey = keys.public_key();
        let identity_key = keys.secret_key().to_secret_bytes();
        let device_id = "test-device".to_string();
        let (manager, _tx, _rx) = new_manager(pubkey, identity_key, device_id, pubkey, None, None);

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
