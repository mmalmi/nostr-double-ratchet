use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use nostr::{Event, PublicKey, UnsignedEvent};

use crate::{
    group::GroupData, AcceptInviteResult, AppKeys, GroupDecryptedEvent, GroupManager,
    GroupManagerOptions, GroupOuterSubscriptionPlan, InMemoryStorage, Invite, Result, SendOptions,
    SessionManager, SessionManagerEvent, StorageAdapter, MESSAGE_EVENT_KIND,
};

#[derive(Debug, Clone)]
struct DirectMessageSubscription {
    subid: String,
    authors: Vec<PublicKey>,
}

const DM_SUBSCRIPTION_THROTTLE: Duration = Duration::from_millis(1500);

/// Shared between `NdrRuntime` and the trailing-flush worker thread. Holding
/// the throttle state, current sub, session pubkey query helper, and outbound
/// event channel here lets the timer thread re-run the sync without needing
/// to hold a reference to the full runtime.
struct DirectMessageSubscriptionShared {
    current: Mutex<Option<DirectMessageSubscription>>,
    last_change: Mutex<Option<Instant>>,
    flush_pending: AtomicBool,
}

pub struct NdrRuntime {
    session_manager: Arc<SessionManager>,
    group_manager: Mutex<GroupManager>,
    event_rx: Mutex<Receiver<SessionManagerEvent>>,
    event_tx: Sender<SessionManagerEvent>,
    direct_message_subscription: Arc<DirectMessageSubscriptionShared>,
}

