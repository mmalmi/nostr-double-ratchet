use crate::{Session, SessionState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct StoredDeviceRecord {
    pub device_id: String,
    pub active_session: Option<SessionState>,
    pub inactive_sessions: Vec<SessionState>,
    #[serde(default)]
    pub created_at: u64,
    pub is_stale: bool,
    pub stale_timestamp: Option<u64>,
    pub last_activity: Option<u64>,
}

pub struct DeviceRecord {
    pub device_id: String,
    pub public_key: String,
    pub active_session: Option<Session>,
    pub inactive_sessions: Vec<Session>,
    pub created_at: u64,
    pub is_stale: bool,
    pub stale_timestamp: Option<u64>,
    pub last_activity: Option<u64>,
}

pub struct UserRecord {
    pub user_id: String,
    pub device_records: HashMap<String, DeviceRecord>,
    pub known_device_identities: Vec<String>,
    is_stale: bool,
    _stale_timestamp: Option<u64>,
}

impl UserRecord {
    pub fn new(user_id: String) -> Self {
        Self {
            user_id,
            device_records: HashMap::new(),
            known_device_identities: Vec::new(),
            is_stale: false,
            _stale_timestamp: None,
        }
    }

    pub fn upsert_session(&mut self, device_id: Option<&str>, session: Session) {
        let device_id = device_id.unwrap_or("unknown").to_string();

        let device = self
            .device_records
            .entry(device_id.clone())
            .or_insert_with(|| DeviceRecord {
                device_id: device_id.clone(),
                public_key: String::new(),
                active_session: None,
                inactive_sessions: Vec::new(),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                ),
            });

        if Self::device_contains_session_state(device, &session.state) {
            session.close();
            Self::compact_duplicate_sessions(device);
            device.last_activity = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
            return;
        }

        let new_priority = Self::session_priority(&session);
        let old_priority = device
            .active_session
            .as_ref()
            .map(Self::session_priority)
            .unwrap_or((0, 0, 0));

        if let Some(old_session) = device.active_session.take() {
            // Keep the richer session active for this device. In practice this avoids
            // clobbering a bidirectional session with a newer one-way bootstrap/import
            // session that can send but cannot yet receive.
            if old_priority >= new_priority {
                device.inactive_sessions.push(session);
                device.active_session = Some(old_session);
            } else {
                device.inactive_sessions.push(old_session);
                device.active_session = Some(session);
            }
        } else {
            device.active_session = Some(session);
        }

        Self::compact_duplicate_sessions(device);

        const MAX_INACTIVE: usize = 10;
        if device.inactive_sessions.len() > MAX_INACTIVE {
            device.inactive_sessions.truncate(MAX_INACTIVE);
        }

        device.last_activity = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    }

    fn session_priority(session: &Session) -> (u8, u32, u32) {
        let can_send = session.can_send();
        let can_receive = session.state.receiving_chain_key.is_some()
            || session.state.their_current_nostr_public_key.is_some()
            || session.state.receiving_chain_message_number > 0;

        let directionality = match (can_send, can_receive) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        };

        (
            directionality,
            session.state.receiving_chain_message_number,
            session.state.sending_chain_message_number,
        )
    }

    fn device_contains_session_state(device: &DeviceRecord, state: &SessionState) -> bool {
        device
            .active_session
            .as_ref()
            .is_some_and(|session| session.state == *state)
            || device
                .inactive_sessions
                .iter()
                .any(|session| session.state == *state)
    }

    pub(crate) fn compact_duplicate_sessions(device: &mut DeviceRecord) {
        let active_state = device
            .active_session
            .as_ref()
            .map(|session| session.state.clone());
        let mut unique_states = Vec::new();
        let mut inactive_sessions = Vec::with_capacity(device.inactive_sessions.len());

        for session in device.inactive_sessions.drain(..) {
            let is_duplicate = active_state
                .as_ref()
                .is_some_and(|state| *state == session.state)
                || unique_states
                    .iter()
                    .any(|state: &SessionState| *state == session.state);

            if is_duplicate {
                session.close();
                continue;
            }

            unique_states.push(session.state.clone());
            inactive_sessions.push(session);
        }

        device.inactive_sessions = inactive_sessions;
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

        sessions.sort_by(|a, b| Self::session_priority(b).cmp(&Self::session_priority(a)));

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
                    created_at: d.created_at,
                    is_stale: d.is_stale,
                    stale_timestamp: d.stale_timestamp,
                    last_activity: d.last_activity,
                })
                .collect(),
            known_device_identities: self.known_device_identities.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct StoredUserRecord {
    pub user_id: String,
    pub devices: Vec<StoredDeviceRecord>,
    #[serde(default)]
    pub known_device_identities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SerializableKeyPair;
    use nostr::Keys;

    fn make_session(
        can_send: bool,
        can_receive: bool,
        receiving_chain_message_number: u32,
        sending_chain_message_number: u32,
    ) -> Session {
        let our_current = Keys::generate();
        let our_next = Keys::generate();
        let their_current = Keys::generate();
        let their_next = Keys::generate();

        Session::new(
            SessionState {
                root_key: [1u8; 32],
                their_current_nostr_public_key: can_receive.then(|| their_current.public_key()),
                their_next_nostr_public_key: can_send
                    .then(|| their_next.public_key())
                    .or_else(|| can_receive.then(|| their_current.public_key())),
                our_current_nostr_key: can_send.then(|| SerializableKeyPair {
                    public_key: our_current.public_key(),
                    private_key: our_current.secret_key().to_secret_bytes(),
                }),
                our_next_nostr_key: SerializableKeyPair {
                    public_key: our_next.public_key(),
                    private_key: our_next.secret_key().to_secret_bytes(),
                },
                receiving_chain_key: can_receive.then_some([2u8; 32]),
                sending_chain_key: can_send.then_some([3u8; 32]),
                sending_chain_message_number: sending_chain_message_number,
                receiving_chain_message_number: receiving_chain_message_number,
                previous_sending_chain_message_count: 0,
                skipped_keys: HashMap::new(),
            },
            "test".to_string(),
        )
    }

    #[test]
    fn upsert_session_keeps_bidirectional_session_active_over_send_only() {
        let mut user = UserRecord::new("peer".to_string());
        let bidirectional = make_session(true, true, 1, 0);
        let bidirectional_state = bidirectional.state.clone();
        let send_only = make_session(true, false, 0, 0);
        let send_only_state = send_only.state.clone();

        user.upsert_session(Some("device-a"), bidirectional);
        user.upsert_session(Some("device-a"), send_only);

        let device = user.device_records.get("device-a").expect("device");
        assert_eq!(
            device
                .active_session
                .as_ref()
                .map(|session| session.state.clone()),
            Some(bidirectional_state),
        );
        assert_eq!(device.inactive_sessions.len(), 1);
        assert_eq!(device.inactive_sessions[0].state, send_only_state);
    }

    #[test]
    fn upsert_session_promotes_bidirectional_session_over_send_only() {
        let mut user = UserRecord::new("peer".to_string());
        let send_only = make_session(true, false, 0, 0);
        let send_only_state = send_only.state.clone();
        let bidirectional = make_session(true, true, 2, 1);
        let bidirectional_state = bidirectional.state.clone();

        user.upsert_session(Some("device-a"), send_only);
        user.upsert_session(Some("device-a"), bidirectional);

        let device = user.device_records.get("device-a").expect("device");
        assert_eq!(
            device
                .active_session
                .as_ref()
                .map(|session| session.state.clone()),
            Some(bidirectional_state),
        );
        assert_eq!(device.inactive_sessions.len(), 1);
        assert_eq!(device.inactive_sessions[0].state, send_only_state);
    }
}
