use crate::{
    apply_app_keys_snapshot, is_app_keys_event, resolve_invite_owner_routing, AppKeys,
    AppKeysSnapshotDecision, DeviceEntry, InMemoryStorage, Invite, MessageQueue, NostrPubSub,
    OneToManyChannel, Result, SenderKeyDistribution, SenderKeyState, StorageAdapter, UserRecord,
    GROUP_SENDER_KEY_DISTRIBUTION_KIND,
};
use nostr::{Keys, PublicKey, Tag, UnsignedEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub enum SessionManagerEvent {
    Subscribe {
        subid: String,
        filter_json: String,
    },
    Unsubscribe(String),
    Publish(UnsignedEvent),
    PublishSigned(nostr::Event), // For events pre-signed with ephemeral keys (kind 1059, 1060)
    PublishSignedForInnerEvent {
        event: nostr::Event,
        inner_event_id: Option<String>,
    },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePushSessionStateSnapshot {
    pub state: crate::SessionState,
    pub tracked_sender_pubkeys: Vec<PublicKey>,
    pub has_receiving_capability: bool,
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
    storage: Arc<dyn StorageAdapter>,
    pubsub: Arc<dyn NostrPubSub>,
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
    fn session_can_receive(session: &crate::Session) -> bool {
        Self::session_state_can_receive(&session.state)
    }

    fn session_send_priority(session: &crate::Session, is_active: bool) -> (u8, u8, u32, u32, u32) {
        let can_send = session.can_send();
        let can_receive = Self::session_can_receive(session);
        let directionality = match (can_send, can_receive) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        };

        (
            directionality,
            u8::from(is_active && can_receive),
            session.state.receiving_chain_message_number,
            session.state.previous_sending_chain_message_count,
            session.state.sending_chain_message_number,
        )
    }

    fn session_state_can_receive(state: &crate::SessionState) -> bool {
        state.receiving_chain_key.is_some()
            || state.their_current_nostr_public_key.is_some()
            || state.receiving_chain_message_number > 0
    }

    fn session_state_tracked_sender_pubkeys(state: &crate::SessionState) -> Vec<PublicKey> {
        let mut pubkeys = HashSet::new();
        if let Some(pubkey) = state.their_current_nostr_public_key {
            pubkeys.insert(pubkey);
        }
        if let Some(pubkey) = state.their_next_nostr_public_key {
            pubkeys.insert(pubkey);
        }

        let mut pubkeys: Vec<PublicKey> = pubkeys.into_iter().collect();
        pubkeys.sort_by_key(|pubkey| pubkey.to_hex());
        pubkeys
    }

    fn message_push_session_snapshots(
        user_record: &UserRecord,
    ) -> Vec<MessagePushSessionStateSnapshot> {
        let mut snapshots = Vec::new();
        let mut seen_states = HashSet::new();

        for device in user_record.device_records.values() {
            for session in device
                .active_session
                .iter()
                .chain(device.inactive_sessions.iter())
            {
                let state = session.state.clone();
                let Ok(state_json) = serde_json::to_string(&state) else {
                    continue;
                };
                if !seen_states.insert(state_json) {
                    continue;
                }

                snapshots.push(MessagePushSessionStateSnapshot {
                    tracked_sender_pubkeys: Self::session_state_tracked_sender_pubkeys(&state),
                    has_receiving_capability: Self::session_state_can_receive(&state),
                    state,
                });
            }
        }

        snapshots
            .sort_by_key(|snapshot| serde_json::to_string(&snapshot.state).unwrap_or_default());
        snapshots
    }

    fn message_push_author_pubkeys_for_records(
        records: &HashMap<PublicKey, UserRecord>,
    ) -> Vec<PublicKey> {
        let mut authors = HashSet::new();
        for user_record in records.values() {
            for snapshot in Self::message_push_session_snapshots(user_record) {
                for author in snapshot.tracked_sender_pubkeys {
                    authors.insert(author);
                }
            }
        }

        let mut authors: Vec<PublicKey> = authors.into_iter().collect();
        authors.sort_by_key(|pubkey| pubkey.to_hex());
        authors
    }

    fn stored_user_record_json(user_record: &UserRecord) -> Result<String> {
        Ok(serde_json::to_string(&user_record.to_stored())?)
    }

    fn with_user_records<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut HashMap<PublicKey, UserRecord>) -> R + Send + 'static,
    ) -> R {
        self.user_records.call(f)
    }
}

mod accept_invite;
mod api;
mod devices;
mod event_processing;
mod group_sender_keys;
mod records;
mod sending;
mod settings_storage;

#[cfg(test)]
#[path = "session_manager/tests.rs"]
mod tests;
