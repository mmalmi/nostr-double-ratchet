use crate::{Error, Result, APP_KEYS_EVENT_KIND};
use nostr::{Event, EventBuilder, Kind, PublicKey, Tag, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const APP_KEYS_D_TAG: &str = "double-ratchet/app-keys";

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub identity_pubkey: PublicKey,
    pub created_at: u64,
}

impl DeviceEntry {
    pub fn new(identity_pubkey: PublicKey, created_at: u64) -> Self {
        Self {
            identity_pubkey,
            created_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppKeys {
    devices: HashMap<PublicKey, DeviceEntry>,
}

impl AppKeys {
    pub fn new(devices: Vec<DeviceEntry>) -> Self {
        let mut map = HashMap::new();
        for device in devices {
            map.insert(device.identity_pubkey, device);
        }
        Self { devices: map }
    }

    pub fn add_device(&mut self, device: DeviceEntry) {
        self.devices
            .entry(device.identity_pubkey)
            .or_insert(device);
    }

    pub fn remove_device(&mut self, identity_pubkey: &PublicKey) {
        self.devices.remove(identity_pubkey);
    }

    pub fn get_device(&self, identity_pubkey: &PublicKey) -> Option<&DeviceEntry> {
        self.devices.get(identity_pubkey)
    }

    pub fn get_all_devices(&self) -> Vec<DeviceEntry> {
        self.devices.values().cloned().collect()
    }

    pub fn get_event(&self, owner_pubkey: PublicKey) -> UnsignedEvent {
        let mut tags = Vec::new();
        let d_tag = Tag::parse(&["d".to_string(), APP_KEYS_D_TAG.to_string()])
            .unwrap_or_else(|_| Tag::parse(&["d".to_string(), APP_KEYS_D_TAG.to_string()]).unwrap());
        tags.push(d_tag);
        tags.push(
            Tag::parse(&["version".to_string(), "1".to_string()])
                .unwrap_or_else(|_| Tag::parse(&["version".to_string(), "1".to_string()]).unwrap()),
        );

        for device in self.get_all_devices() {
            let device_tag = Tag::parse(&[
                "device".to_string(),
                hex::encode(device.identity_pubkey.to_bytes()),
                device.created_at.to_string(),
            ])
            .map_err(|e| Error::InvalidEvent(e.to_string()))
            .unwrap();
            tags.push(device_tag);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        EventBuilder::new(Kind::from(APP_KEYS_EVENT_KIND as u16), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(now))
            .build(owner_pubkey)
    }

    pub fn from_event(event: &Event) -> Result<Self> {
        if event.verify().is_err() {
            return Err(Error::InvalidEvent("Invalid signature".to_string()));
        }

        let has_d_tag = event.tags.iter().any(|t| {
            let vals = t.clone().to_vec();
            vals.first().map(|s| s.as_str()) == Some("d")
                && vals.get(1).map(|s| s.as_str()) == Some(APP_KEYS_D_TAG)
        });

        if !has_d_tag {
            return Err(Error::InvalidEvent("Missing app-keys d tag".to_string()));
        }

        let mut devices = Vec::new();
        for tag in event.tags.iter() {
            let vals = tag.clone().to_vec();
            if vals.first().map(|s| s.as_str()) != Some("device") {
                continue;
            }
            if vals.len() < 3 {
                continue;
            }

            let pk_hex = vals[1].clone();
            let created_at_str = vals[2].clone();
            let created_at = created_at_str
                .parse::<u64>()
                .unwrap_or_else(|_| event.created_at.as_u64());

            let pk = crate::utils::pubkey_from_hex(&pk_hex)?;
            devices.push(DeviceEntry::new(pk, created_at));
        }

        Ok(AppKeys::new(devices))
    }

    pub fn serialize(&self) -> Result<String> {
        #[derive(Serialize)]
        struct StoredDevice {
            identity_pubkey: String,
            created_at: u64,
        }
        #[derive(Serialize)]
        struct StoredAppKeys {
            devices: Vec<StoredDevice>,
        }

        let devices = self
            .get_all_devices()
            .into_iter()
            .map(|d| StoredDevice {
                identity_pubkey: hex::encode(d.identity_pubkey.to_bytes()),
                created_at: d.created_at,
            })
            .collect();

        Ok(serde_json::to_string(&StoredAppKeys { devices })?)
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct StoredDevice {
            identity_pubkey: String,
            created_at: u64,
        }
        #[derive(Deserialize)]
        struct StoredAppKeys {
            devices: Vec<StoredDevice>,
        }

        let data: StoredAppKeys = serde_json::from_str(json)?;
        let devices = data
            .devices
            .into_iter()
            .filter_map(|d| {
                crate::utils::pubkey_from_hex(&d.identity_pubkey)
                    .ok()
                    .map(|pk| DeviceEntry::new(pk, d.created_at))
            })
            .collect();

        Ok(AppKeys::new(devices))
    }

    pub fn merge(&self, other: &AppKeys) -> AppKeys {
        let mut merged = HashMap::new();

        for device in self.get_all_devices().into_iter().chain(other.get_all_devices()) {
            merged
                .entry(device.identity_pubkey)
                .and_modify(|existing: &mut DeviceEntry| {
                    if device.created_at < existing.created_at {
                        *existing = device.clone();
                    }
                })
                .or_insert(device);
        }

        AppKeys { devices: merged }
    }
}

pub fn is_app_keys_event(event: &Event) -> bool {
    if event.kind.as_u16() != APP_KEYS_EVENT_KIND as u16 {
        return false;
    }

    event.tags.iter().any(|t| {
        let vals = t.clone().to_vec();
        vals.first().map(|s| s.as_str()) == Some("d")
            && vals.get(1).map(|s| s.as_str()) == Some(APP_KEYS_D_TAG)
    })
}
