use super::*;

impl SessionManager {
    pub fn accept_invite(
        &self,
        invite: &Invite,
        owner_pubkey_hint: Option<PublicKey>,
    ) -> Result<AcceptInviteResult> {
        let inviter_device_pubkey = invite.inviter;
        if inviter_device_pubkey == self.our_public_key {
            return Err(crate::Error::Invite(
                "Cannot accept invite from this device".to_string(),
            ));
        }

        let explicit_same_device_owner_hint =
            owner_pubkey_hint.is_some_and(|hint| hint == inviter_device_pubkey);
        let claimed_owner_pubkey = owner_pubkey_hint
            .or(invite.owner_public_key)
            .unwrap_or_else(|| self.resolve_to_owner(&inviter_device_pubkey));
        let mut owner_pubkey = claimed_owner_pubkey;
        let mut used_link_bootstrap_exception = false;

        if claimed_owner_pubkey != inviter_device_pubkey {
            let cached_app_keys = self
                .cached_app_keys
                .lock()
                .unwrap()
                .get(&claimed_owner_pubkey)
                .cloned();
            if let Some(app_keys) = cached_app_keys {
                let routing = resolve_invite_owner_routing(
                    inviter_device_pubkey,
                    claimed_owner_pubkey,
                    invite.purpose.as_deref(),
                    self.owner_public_key,
                    Some(&app_keys),
                );
                owner_pubkey = routing.owner_pubkey;
                used_link_bootstrap_exception = routing.used_link_bootstrap_exception;
                if owner_pubkey == claimed_owner_pubkey {
                    self.update_delegate_mapping(claimed_owner_pubkey, &app_keys);
                }
            } else {
                let known_device_identities = self.with_user_records(move |records| {
                    records
                        .get(&claimed_owner_pubkey)
                        .map(|record| record.known_device_identities.clone())
                        .unwrap_or_default()
                });

                let stored_app_keys = (!known_device_identities.is_empty()).then(|| {
                    AppKeys::new(
                        known_device_identities
                            .iter()
                            .filter_map(|identity_hex| {
                                crate::utils::pubkey_from_hex(identity_hex)
                                    .ok()
                                    .map(|pubkey| DeviceEntry::new(pubkey, 0))
                            })
                            .collect(),
                    )
                });
                let routing = resolve_invite_owner_routing(
                    inviter_device_pubkey,
                    claimed_owner_pubkey,
                    invite.purpose.as_deref(),
                    self.owner_public_key,
                    stored_app_keys.as_ref(),
                );
                owner_pubkey = routing.owner_pubkey;
                used_link_bootstrap_exception = routing.used_link_bootstrap_exception;
                if owner_pubkey == claimed_owner_pubkey {
                    if let Some(app_keys) = stored_app_keys.as_ref() {
                        self.update_delegate_mapping(claimed_owner_pubkey, app_keys);
                    }
                }
            }
        }

        let device_id = invite
            .device_id
            .clone()
            .unwrap_or_else(|| hex::encode(inviter_device_pubkey.to_bytes()));

        let existing_device_session_info = self.with_user_records({
            let device_id = device_id.clone();
            move |records| {
                let device_record = records
                    .get(&owner_pubkey)
                    .and_then(|r| r.device_records.get(&device_id))?;

                let active_session = device_record.active_session.as_ref().map(|session| {
                    (
                        session.can_send(),
                        SessionManager::session_can_receive(session),
                        session.state.sending_chain_message_number,
                        session.state.receiving_chain_message_number,
                    )
                });
                let has_any_session =
                    active_session.is_some() || !device_record.inactive_sessions.is_empty();

                let mut any_send_capable = active_session
                    .as_ref()
                    .is_some_and(|(can_send, _, _, _)| *can_send);
                let mut any_receive_capable = active_session
                    .as_ref()
                    .is_some_and(|(_, can_receive, _, _)| *can_receive);
                let mut any_session_has_activity = active_session.as_ref().is_some_and(
                    |(_, _, sent_messages, received_messages)| {
                        *sent_messages > 0 || *received_messages > 0
                    },
                );

                for session in &device_record.inactive_sessions {
                    if session.can_send() {
                        any_send_capable = true;
                    }
                    if SessionManager::session_can_receive(session) {
                        any_receive_capable = true;
                    }
                    if session.state.sending_chain_message_number > 0
                        || session.state.receiving_chain_message_number > 0
                    {
                        any_session_has_activity = true;
                    }
                }

                Some((
                    active_session,
                    has_any_session,
                    any_send_capable,
                    any_receive_capable,
                    any_session_has_activity,
                ))
            }
        });
        if existing_device_session_info.is_some_and(
            |(_, _, any_send_capable, any_receive_capable, any_session_has_activity)| {
                any_send_capable && (any_receive_capable || any_session_has_activity)
            },
        ) {
            self.record_known_device_identity(owner_pubkey, inviter_device_pubkey);
            return Ok(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: false,
            });
        }
        if explicit_same_device_owner_hint
            && invite.purpose.as_deref() != Some("link")
            && existing_device_session_info.is_some_and(
                |(
                    _,
                    has_any_session,
                    any_send_capable,
                    any_receive_capable,
                    any_session_has_activity,
                )| {
                    has_any_session
                        && !any_send_capable
                        && !any_receive_capable
                        && !any_session_has_activity
                },
            )
        {
            self.record_known_device_identity(owner_pubkey, inviter_device_pubkey);
            return Ok(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: false,
            });
        }
        let replace_existing_active_session = existing_device_session_info.is_some_and(
            |(active_session, _, _, _, any_session_has_activity)| {
                active_session.is_some_and(
                    |(can_send, can_receive, sent_messages, received_messages)| {
                        can_send
                            && !can_receive
                            && sent_messages == 0
                            && received_messages == 0
                            && !any_session_has_activity
                    },
                )
            },
        );
        let replace_receive_only_active_session =
            existing_device_session_info.is_some_and(|(active_session, _, _, _, _)| {
                active_session.is_some_and(
                    |(can_send, can_receive, sent_messages, received_messages)| {
                        !can_send && can_receive && sent_messages == 0 && received_messages == 0
                    },
                )
            });

        let replace_existing_active_session =
            replace_existing_active_session || replace_receive_only_active_session;

        {
            let mut pending = self.pending_acceptances.lock().unwrap();
            if pending.contains(&inviter_device_pubkey) {
                return Err(crate::Error::Invite(
                    "Invite acceptance already in progress".to_string(),
                ));
            }
            pending.insert(inviter_device_pubkey);
        }

        let result = (|| -> Result<AcceptInviteResult> {
            let (mut session, response_event) = invite.accept_with_owner(
                self.our_public_key,
                self.our_identity_key,
                Some(self.device_id.clone()),
                Some(self.owner_public_key),
            )?;

            self.pubsub.publish_signed(response_event)?;

            let invite_bootstrap_messages = self.build_bootstrap_messages(owner_pubkey);
            let invite_bootstrap_events =
                SessionManager::sign_bootstrap_schedule(&mut session, &invite_bootstrap_messages);

            self.with_user_records({
                let device_id = device_id.clone();
                move |records| {
                    let user_record = records
                        .entry(owner_pubkey)
                        .or_insert_with(|| UserRecord::new(hex::encode(owner_pubkey.to_bytes())));
                    SessionManager::upsert_device_record(user_record, &device_id);

                    if replace_existing_active_session {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let device_record = user_record
                            .device_records
                            .get_mut(&device_id)
                            .expect("device record should exist");

                        if let Some(active) = device_record.active_session.take() {
                            device_record.inactive_sessions.insert(0, active);
                        }
                        device_record.active_session = Some(session);
                        crate::UserRecord::compact_duplicate_sessions(device_record);

                        const MAX_INACTIVE: usize = 10;
                        if device_record.inactive_sessions.len() > MAX_INACTIVE {
                            device_record.inactive_sessions.truncate(MAX_INACTIVE);
                        }
                        device_record.last_activity = Some(now);
                    } else {
                        // Preserve an already-used active session so repeated invite replays
                        // don't clobber the established sending/receiving path for this device.
                        user_record.upsert_session(Some(&device_id), session);
                    }
                }
            });

            self.record_known_device_identity(owner_pubkey, inviter_device_pubkey);
            let _ = self.store_user_record(&owner_pubkey);
            self.send_message_history(owner_pubkey, &device_id);
            if !invite_bootstrap_events.is_empty() {
                self.publish_bootstrap_schedule(invite_bootstrap_events);
            }
            if used_link_bootstrap_exception {
                self.send_link_bootstrap(owner_pubkey, &device_id);
            }
            let _ = self.flush_message_queue(&device_id);

            Ok(AcceptInviteResult {
                owner_pubkey,
                inviter_device_pubkey,
                device_id,
                created_new_session: true,
            })
        })();

        self.pending_acceptances
            .lock()
            .unwrap()
            .remove(&inviter_device_pubkey);

        result
    }
}