impl NdrRuntime {
    pub fn new(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        let storage = storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new()));
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
        let session_manager = SessionManager::new(
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            event_tx.clone(),
            Some(storage.clone()),
            invite,
        );
        let group_manager = GroupManager::new(GroupManagerOptions {
            our_owner_pubkey: owner_public_key,
            our_device_pubkey: our_public_key,
            storage: Some(storage),
            one_to_many: None,
        });

        Self {
            session_manager: Arc::new(session_manager),
            group_manager: Mutex::new(group_manager),
            event_rx: Mutex::new(event_rx),
            event_tx,
            direct_message_subscription: Arc::new(DirectMessageSubscriptionShared {
                current: Mutex::new(None),
                last_change: Mutex::new(None),
                flush_pending: AtomicBool::new(false),
            }),
        }
    }

    pub fn init(&self) -> Result<()> {
        self.session_manager.init()?;
        self.sync_direct_message_subscriptions()
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) -> Result<()> {
        self.session_manager.init()?;
        self.session_manager.setup_user(user_pubkey);
        self.sync_direct_message_subscriptions()
    }

    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<AcceptInviteResult> {
        self.session_manager.init()?;
        let result = self
            .session_manager
            .accept_invite(invite, owner_pubkey_hint)?;
        self.sync_direct_message_subscriptions()?;
        Ok(result)
    }

    pub fn send_text(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        let ids = self.session_manager.send_text(recipient, text, options)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn send_text_with_inner_id(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<(String, Vec<String>)> {
        let result = self
            .session_manager
            .send_text_with_inner_id(recipient, text, options)?;
        self.sync_direct_message_subscriptions()?;
        Ok(result)
    }

    pub fn send_event(
        &self,
        recipient: PublicKey,
        event: nostr::UnsignedEvent,
    ) -> Result<Vec<String>> {
        let ids = self.session_manager.send_event(recipient, event)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn send_reaction(
        &self,
        recipient: PublicKey,
        message_id: String,
        emoji: String,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        let ids = self
            .session_manager
            .send_reaction(recipient, message_id, emoji, options)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn send_receipt(
        &self,
        recipient: PublicKey,
        receipt_type: &str,
        message_ids: Vec<String>,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        let ids =
            self.session_manager
                .send_receipt(recipient, receipt_type, message_ids, options)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn send_typing(
        &self,
        recipient: PublicKey,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        let ids = self.session_manager.send_typing(recipient, options)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn send_chat_settings(
        &self,
        recipient: PublicKey,
        ttl_seconds: u64,
    ) -> Result<Vec<String>> {
        let ids = self
            .session_manager
            .send_chat_settings(recipient, ttl_seconds)?;
        self.sync_direct_message_subscriptions()?;
        Ok(ids)
    }

    pub fn import_session_state(
        &self,
        peer_pubkey: PublicKey,
        device_id: Option<String>,
        state: crate::SessionState,
    ) -> Result<()> {
        self.session_manager
            .import_session_state(peer_pubkey, device_id, state)?;
        self.sync_direct_message_subscriptions()
    }

    pub fn export_active_sessions(&self) -> Vec<(PublicKey, String, crate::SessionState)> {
        self.session_manager.export_active_sessions()
    }

    pub fn ingest_app_keys_snapshot(
        &self,
        owner_pubkey: PublicKey,
        app_keys: AppKeys,
        created_at: u64,
    ) {
        self.session_manager
            .ingest_app_keys_snapshot(owner_pubkey, app_keys, created_at);
        let _ = self.sync_direct_message_subscriptions();
    }

    pub fn process_received_event(&self, event: Event) {
        self.session_manager.process_received_event(event);
        let _ = self.sync_direct_message_subscriptions();
    }

    pub fn sync_direct_message_subscriptions(&self) -> Result<()> {
        // The relay REQ for direct messages is filtered by author pubkeys.
        // The double-ratchet rotates `their_current_nostr_public_key` /
        // `their_next_nostr_public_key` on every step, so this set churns
        // continuously while a chat is active. Each REQ rebuild makes every
        // relay replay all matching historical events, which slams the event
        // pipeline and the UI with redundant work.
        //
        // Two-stage filter:
        //   1. Identical author set → no-op.
        //   2. Otherwise honour a 1.5 s trailing throttle: bursts of ratchet
        //      steps collapse into one relay REQ. If the window has not
        //      elapsed we spawn a one-shot worker that fires the latest
        //      sync at the boundary, even if no further runtime call comes
        //      along to drive it.
        let next_authors = self.session_manager.get_all_message_push_author_pubkeys();

        let unchanged = self
            .direct_message_subscription
            .current
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|subscription| subscription.authors == next_authors);
        if unchanged {
            return Ok(());
        }

        let elapsed_since_last_change = self
            .direct_message_subscription
            .last_change
            .lock()
            .unwrap()
            .map(|last| last.elapsed());
        if let Some(elapsed) = elapsed_since_last_change {
            if elapsed < DM_SUBSCRIPTION_THROTTLE {
                self.schedule_direct_message_subscription_flush(DM_SUBSCRIPTION_THROTTLE - elapsed);
                return Ok(());
            }
        }

        self.apply_direct_message_subscription(next_authors)
    }

    fn apply_direct_message_subscription(&self, next_authors: Vec<PublicKey>) -> Result<()> {
        let mut current = self.direct_message_subscription.current.lock().unwrap();
        let mut last_change = self.direct_message_subscription.last_change.lock().unwrap();

        if current
            .as_ref()
            .is_some_and(|subscription| subscription.authors == next_authors)
        {
            return Ok(());
        }

        if let Some(subscription) = current.take() {
            let _ = self
                .event_tx
                .send(SessionManagerEvent::Unsubscribe(subscription.subid));
        }

        if next_authors.is_empty() {
            *last_change = Some(Instant::now());
            return Ok(());
        }

        let filter = crate::pubsub::build_filter()
            .kinds(vec![MESSAGE_EVENT_KIND as u64])
            .authors(next_authors.clone())
            .build();
        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("ndr-runtime-messages-{}", uuid::Uuid::new_v4());
        let _ = self.event_tx.send(SessionManagerEvent::Subscribe {
            subid: subid.clone(),
            filter_json,
        });
        *current = Some(DirectMessageSubscription {
            subid,
            authors: next_authors,
        });
        *last_change = Some(Instant::now());

        Ok(())
    }

    fn schedule_direct_message_subscription_flush(&self, delay: Duration) {
        // One-shot trailing flush. `flush_pending` deduplicates so a burst of
        // throttled calls only spawns a single worker thread.
        if self
            .direct_message_subscription
            .flush_pending
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        let shared_weak = Arc::downgrade(&self.direct_message_subscription);
        let session_manager = Arc::clone(&self.session_manager);
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let Some(shared) = shared_weak.upgrade() else {
                return;
            };
            shared.flush_pending.store(false, Ordering::Release);
            DirectMessageSubscriptionShared::flush_now(&shared, &session_manager, &event_tx);
        });
    }
}

impl DirectMessageSubscriptionShared {
    fn flush_now(
        shared: &Arc<Self>,
        session_manager: &SessionManager,
        event_tx: &Sender<SessionManagerEvent>,
    ) {
        let next_authors = session_manager.get_all_message_push_author_pubkeys();

        let mut current = shared.current.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|subscription| subscription.authors == next_authors)
        {
            return;
        }
        let mut last_change = shared.last_change.lock().unwrap();

        if let Some(subscription) = current.take() {
            let _ = event_tx.send(SessionManagerEvent::Unsubscribe(subscription.subid));
        }

        if next_authors.is_empty() {
            *last_change = Some(Instant::now());
            return;
        }

        let filter = crate::pubsub::build_filter()
            .kinds(vec![MESSAGE_EVENT_KIND as u64])
            .authors(next_authors.clone())
            .build();
        let Ok(filter_json) = serde_json::to_string(&filter) else {
            return;
        };
        let subid = format!("ndr-runtime-messages-{}", uuid::Uuid::new_v4());
        let _ = event_tx.send(SessionManagerEvent::Subscribe {
            subid: subid.clone(),
            filter_json,
        });
        *current = Some(DirectMessageSubscription {
            subid,
            authors: next_authors,
        });
        *last_change = Some(Instant::now());
    }
}

impl NdrRuntime {
    pub fn pending_invite_response_owner_pubkeys(&self) -> Vec<PublicKey> {
        self.session_manager.pending_invite_response_owner_pubkeys()
    }

    pub fn get_owner_pubkey(&self) -> PublicKey {
        self.session_manager.get_owner_pubkey()
    }

    pub fn get_our_pubkey(&self) -> PublicKey {
        self.session_manager.get_our_pubkey()
    }

    pub fn get_device_id(&self) -> &str {
        self.session_manager.get_device_id()
    }

    pub fn set_auto_adopt_chat_settings(&self, enabled: bool) {
        self.session_manager.set_auto_adopt_chat_settings(enabled)
    }

    pub fn session_manager(&self) -> &SessionManager {
        &self.session_manager
    }

    pub fn with_group_context<R>(
        &self,
        f: impl FnOnce(&SessionManager, &mut GroupManager, &Sender<SessionManagerEvent>) -> R,
    ) -> R {
        let mut group_manager = self.group_manager.lock().unwrap();
        f(&self.session_manager, &mut group_manager, &self.event_tx)
    }

    pub fn sync_groups(&self, groups: Vec<GroupData>) -> Result<()> {
        self.with_group_context(|_, group_manager, _| {
            let next_group_ids: HashSet<String> =
                groups.iter().map(|group| group.id.clone()).collect();
            let stale_group_ids: Vec<String> = group_manager
                .managed_group_ids()
                .into_iter()
                .filter(|group_id| !next_group_ids.contains(group_id))
                .collect();

            for group in groups {
                group_manager.upsert_group(group)?;
            }
            for group_id in stale_group_ids {
                group_manager.remove_group(&group_id);
            }

            Ok(())
        })
    }

    pub fn group_known_sender_event_pubkeys(&self) -> Vec<PublicKey> {
        self.with_group_context(|_, group_manager, _| group_manager.known_sender_event_pubkeys())
    }

    pub fn group_outer_subscription_plan(&self) -> GroupOuterSubscriptionPlan {
        self.with_group_context(|_, group_manager, _| group_manager.outer_subscription_plan())
    }

    pub fn group_handle_incoming_session_event(
        &self,
        event: &UnsignedEvent,
        from_owner_pubkey: PublicKey,
        from_sender_device_pubkey: Option<PublicKey>,
    ) -> Vec<GroupDecryptedEvent> {
        self.with_group_context(|_, group_manager, _| {
            group_manager.handle_incoming_session_event(
                event,
                from_owner_pubkey,
                from_sender_device_pubkey,
            )
        })
    }

    pub fn group_handle_outer_event(&self, outer: &Event) -> Option<GroupDecryptedEvent> {
        self.with_group_context(|_, group_manager, _| group_manager.handle_outer_event(outer))
    }

    pub fn drain_events(&self) -> Vec<SessionManagerEvent> {
        let event_rx = self.event_rx.lock().unwrap();
        event_rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use nostr::Keys;

    use crate::group::create_group_data;
    use crate::{SerializableKeyPair, SessionManagerEvent, SessionState, MESSAGE_EVENT_KIND};

    use super::NdrRuntime;

    fn session_state_tracking(current: nostr::PublicKey, next: nostr::PublicKey) -> SessionState {
        let our_next = Keys::generate();
        SessionState {
            root_key: [1; 32],
            their_current_nostr_public_key: Some(current),
            their_next_nostr_public_key: Some(next),
            our_current_nostr_key: None,
            our_next_nostr_key: SerializableKeyPair {
                public_key: our_next.public_key(),
                private_key: our_next.secret_key().to_secret_bytes(),
            },
            receiving_chain_key: None,
            sending_chain_key: None,
            sending_chain_message_number: 0,
            receiving_chain_message_number: 0,
            previous_sending_chain_message_count: 0,
            skipped_keys: Default::default(),
        }
    }

    #[test]
    fn runtime_init_queues_initial_publish_events() {
        let keys = Keys::generate();
        let runtime = NdrRuntime::new(
            keys.public_key(),
            keys.secret_key().secret_bytes(),
            keys.public_key().to_hex(),
            keys.public_key(),
            None,
            None,
        );

        runtime.init().unwrap();

        assert!(!runtime.drain_events().is_empty());
    }

    #[test]
    fn runtime_setup_user_delegates_to_session_manager() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let runtime = NdrRuntime::new(
            alice.public_key(),
            alice.secret_key().secret_bytes(),
            alice.public_key().to_hex(),
            alice.public_key(),
            None,
            None,
        );

        runtime.init().unwrap();
        runtime.setup_user(bob.public_key()).unwrap();
    }

    #[test]
    fn runtime_owns_direct_message_subscription_lifecycle() {
        let alice = Keys::generate();
        let peer = Keys::generate().public_key();
        let peer_device = Keys::generate().public_key();
        let current_sender = Keys::generate().public_key();
        let next_sender = Keys::generate().public_key();
        let runtime = NdrRuntime::new(
            alice.public_key(),
            alice.secret_key().secret_bytes(),
            alice.public_key().to_hex(),
            alice.public_key(),
            None,
            None,
        );

        runtime
            .import_session_state(
                peer,
                Some(peer_device.to_hex()),
                session_state_tracking(current_sender, next_sender),
            )
            .unwrap();

        let events = runtime.drain_events();
        let (subid, filter_json) = events
            .iter()
            .find_map(|event| match event {
                SessionManagerEvent::Subscribe { subid, filter_json }
                    if subid.starts_with("ndr-runtime-messages-") =>
                {
                    Some((subid.clone(), filter_json.clone()))
                }
                _ => None,
            })
            .expect("runtime should subscribe for session message authors");
        let filter = serde_json::from_str::<serde_json::Value>(&filter_json).unwrap();
        assert_eq!(
            filter.get("kinds").and_then(serde_json::Value::as_array),
            Some(&vec![serde_json::json!(MESSAGE_EVENT_KIND)])
        );
        let authors = filter
            .get("authors")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert!(authors.contains(&current_sender.to_hex().as_str()));
        assert!(authors.contains(&next_sender.to_hex().as_str()));

        runtime.session_manager().delete_chat(peer).unwrap();
        runtime.sync_direct_message_subscriptions().unwrap();
        // The first sync set `last_change`, so the immediate follow-up call
        // is throttled and schedules a trailing-flush worker. Wait long
        // enough for that worker to fire the unsub.
        std::thread::sleep(std::time::Duration::from_millis(1700));
        let events = runtime.drain_events();
        assert!(events.iter().any(|event| {
            matches!(event, SessionManagerEvent::Unsubscribe(unsubid) if unsubid == &subid)
        }));
    }

    #[test]
    fn runtime_sync_groups_replaces_stale_group_set() {
        let alice = Keys::generate();
        let runtime = NdrRuntime::new(
            alice.public_key(),
            alice.secret_key().secret_bytes(),
            alice.public_key().to_hex(),
            alice.public_key(),
            None,
            None,
        );

        let group_one = create_group_data("One", &alice.public_key().to_hex(), &[]);
        let group_two = create_group_data("Two", &alice.public_key().to_hex(), &[]);

        runtime
            .sync_groups(vec![group_one.clone(), group_two.clone()])
            .unwrap();

        let mut initial_group_ids =
            runtime.with_group_context(|_, group_manager, _| group_manager.managed_group_ids());
        initial_group_ids.sort();
        let mut expected_initial_group_ids = vec![group_one.id.clone(), group_two.id.clone()];
        expected_initial_group_ids.sort();
        assert_eq!(initial_group_ids, expected_initial_group_ids);

        runtime.sync_groups(vec![group_two.clone()]).unwrap();

        let updated_group_ids =
            runtime.with_group_context(|_, group_manager, _| group_manager.managed_group_ids());
        assert_eq!(updated_group_ids, vec![group_two.id]);
    }
}
