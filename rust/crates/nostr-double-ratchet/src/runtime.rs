use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use nostr::{Event, PublicKey, UnsignedEvent};

use crate::{
    group::GroupData, AcceptInviteResult, AppKeys, GroupDecryptedEvent, GroupManager,
    GroupManagerOptions, GroupOuterSubscriptionPlan, InMemoryStorage, Invite,
    MessagePushSessionStateSnapshot, Result, SendOptions, SessionManager, SessionManagerEvent,
    StorageAdapter, MESSAGE_EVENT_KIND,
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
        Self::new_with_group_storage(
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            storage.clone(),
            storage,
            invite,
        )
    }

    pub fn new_with_group_storage(
        our_public_key: PublicKey,
        our_identity_key: [u8; 32],
        device_id: String,
        owner_public_key: PublicKey,
        session_storage: Option<Arc<dyn StorageAdapter>>,
        group_storage: Option<Arc<dyn StorageAdapter>>,
        invite: Option<Invite>,
    ) -> Self {
        let session_storage = session_storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new()));
        let group_storage = group_storage.unwrap_or_else(|| Arc::clone(&session_storage));
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
        let session_manager = SessionManager::new(
            our_public_key,
            our_identity_key,
            device_id,
            owner_public_key,
            event_tx.clone(),
            Some(session_storage),
            invite,
        );
        let group_manager = GroupManager::new(GroupManagerOptions {
            our_owner_pubkey: owner_public_key,
            our_device_pubkey: our_public_key,
            storage: Some(group_storage),
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

    pub fn reload_from_storage(&self) -> Result<()> {
        self.session_manager.reload_from_storage()?;
        self.sync_direct_message_subscriptions()
    }

    pub fn delete_chat(&self, user_pubkey: PublicKey) -> Result<()> {
        self.session_manager.delete_chat(user_pubkey)?;
        self.sync_direct_message_subscriptions()
    }

    pub fn cleanup_discovery_queue(&self, max_age_ms: u64) -> Result<usize> {
        self.session_manager.cleanup_discovery_queue(max_age_ms)
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

    pub fn export_active_session_state(
        &self,
        peer_pubkey: PublicKey,
    ) -> Result<Option<crate::SessionState>> {
        self.session_manager
            .export_active_session_state(peer_pubkey)
    }

    pub fn get_stored_user_record_json(&self, user_pubkey: PublicKey) -> Result<Option<String>> {
        self.session_manager
            .get_stored_user_record_json(user_pubkey)
    }

    pub fn get_all_message_push_author_pubkeys(&self) -> Vec<PublicKey> {
        self.session_manager.get_all_message_push_author_pubkeys()
    }

    pub fn get_message_push_author_pubkeys(&self, peer_owner_pubkey: PublicKey) -> Vec<PublicKey> {
        self.session_manager
            .get_message_push_author_pubkeys(peer_owner_pubkey)
    }

    pub fn get_message_push_session_states(
        &self,
        peer_owner_pubkey: PublicKey,
    ) -> Vec<MessagePushSessionStateSnapshot> {
        self.session_manager
            .get_message_push_session_states(peer_owner_pubkey)
    }

    pub fn known_peer_owner_pubkeys(&self) -> Vec<PublicKey> {
        self.session_manager.known_peer_owner_pubkeys()
    }

    pub fn known_device_identity_pubkeys_for_owner(
        &self,
        owner_pubkey: PublicKey,
    ) -> Vec<PublicKey> {
        self.session_manager
            .known_device_identity_pubkeys_for_owner(owner_pubkey)
    }

    pub fn get_total_sessions(&self) -> usize {
        self.session_manager.get_total_sessions()
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
        // Three-stage filter:
        //   1. Identical author set → no-op.
        //   2. Newly added authors are subscribed immediately. They may already
        //      have relay events waiting, and delaying them can miss live delivery.
        //   3. Pure removals honour a 1.5 s trailing throttle so bursts of
        //      ratchet steps collapse into one relay REQ. If the window has not
        //      elapsed we spawn a one-shot worker that fires the latest sync at
        //      the boundary, even if no further runtime call comes along to drive it.
        let next_authors = self.session_manager.get_all_message_push_author_pubkeys();

        let current_authors = self
            .direct_message_subscription
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|subscription| subscription.authors.clone())
            .unwrap_or_default();
        if current_authors == next_authors {
            return Ok(());
        }

        let has_added_authors = next_authors
            .iter()
            .any(|author| !current_authors.contains(author));
        let elapsed_since_last_change = self
            .direct_message_subscription
            .last_change
            .lock()
            .unwrap()
            .map(|last| last.elapsed());
        if let Some(elapsed) = elapsed_since_last_change {
            if elapsed < DM_SUBSCRIPTION_THROTTLE && !has_added_authors {
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

    pub fn current_device_invite_response_pubkey(&self) -> Option<PublicKey> {
        self.session_manager.current_device_invite_response_pubkey()
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

    pub fn queued_message_diagnostics(
        &self,
        inner_event_id: Option<&str>,
    ) -> Result<Vec<crate::QueuedMessageDiagnostic>> {
        self.session_manager
            .queued_message_diagnostics(inner_event_id)
    }
}

#[cfg(test)]
mod tests {
    use nostr::{Keys, PublicKey};

    use crate::group::create_group_data;
    use crate::{
        AppKeys, DeviceEntry, SerializableKeyPair, SessionManagerEvent, SessionState,
        CHAT_MESSAGE_KIND, MESSAGE_EVENT_KIND,
    };

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

    fn signed_events(events: Vec<SessionManagerEvent>) -> Vec<nostr::Event> {
        events
            .into_iter()
            .filter_map(|event| match event {
                SessionManagerEvent::PublishSigned(event) => Some(event),
                SessionManagerEvent::PublishSignedForInnerEvent { event, .. } => Some(event),
                _ => None,
            })
            .collect()
    }

    fn route_runtime_events(runtimes: &[&NdrRuntime]) {
        for _ in 0..32 {
            let mut routed = 0usize;
            for (origin, runtime) in runtimes.iter().enumerate() {
                for event in signed_events(runtime.drain_events()) {
                    routed += 1;
                    for (target, recipient) in runtimes.iter().enumerate() {
                        if target != origin {
                            recipient.process_received_event(event.clone());
                        }
                    }
                }
            }

            if routed == 0 {
                return;
            }
        }

        panic!("runtime event routing did not quiesce");
    }

    fn sorted_pubkeys(mut pubkeys: Vec<PublicKey>) -> Vec<PublicKey> {
        pubkeys.sort_by_key(|pubkey| pubkey.to_hex());
        pubkeys
    }

    fn decrypted_chat_messages(
        runtime: &NdrRuntime,
        expected_content: &str,
    ) -> Vec<(PublicKey, Option<PublicKey>, nostr::UnsignedEvent)> {
        runtime
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                SessionManagerEvent::DecryptedMessage {
                    sender,
                    sender_device,
                    content,
                    ..
                } => {
                    let rumor = serde_json::from_str::<nostr::UnsignedEvent>(&content).ok()?;
                    (rumor.kind.as_u16() == CHAT_MESSAGE_KIND as u16
                        && rumor.content == expected_content)
                        .then_some((sender, sender_device, rumor))
                }
                _ => None,
            })
            .collect()
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
    fn runtime_subscribes_added_direct_message_authors_without_throttle() {
        let alice = Keys::generate();
        let peer1 = Keys::generate().public_key();
        let peer1_device = Keys::generate().public_key();
        let peer1_current = Keys::generate().public_key();
        let peer1_next = Keys::generate().public_key();
        let peer2 = Keys::generate().public_key();
        let peer2_device = Keys::generate().public_key();
        let peer2_current = Keys::generate().public_key();
        let peer2_next = Keys::generate().public_key();
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
                peer1,
                Some(peer1_device.to_hex()),
                session_state_tracking(peer1_current, peer1_next),
            )
            .unwrap();
        let _ = runtime.drain_events();

        runtime
            .import_session_state(
                peer2,
                Some(peer2_device.to_hex()),
                session_state_tracking(peer2_current, peer2_next),
            )
            .unwrap();

        let events = runtime.drain_events();
        let latest_subscribe = events.iter().rev().find_map(|event| match event {
            SessionManagerEvent::Subscribe { filter_json, .. } => {
                Some(serde_json::from_str::<serde_json::Value>(filter_json).unwrap())
            }
            _ => None,
        });
        let filter = latest_subscribe.expect("new authors should resubscribe immediately");
        let authors = filter
            .get("authors")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert!(authors.contains(&peer1_current.to_hex().as_str()));
        assert!(authors.contains(&peer1_next.to_hex().as_str()));
        assert!(authors.contains(&peer2_current.to_hex().as_str()));
        assert!(authors.contains(&peer2_next.to_hex().as_str()));
    }

    #[test]
    fn fresh_same_owner_runtime_send_reaches_peer_and_existing_owner_device() {
        let alice_owner_keys = Keys::generate();
        let alice_owner = alice_owner_keys.public_key();
        let alice_old_device_keys = Keys::generate();
        let alice_old_device = alice_old_device_keys.public_key();
        let alice_fresh_device_keys = Keys::generate();
        let alice_fresh_device = alice_fresh_device_keys.public_key();
        let bob_keys = Keys::generate();
        let bob = bob_keys.public_key();

        let alice_old = NdrRuntime::new(
            alice_old_device,
            alice_old_device_keys.secret_key().secret_bytes(),
            alice_old_device.to_hex(),
            alice_owner,
            None,
            None,
        );
        let alice_fresh = NdrRuntime::new(
            alice_fresh_device,
            alice_fresh_device_keys.secret_key().secret_bytes(),
            alice_fresh_device.to_hex(),
            alice_owner,
            None,
            None,
        );
        let bob_runtime = NdrRuntime::new(
            bob,
            bob_keys.secret_key().secret_bytes(),
            bob.to_hex(),
            bob,
            None,
            None,
        );

        let alice_app_keys = AppKeys::new(vec![
            DeviceEntry::new(alice_old_device, 1),
            DeviceEntry::new(alice_fresh_device, 2),
        ]);
        let bob_app_keys = AppKeys::new(vec![DeviceEntry::new(bob, 1)]);
        for runtime in [&alice_old, &alice_fresh, &bob_runtime] {
            runtime.ingest_app_keys_snapshot(alice_owner, alice_app_keys.clone(), 10);
            runtime.ingest_app_keys_snapshot(bob, bob_app_keys.clone(), 10);
        }

        alice_old.init().unwrap();
        alice_fresh.init().unwrap();
        bob_runtime.init().unwrap();
        route_runtime_events(&[&alice_old, &alice_fresh, &bob_runtime]);

        let expected_alice_devices = sorted_pubkeys(vec![alice_old_device, alice_fresh_device]);
        assert_eq!(
            sorted_pubkeys(alice_fresh.known_device_identity_pubkeys_for_owner(alice_owner)),
            expected_alice_devices
        );
        assert_eq!(
            sorted_pubkeys(bob_runtime.known_device_identity_pubkeys_for_owner(alice_owner)),
            expected_alice_devices
        );

        let text = "fresh same-owner message";
        let (inner_id, published_ids) = alice_fresh
            .send_text_with_inner_id(bob, text.to_string(), None)
            .unwrap();
        assert!(!inner_id.is_empty());
        assert!(
            published_ids.len() >= 2,
            "fresh sender should publish to peer and existing same-owner device"
        );

        let outbound = alice_fresh.drain_events();
        let target_device_ids = outbound
            .iter()
            .filter_map(|event| match event {
                SessionManagerEvent::PublishSignedForInnerEvent {
                    event,
                    target_device_id,
                    ..
                } if event.kind.as_u16() == MESSAGE_EVENT_KIND as u16 => target_device_id.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(target_device_ids.contains(&bob.to_hex()));
        assert!(target_device_ids.contains(&alice_old_device.to_hex()));
        assert!(!target_device_ids.contains(&alice_fresh_device.to_hex()));

        for event in signed_events(outbound) {
            bob_runtime.process_received_event(event.clone());
            alice_old.process_received_event(event);
        }

        let bob_messages = decrypted_chat_messages(&bob_runtime, text);
        assert!(bob_messages.iter().any(|(sender, sender_device, _)| {
            *sender == alice_owner && *sender_device == Some(alice_fresh_device)
        }));

        let old_device_messages = decrypted_chat_messages(&alice_old, text);
        assert!(
            old_device_messages
                .iter()
                .any(|(sender, sender_device, _)| {
                    *sender == alice_owner && *sender_device == Some(alice_fresh_device)
                }),
            "existing same-owner device should receive the fresh device send as a self message"
        );
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
