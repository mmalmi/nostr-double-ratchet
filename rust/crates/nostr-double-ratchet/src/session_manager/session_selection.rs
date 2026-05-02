use super::*;

impl SessionManager {
    pub(super) fn session_can_receive(session: &crate::Session) -> bool {
        Self::session_state_can_receive(&session.state)
    }

    pub(super) fn session_can_send_from_state(state: &crate::SessionState) -> bool {
        state.their_next_nostr_public_key.is_some() && state.our_current_nostr_key.is_some()
    }

    pub(super) fn session_send_priority(
        session: &crate::Session,
        is_active: bool,
    ) -> (u8, u8, u32, u32, u32) {
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

    pub(super) fn session_state_can_receive(state: &crate::SessionState) -> bool {
        state.receiving_chain_key.is_some()
            || state.their_current_nostr_public_key.is_some()
            || state.receiving_chain_message_number > 0
    }

    pub(super) fn session_state_priority(state: &crate::SessionState) -> (u8, u32, u32, u32) {
        let can_send = Self::session_can_send_from_state(state);
        let can_receive = Self::session_state_can_receive(state);

        let directionality = match (can_send, can_receive) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        };

        (
            directionality,
            state.receiving_chain_message_number,
            state.previous_sending_chain_message_count,
            state.sending_chain_message_number,
        )
    }

    pub(super) fn session_state_tracked_sender_pubkeys(
        state: &crate::SessionState,
    ) -> Vec<PublicKey> {
        let mut pubkeys = HashSet::new();
        if let Some(pubkey) = state.their_current_nostr_public_key {
            pubkeys.insert(pubkey);
        }
        if let Some(pubkey) = state.their_next_nostr_public_key {
            pubkeys.insert(pubkey);
        }
        for pubkey in state.skipped_keys.keys() {
            pubkeys.insert(*pubkey);
        }

        let mut pubkeys: Vec<PublicKey> = pubkeys.into_iter().collect();
        pubkeys.sort_by_key(|pubkey| pubkey.to_hex());
        pubkeys
    }

    pub(super) fn message_push_session_snapshots(
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

    pub(super) fn message_push_author_pubkeys_for_records(
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

    pub(super) fn push_unique_session_state(
        sessions: &mut Vec<crate::SessionState>,
        state: crate::SessionState,
    ) {
        if !sessions.contains(&state) {
            sessions.push(state);
        }
    }

    pub(super) fn promote_session_to_active(
        user_record: &mut UserRecord,
        device_id: &str,
        session_index: usize,
    ) {
        let Some(device_record) = user_record.device_records.get_mut(device_id) else {
            return;
        };

        Self::promote_device_record_session_to_active(device_record, session_index);
    }

    pub(super) fn promote_device_record_session_to_active(
        device_record: &mut crate::DeviceRecord,
        session_index: usize,
    ) {
        if session_index >= device_record.inactive_sessions.len() {
            return;
        }

        let session = device_record.inactive_sessions.remove(session_index);
        if let Some(active) = device_record.active_session.take() {
            device_record.inactive_sessions.insert(0, active);
        }
        device_record.active_session = Some(session);

        const MAX_INACTIVE: usize = 10;
        if device_record.inactive_sessions.len() > MAX_INACTIVE {
            device_record.inactive_sessions.truncate(MAX_INACTIVE);
        }
    }

    pub(super) fn send_event_with_best_session(
        device_record: &mut crate::DeviceRecord,
        event: UnsignedEvent,
    ) -> Option<nostr::Event> {
        let mut candidates = Vec::new();
        if let Some(ref session) = device_record.active_session {
            if session.can_send() {
                candidates.push((None, Self::session_send_priority(session, true)));
            }
        }

        for idx in 0..device_record.inactive_sessions.len() {
            let session = &device_record.inactive_sessions[idx];
            if session.can_send() {
                candidates.push((Some(idx), Self::session_send_priority(session, false)));
            }
        }

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        for (session_index, _) in candidates {
            match session_index {
                None => {
                    if let Some(ref mut session) = device_record.active_session {
                        if let Ok(signed_event) = session.send_event(event.clone()) {
                            return Some(signed_event);
                        }
                    }
                }
                Some(idx) => {
                    let signed_event = {
                        let session = &mut device_record.inactive_sessions[idx];
                        session.send_event(event.clone()).ok()
                    };

                    if let Some(signed_event) = signed_event {
                        Self::promote_device_record_session_to_active(device_record, idx);
                        return Some(signed_event);
                    }
                }
            }
        }

        None
    }
}
