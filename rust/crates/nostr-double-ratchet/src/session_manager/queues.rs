use super::*;

impl SessionManager {
    pub fn queued_message_diagnostics(
        &self,
        inner_event_id: Option<&str>,
    ) -> Result<Vec<crate::QueuedMessageDiagnostic>> {
        let mut out = Vec::new();
        let device_to_owner = self.with_user_records(|records| {
            records
                .iter()
                .flat_map(|(owner, record)| {
                    record
                        .device_records
                        .keys()
                        .chain(record.known_device_identities.iter())
                        .map(|device_id| (device_id.clone(), *owner))
                        .collect::<Vec<_>>()
                })
                .collect::<HashMap<_, _>>()
        });

        for entry in self.discovery_queue.entries()? {
            let entry_inner_id = entry.event.id.as_ref().map(ToString::to_string);
            if inner_event_id.is_some_and(|id| entry_inner_id.as_deref() != Some(id)) {
                continue;
            }
            out.push(crate::QueuedMessageDiagnostic {
                stage: crate::QueuedMessageStage::Discovery,
                owner_pubkey: crate::utils::pubkey_from_hex(&entry.target_key).ok(),
                target_key: entry.target_key,
                inner_event_id: entry_inner_id,
                created_at_ms: entry.created_at,
            });
        }

        for entry in self.message_queue.entries()? {
            let entry_inner_id = entry.event.id.as_ref().map(ToString::to_string);
            if inner_event_id.is_some_and(|id| entry_inner_id.as_deref() != Some(id)) {
                continue;
            }
            out.push(crate::QueuedMessageDiagnostic {
                stage: crate::QueuedMessageStage::Device,
                owner_pubkey: device_to_owner.get(&entry.target_key).copied(),
                target_key: entry.target_key,
                inner_event_id: entry_inner_id,
                created_at_ms: entry.created_at,
            });
        }

        out.sort_by_key(|entry| entry.created_at_ms);
        Ok(out)
    }

    pub(super) fn flush_message_queue(&self, device_identity: &str) -> Result<()> {
        let entries = self.message_queue.get_for_target(device_identity)?;
        if entries.is_empty() {
            return Ok(());
        }

        let owner_pubkey = self.with_user_records({
            let device_identity = device_identity.to_string();
            move |records| {
                records.iter().find_map(|(owner, user_record)| {
                    user_record
                        .device_records
                        .contains_key(&device_identity)
                        .then_some(*owner)
                })
            }
        });
        let Some(owner_pubkey) = owner_pubkey else {
            return Ok(());
        };

        let mut sent: Vec<(String, Option<String>)> = Vec::new();
        let pending_publishes = self.with_user_records({
            let device_identity = device_identity.to_string();
            let entries = entries.clone();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_identity)
                else {
                    return Vec::new();
                };

                let mut pending = Vec::new();
                for entry in entries {
                    let maybe_event_id = entry
                        .event
                        .id
                        .as_ref()
                        .map(|id| id.to_string())
                        .or_else(|| entry.id.split('/').next().map(str::to_string));
                    if let Some(signed_event) =
                        Self::send_event_with_best_session(device_record, entry.event)
                    {
                        pending.push((entry.id, maybe_event_id, signed_event));
                    }
                }
                pending
            }
        });

        for (entry_id, maybe_event_id, signed_event) in pending_publishes {
            if self
                .pubsub
                .publish_signed_for_inner_event_to_device(
                    signed_event,
                    maybe_event_id.clone(),
                    Some(device_identity.to_string()),
                )
                .is_ok()
            {
                sent.push((entry_id, maybe_event_id));
            }
        }

        for (entry_id, maybe_event_id) in sent {
            if let Some(event_id) = maybe_event_id {
                let _ = self
                    .message_queue
                    .remove_by_target_and_event_id(device_identity, &event_id);
            } else {
                let _ = self.message_queue.remove(&entry_id);
            }
        }

        let _ = self.store_user_record(&owner_pubkey);
        Ok(())
    }

    pub(super) fn expand_discovery_queue(
        &self,
        owner_pubkey: PublicKey,
        devices: &[DeviceEntry],
    ) -> Result<()> {
        let entries = self
            .discovery_queue
            .get_for_target(&owner_pubkey.to_hex())?;
        if entries.is_empty() {
            return Ok(());
        }

        for entry in entries {
            let mut expanded_for_all_devices = true;
            for device in devices {
                let device_id = device.identity_pubkey.to_hex();
                if device_id == self.device_id {
                    continue;
                }
                if self.message_queue.add(&device_id, &entry.event).is_err() {
                    expanded_for_all_devices = false;
                }
            }

            // Keep discovery entries when any per-device queue write fails so
            // the next AppKeys cycle can retry expansion without message loss.
            if expanded_for_all_devices {
                let _ = self.discovery_queue.remove(&entry.id);
            }
        }

        Ok(())
    }

    pub(super) fn send_message_history(&self, owner_pubkey: PublicKey, device_id: &str) {
        let history = {
            self.message_history
                .lock()
                .unwrap()
                .get(&owner_pubkey)
                .cloned()
                .unwrap_or_default()
        };

        if history.is_empty() {
            return;
        }

        let signed_history = self.with_user_records({
            let device_id = device_id.to_string();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_id) else {
                    return Vec::new();
                };
                let mut signed = Vec::new();
                for event in history {
                    if let Some(signed_event) =
                        Self::send_event_with_best_session(device_record, event)
                    {
                        signed.push(signed_event);
                    }
                }
                signed
            }
        });

        for signed_event in signed_history {
            let _ = self.pubsub.publish_signed(signed_event);
        }
        let _ = self.store_user_record(&owner_pubkey);
    }
}
