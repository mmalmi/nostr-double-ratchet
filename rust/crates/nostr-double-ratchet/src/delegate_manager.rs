use crate::{Error, Invite, Result, StorageAdapter};
use crate::{InMemoryStorage, NostrPubSub};
use nostr::Keys;
use nostr::PublicKey;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct DelegatePayload {
    pub identity_pubkey: PublicKey,
}

pub struct DelegateManager {
    pubsub: Arc<dyn NostrPubSub>,
    storage: Arc<dyn StorageAdapter>,
    device_public_key: PublicKey,
    device_private_key: [u8; 32],
    invite: Option<Invite>,
    owner_pubkey: Arc<Mutex<Option<PublicKey>>>,
    initialized: bool,
    storage_version: String,
    activation_waiters: Arc<Mutex<Vec<crossbeam_channel::Sender<PublicKey>>>>,
}

impl DelegateManager {
    pub fn new(pubsub: Arc<dyn NostrPubSub>, storage: Option<Arc<dyn StorageAdapter>>) -> Self {
        Self {
            pubsub,
            storage: storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            device_public_key: Keys::generate().public_key(),
            device_private_key: [0u8; 32],
            invite: None,
            owner_pubkey: Arc::new(Mutex::new(None)),
            initialized: false,
            storage_version: "1".to_string(),
            activation_waiters: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;

        // Load or generate identity keys
        let stored_public = self.storage.get(&self.identity_public_key_key())?;
        let stored_private = self.storage.get(&self.identity_private_key_key())?;

        if let (Some(pub_hex), Some(priv_hex)) = (stored_public, stored_private) {
            self.device_public_key = crate::utils::pubkey_from_hex(&pub_hex)?;
            let bytes = hex::decode(priv_hex)?;
            self.device_private_key = bytes
                .try_into()
                .map_err(|_| Error::Storage("Invalid private key".to_string()))?;
        } else {
            let keys = Keys::generate();
            self.device_public_key = keys.public_key();
            self.device_private_key = keys.secret_key().to_secret_bytes();
            self.storage.put(
                &self.identity_public_key_key(),
                hex::encode(self.device_public_key.to_bytes()),
            )?;
            self.storage.put(
                &self.identity_private_key_key(),
                hex::encode(self.device_private_key),
            )?;
        }

        if let Some(owner_hex) = self.storage.get(&self.owner_pubkey_key())? {
            if let Ok(owner_pk) = crate::utils::pubkey_from_hex(&owner_hex) {
                *self.owner_pubkey.lock().unwrap() = Some(owner_pk);
            }
        }

        // Load or create invite
        let stored_invite = self
            .storage
            .get(&self.invite_key())?
            .and_then(|data| Invite::deserialize(&data).ok());

        let device_id = hex::encode(self.device_public_key.to_bytes());
        let invite = match stored_invite {
            Some(invite) => invite,
            None => Invite::create_new(self.device_public_key, Some(device_id), None)?,
        };
        self.storage.put(&self.invite_key(), invite.serialize()?)?;
        self.invite = Some(invite.clone());

        // Publish signed invite event
        if let Ok(unsigned) = invite.get_event() {
            let keys = Keys::new(nostr::SecretKey::from_slice(&self.device_private_key)?);
            let signed = unsigned
                .sign_with_keys(&keys)
                .map_err(|e| Error::InvalidEvent(e.to_string()))?;
            let _ = self.pubsub.publish_signed(signed);
        }

        Ok(())
    }

    pub fn get_registration_payload(&self) -> DelegatePayload {
        DelegatePayload {
            identity_pubkey: self.device_public_key,
        }
    }

    pub fn get_identity_public_key(&self) -> PublicKey {
        self.device_public_key
    }

    pub fn get_identity_private_key(&self) -> [u8; 32] {
        self.device_private_key
    }

    pub fn get_invite(&self) -> Option<Invite> {
        self.invite.clone()
    }

    pub fn get_owner_public_key(&self) -> Option<PublicKey> {
        *self.owner_pubkey.lock().unwrap()
    }

    pub fn rotate_invite(&mut self) -> Result<()> {
        let device_id = hex::encode(self.device_public_key.to_bytes());
        let invite = Invite::create_new(self.device_public_key, Some(device_id), None)?;
        self.storage.put(&self.invite_key(), invite.serialize()?)?;
        self.invite = Some(invite.clone());

        let keys = Keys::new(nostr::SecretKey::from_slice(&self.device_private_key)?);
        let signed = invite
            .get_event()?
            .sign_with_keys(&keys)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        let _ = self.pubsub.publish_signed(signed);

        Ok(())
    }

    pub fn activate(&mut self, owner_pubkey: PublicKey) -> Result<()> {
        *self.owner_pubkey.lock().unwrap() = Some(owner_pubkey);
        self.storage.put(
            &self.owner_pubkey_key(),
            hex::encode(owner_pubkey.to_bytes()),
        )?;
        Ok(())
    }

    pub fn wait_for_activation(&mut self, timeout: Duration) -> Result<PublicKey> {
        if let Some(owner) = *self.owner_pubkey.lock().unwrap() {
            return Ok(owner);
        }

        let (tx, rx) = crossbeam_channel::bounded(1);
        self.activation_waiters.lock().unwrap().push(tx);

        // Subscription for AppKeys handled externally; caller must feed events via process_received_event
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(owner) = rx.recv_timeout(Duration::from_millis(100)) {
                return Ok(owner);
            }
        }

        Err(Error::Invite("Activation timeout".to_string()))
    }

    pub fn process_received_event(&mut self, event: &nostr::Event) -> Result<()> {
        if !crate::is_app_keys_event(event) {
            return Ok(());
        }

        let app_keys = crate::AppKeys::from_event(event)?;
        let device_in_list = app_keys
            .get_all_devices()
            .iter()
            .any(|d| d.identity_pubkey.to_bytes() == self.device_public_key.to_bytes());

        if device_in_list {
            let owner_pubkey = event.pubkey;
            *self.owner_pubkey.lock().unwrap() = Some(owner_pubkey);
            self.storage.put(
                &self.owner_pubkey_key(),
                hex::encode(owner_pubkey.to_bytes()),
            )?;
            let mut waiters = self.activation_waiters.lock().unwrap();
            for waiter in waiters.drain(..) {
                let _ = waiter.send(owner_pubkey);
            }
        }

        Ok(())
    }

    fn owner_pubkey_key(&self) -> String {
        format!("v{}/delegate-manager/owner-pubkey", self.storage_version)
    }

    fn invite_key(&self) -> String {
        format!("v{}/delegate-manager/invite", self.storage_version)
    }

    fn identity_public_key_key(&self) -> String {
        format!("v{}/delegate-manager/identity-public-key", self.storage_version)
    }

    fn identity_private_key_key(&self) -> String {
        format!("v{}/delegate-manager/identity-private-key", self.storage_version)
    }
}
