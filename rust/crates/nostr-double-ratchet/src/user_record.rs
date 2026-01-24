use crate::{Session, SessionState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct StoredDeviceRecord {
    pub device_id: String,
    pub active_session: Option<SessionState>,
    pub inactive_sessions: Vec<SessionState>,
    pub is_stale: bool,
    pub stale_timestamp: Option<u64>,
    pub last_activity: Option<u64>,
}

pub struct DeviceRecord {
    pub device_id: String,
    pub public_key: String,
    pub active_session: Option<Session>,
    pub inactive_sessions: Vec<Session>,
    pub is_stale: bool,
    pub stale_timestamp: Option<u64>,
    pub last_activity: Option<u64>,
}

pub struct UserRecord {
    pub user_id: String,
    pub device_records: HashMap<String, DeviceRecord>,
    is_stale: bool,
    _stale_timestamp: Option<u64>,
}

impl UserRecord {
    pub fn new(user_id: String) -> Self {
        Self {
            user_id,
            device_records: HashMap::new(),
            is_stale: false,
            _stale_timestamp: None,
        }
    }

    pub fn upsert_session(&mut self, device_id: Option<&str>, session: Session) {
        let device_id = device_id.unwrap_or("unknown").to_string();

        let device = self.device_records.entry(device_id.clone()).or_insert_with(|| {
            DeviceRecord {
                device_id: device_id.clone(),
                public_key: String::new(),
                active_session: None,
                inactive_sessions: Vec::new(),
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()),
            }
        });

        // Prefer sendable sessions as active
        let new_can_send = session.can_send();
        let old_can_send = device.active_session.as_ref().map(|s| s.can_send()).unwrap_or(false);

        if let Some(old_session) = device.active_session.take() {
            // If new session can send but old can't, replace
            // If old session can send but new can't, keep old as active, new as inactive
            if old_can_send && !new_can_send {
                device.inactive_sessions.push(session);
                device.active_session = Some(old_session);
            } else {
                device.inactive_sessions.push(old_session);
                device.active_session = Some(session);
            }
        } else {
            device.active_session = Some(session);
        }

        device.last_activity = Some(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs());
    }

    pub fn get_all_sessions_mut(&mut self) -> Vec<&mut Session> {
        if self.is_stale {
            return Vec::new();
        }

        let mut sessions: Vec<&mut Session> = Vec::new();
        for device in self.device_records.values_mut().filter(|d| !d.is_stale) {
            if let Some(ref mut active) = device.active_session {
                sessions.push(active);
            }
            for inactive in &mut device.inactive_sessions {
                sessions.push(inactive);
            }
        }
        sessions
    }

    pub fn get_active_sessions_mut(&mut self) -> Vec<&mut Session> {
        if self.is_stale {
            return Vec::new();
        }

        let mut sessions: Vec<&mut Session> = self
            .device_records
            .values_mut()
            .filter(|d| !d.is_stale)
            .filter_map(|d| d.active_session.as_mut())
            .collect();

        sessions.sort_by(|a, b| {
            let a_can_send = a.can_send();
            let b_can_send = b.can_send();

            match (a_can_send, b_can_send) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        });

        sessions
    }

    pub fn close(&mut self) {
        for device in self.device_records.values_mut() {
            if let Some(session) = device.active_session.take() {
                session.close();
            }
            for session in device.inactive_sessions.drain(..) {
                session.close();
            }
        }
        self.device_records.clear();
    }

    pub fn to_stored(&self) -> StoredUserRecord {
        StoredUserRecord {
            user_id: self.user_id.clone(),
            devices: self
                .device_records
                .values()
                .map(|d| StoredDeviceRecord {
                    device_id: d.device_id.clone(),
                    active_session: d.active_session.as_ref().map(|s| s.state.clone()),
                    inactive_sessions: d
                        .inactive_sessions
                        .iter()
                        .map(|s| s.state.clone())
                        .collect(),
                    is_stale: d.is_stale,
                    stale_timestamp: d.stale_timestamp,
                    last_activity: d.last_activity,
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct StoredUserRecord {
    pub user_id: String,
    pub devices: Vec<StoredDeviceRecord>,
}
