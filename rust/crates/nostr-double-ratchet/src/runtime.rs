use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use nostr::PublicKey;

use crate::{
    AcceptInviteResult, AppKeys, GroupManager, GroupManagerOptions, InMemoryStorage, Invite,
    Result, SendOptions, SessionManager, SessionManagerEvent, StorageAdapter,
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

    pub fn drain_events(&self) -> Vec<SessionManagerEvent> {
        let event_rx = self.event_rx.lock().unwrap();
        event_rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use nostr::Keys;

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
}
