use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use nostr::{Event, PublicKey, UnsignedEvent};

use crate::{
    group::GroupData, AcceptInviteResult, AppKeys, GroupDecryptedEvent, GroupManager,
    GroupManagerOptions, GroupOuterSubscriptionPlan, InMemoryStorage, Invite, Result, SendOptions,
    SessionManager, SessionManagerEvent, StorageAdapter,
};

pub struct NdrRuntime {
    session_manager: SessionManager,
    group_manager: Mutex<GroupManager>,
    event_rx: Mutex<Receiver<SessionManagerEvent>>,
    event_tx: Sender<SessionManagerEvent>,
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
            session_manager,
            group_manager: Mutex::new(group_manager),
            event_rx: Mutex::new(event_rx),
            event_tx,
        }
    }

    pub fn init(&self) -> Result<()> {
        self.session_manager.init()
    }

    pub fn setup_user(&self, user_pubkey: PublicKey) -> Result<()> {
        self.session_manager.init()?;
        self.session_manager.setup_user(user_pubkey);
        Ok(())
    }

    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<AcceptInviteResult> {
        self.session_manager.init()?;
        self.session_manager
            .accept_invite(invite, owner_pubkey_hint)
    }

    pub fn send_text(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        self.session_manager.send_text(recipient, text, options)
    }

    pub fn send_text_with_inner_id(
        &self,
        recipient: PublicKey,
        text: String,
        options: Option<SendOptions>,
    ) -> Result<(String, Vec<String>)> {
        self.session_manager
            .send_text_with_inner_id(recipient, text, options)
    }

    pub fn send_event(
        &self,
        recipient: PublicKey,
        event: nostr::UnsignedEvent,
    ) -> Result<Vec<String>> {
        self.session_manager.send_event(recipient, event)
    }

    pub fn send_reaction(
        &self,
        recipient: PublicKey,
        message_id: String,
        emoji: String,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        self.session_manager
            .send_reaction(recipient, message_id, emoji, options)
    }

    pub fn send_receipt(
        &self,
        recipient: PublicKey,
        receipt_type: &str,
        message_ids: Vec<String>,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        self.session_manager
            .send_receipt(recipient, receipt_type, message_ids, options)
    }

    pub fn send_typing(
        &self,
        recipient: PublicKey,
        options: Option<SendOptions>,
    ) -> Result<Vec<String>> {
        self.session_manager.send_typing(recipient, options)
    }

    pub fn send_chat_settings(
        &self,
        recipient: PublicKey,
        ttl_seconds: u64,
    ) -> Result<Vec<String>> {
        self.session_manager
            .send_chat_settings(recipient, ttl_seconds)
    }

    pub fn import_session_state(
        &self,
        peer_pubkey: PublicKey,
        device_id: Option<String>,
        state: crate::SessionState,
    ) -> Result<()> {
        self.session_manager
            .import_session_state(peer_pubkey, device_id, state)
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
            .ingest_app_keys_snapshot(owner_pubkey, app_keys, created_at)
    }

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

    use super::NdrRuntime;

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
