use super::*;

impl SessionManager {
    pub fn process_received_event(&self, event: nostr::Event) {
        if is_app_keys_event(&event) {
            if let Ok(app_keys) = AppKeys::from_event(&event) {
                self.handle_app_keys_event(event.pubkey, app_keys, event.created_at.as_u64());
            }
            return;
        }

        if event.kind.as_u16() == crate::INVITE_RESPONSE_KIND as u16 {
            if self
                .processed_invite_responses
                .lock()
                .unwrap()
                .contains(&event.id.to_string())
            {
                return;
            }

            if let Some(state) = self.invite_state.lock().unwrap().as_ref() {
                match state
                    .invite
                    .process_invite_response(&event, state.our_identity_key)
                {
                    Ok(Some(response)) => {
                        let claimed_owner = response
                            .owner_public_key
                            .unwrap_or_else(|| self.resolve_to_owner(&response.invitee_identity));
                        if !self.install_invite_response_session(event.id.to_string(), response) {
                            self.setup_user(claimed_owner);
                            self.queue_pending_invite_response(event.clone());
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
            return;
        }

        if event.kind.as_u16() == crate::INVITE_EVENT_KIND as u16 {
            if let Ok(invite) = Invite::from_event(&event) {
                let _ = self.accept_invite(&invite, None);
            }
            return;
        }

        if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16 {
            let event_id = Some(event.id.to_string());
            let decrypted = self.with_user_records({
                let event = event.clone();
                move |records| {
                    for (owner_pubkey, user_record) in records.iter_mut() {
                        let device_ids: Vec<String> =
                            user_record.device_records.keys().cloned().collect();

                        for device_id in device_ids {
                            let Some(device_record) =
                                user_record.device_records.get_mut(&device_id)
                            else {
                                continue;
                            };

                            if let Some(ref mut session) = device_record.active_session {
                                if let Ok(Some(plaintext)) = session.receive(&event) {
                                    return Some((*owner_pubkey, plaintext, device_id.clone()));
                                }
                            }

                            for idx in 0..device_record.inactive_sessions.len() {
                                let plaintext_opt = {
                                    let session = &mut device_record.inactive_sessions[idx];
                                    session.receive(&event).ok().flatten()
                                };

                                if let Some(plaintext) = plaintext_opt {
                                    SessionManager::promote_session_to_active(
                                        user_record,
                                        &device_id,
                                        idx,
                                    );
                                    return Some((*owner_pubkey, plaintext, device_id.clone()));
                                }
                            }
                        }
                    }

                    None
                }
            });

            if let Some((owner_pubkey, plaintext, device_id)) = decrypted {
                let sender_device = if let Ok(sender_pk) = crate::utils::pubkey_from_hex(&device_id)
                {
                    let sender_owner = self.resolve_to_owner(&sender_pk);
                    if sender_owner != sender_pk
                        && !self.is_device_authorized(sender_owner, sender_pk)
                    {
                        return;
                    }
                    Some(sender_pk)
                } else {
                    None
                };

                if let Ok(rumor) = serde_json::from_str::<UnsignedEvent>(&plaintext) {
                    self.maybe_auto_adopt_chat_settings(owner_pubkey, &rumor);
                    let _ = self.maybe_handle_group_sender_key_distribution(
                        owner_pubkey,
                        sender_device,
                        &rumor,
                    );
                }

                let _ = self.store_user_record(&owner_pubkey);
                let _ =
                    self.pubsub
                        .decrypted_message(owner_pubkey, sender_device, plaintext, event_id);
                let _ = self.flush_message_queue(&device_id);
            } else if let Some((sender, sender_device, plaintext, event_id)) =
                self.try_decrypt_group_sender_key_outer(&event, None)
            {
                let _ = self
                    .pubsub
                    .decrypted_message(sender, sender_device, plaintext, event_id);
            }
        }
    }
}
