use crate::{AppKeys, DeviceEntry, Result, StorageAdapter};
use crate::{InMemoryStorage, NostrPubSub};
use nostr::PublicKey;
use std::sync::Arc;

pub struct AppKeysManager {
    pubsub: Arc<dyn NostrPubSub>,
    storage: Arc<dyn StorageAdapter>,
    app_keys: Option<AppKeys>,
    initialized: bool,
    storage_version: String,
}

impl AppKeysManager {
    pub fn new(pubsub: Arc<dyn NostrPubSub>, storage: Option<Arc<dyn StorageAdapter>>) -> Self {
        Self {
            pubsub,
            storage: storage.unwrap_or_else(|| Arc::new(InMemoryStorage::new())),
            app_keys: None,
            initialized: false,
            storage_version: "1".to_string(),
        }
    }

    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;

        if let Some(data) = self.storage.get(&self.app_keys_key())? {
            if let Ok(keys) = AppKeys::deserialize(&data) {
                self.app_keys = Some(keys);
            }
        }

        if self.app_keys.is_none() {
            self.app_keys = Some(AppKeys::new(Vec::new()));
        }

        Ok(())
    }

    pub fn get_app_keys(&self) -> Option<&AppKeys> {
        self.app_keys.as_ref()
    }

    pub fn get_own_devices(&self) -> Vec<DeviceEntry> {
        self.app_keys
            .as_ref()
            .map(|a| a.get_all_devices())
            .unwrap_or_default()
    }

    pub fn add_device(&mut self, device: DeviceEntry) -> Result<()> {
        if self.app_keys.is_none() {
            self.app_keys = Some(AppKeys::new(Vec::new()));
        }
        let serialized = if let Some(app_keys) = self.app_keys.as_mut() {
            app_keys.add_device(device);
            Some(app_keys.serialize()?)
        } else {
            None
        };
        if let Some(serialized) = serialized {
            let key = self.app_keys_key();
            self.storage.put(&key, serialized)?;
        }
        Ok(())
    }

    pub fn revoke_device(&mut self, identity_pubkey: &PublicKey) -> Result<()> {
        let serialized = if let Some(app_keys) = self.app_keys.as_mut() {
            app_keys.remove_device(identity_pubkey);
            Some(app_keys.serialize()?)
        } else {
            None
        };
        if let Some(serialized) = serialized {
            let key = self.app_keys_key();
            self.storage.put(&key, serialized)?;
        }
        Ok(())
    }

    pub fn set_app_keys(&mut self, keys: AppKeys) -> Result<()> {
        self.app_keys = Some(keys);
        if let Some(app_keys) = self.app_keys.as_ref() {
            self.save_app_keys(app_keys)?;
        }
        Ok(())
    }

    pub fn publish(&self, owner_pubkey: PublicKey) -> Result<()> {
        if let Some(app_keys) = self.app_keys.as_ref() {
            let event = app_keys.get_event(owner_pubkey);
            self.pubsub.publish(event)?;
        }
        Ok(())
    }

    pub fn close(&mut self) {
        // No-op for now
    }

    fn app_keys_key(&self) -> String {
        format!("v{}/app-keys-manager/app-keys", self.storage_version)
    }

    fn save_app_keys(&self, app_keys: &AppKeys) -> Result<()> {
        self.storage
            .put(&self.app_keys_key(), app_keys.serialize()?)?;
        Ok(())
    }
}
