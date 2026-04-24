use super::*;

impl SessionManager {
    pub(super) fn device_invite_key(&self, device_id: &str) -> String {
        format!("device-invite/{}", device_id)
    }

    pub(super) fn send_options_default_key(&self) -> String {
        "send-options/default".to_string()
    }

    pub(super) fn send_options_peer_prefix(&self) -> String {
        "send-options/peer/".to_string()
    }

    pub(super) fn send_options_peer_key(&self, owner_pubkey: &PublicKey) -> String {
        format!(
            "{}{}",
            self.send_options_peer_prefix(),
            hex::encode(owner_pubkey.to_bytes())
        )
    }

    pub(super) fn send_options_group_prefix(&self) -> String {
        "send-options/group/".to_string()
    }

    pub(super) fn send_options_group_key(&self, group_id: &str) -> String {
        format!("{}{}", self.send_options_group_prefix(), group_id)
    }

    pub(super) fn load_send_options(&self) -> Result<()> {
        // Default
        if let Some(data) = self.storage.get(&self.send_options_default_key())? {
            if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                *self.default_send_options.lock().unwrap() = Some(opts);
            }
        }

        // Per-peer
        let peer_keys = self.storage.list(&self.send_options_peer_prefix())?;
        for k in peer_keys {
            let hex_pk = k
                .strip_prefix(&self.send_options_peer_prefix())
                .unwrap_or("");
            if hex_pk.is_empty() {
                continue;
            }
            let Ok(pk) = crate::utils::pubkey_from_hex(hex_pk) else {
                continue;
            };
            if let Some(data) = self.storage.get(&k)? {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                    self.peer_send_options.lock().unwrap().insert(pk, opts);
                }
            }
        }

        // Per-group
        let group_keys = self.storage.list(&self.send_options_group_prefix())?;
        for k in group_keys {
            let group_id = k
                .strip_prefix(&self.send_options_group_prefix())
                .unwrap_or("")
                .to_string();
            if group_id.is_empty() {
                continue;
            }
            if let Some(data) = self.storage.get(&k)? {
                if let Ok(opts) = serde_json::from_str::<crate::SendOptions>(&data) {
                    self.group_send_options
                        .lock()
                        .unwrap()
                        .insert(group_id, opts);
                }
            }
        }

        Ok(())
    }

    pub(super) fn effective_send_options(
        &self,
        recipient_owner: PublicKey,
        group_id: Option<&str>,
        override_options: Option<crate::SendOptions>,
    ) -> crate::SendOptions {
        if let Some(o) = override_options {
            return o;
        }

        if let Some(gid) = group_id {
            if let Some(o) = self.group_send_options.lock().unwrap().get(gid).cloned() {
                return o;
            }
        }

        if let Some(o) = self
            .peer_send_options
            .lock()
            .unwrap()
            .get(&recipient_owner)
            .cloned()
        {
            return o;
        }

        if let Some(o) = self.default_send_options.lock().unwrap().clone() {
            return o;
        }

        crate::SendOptions::default()
    }

    pub(super) fn chat_settings_peer_pubkey(
        &self,
        from_owner_pubkey: PublicKey,
        rumor: &UnsignedEvent,
    ) -> Option<PublicKey> {
        let us = self.owner_public_key;

        // Determine which peer this applies to:
        // - for incoming messages, `from_owner_pubkey` is the peer
        // - for sender-copy sync across our own devices, `["p", <peer>]` indicates the peer
        let recipient_p = rumor.tags.iter().find_map(|t| {
            let v = t.clone().to_vec();
            if v.first().map(|s| s.as_str()) != Some("p") {
                return None;
            }
            let pk_hex = v.get(1)?;
            crate::utils::pubkey_from_hex(pk_hex).ok()
        });

        if let Some(p) = recipient_p {
            if p != us {
                return Some(p);
            }
        }

        if from_owner_pubkey != us {
            return Some(from_owner_pubkey);
        }

        None
    }

    pub(super) fn maybe_auto_adopt_chat_settings(
        &self,
        from_owner_pubkey: PublicKey,
        rumor: &UnsignedEvent,
    ) {
        if !*self.auto_adopt_chat_settings.lock().unwrap() {
            return;
        }

        if rumor.kind.as_u16() != crate::CHAT_SETTINGS_KIND as u16 {
            return;
        }

        let payload = match serde_json::from_str::<serde_json::Value>(&rumor.content) {
            Ok(v) => v,
            Err(_) => return,
        };

        let typ = payload.get("type").and_then(|v| v.as_str());
        let v = payload.get("v").and_then(|v| v.as_u64());
        if typ != Some("chat-settings") || v != Some(1) {
            return;
        }

        let Some(peer_pubkey) = self.chat_settings_peer_pubkey(from_owner_pubkey, rumor) else {
            return;
        };

        match payload.get("messageTtlSeconds") {
            // Missing: clear per-peer override (fall back to global default).
            None => {
                let _ = self.set_peer_send_options(peer_pubkey, None);
            }
            // Null: disable per-peer expiration (even if a global default exists).
            Some(serde_json::Value::Null) => {
                let _ =
                    self.set_peer_send_options(peer_pubkey, Some(crate::SendOptions::default()));
            }
            Some(serde_json::Value::Number(n)) => {
                let Some(ttl) = n.as_u64() else {
                    return;
                };
                let opts = if ttl == 0 {
                    crate::SendOptions::default()
                } else {
                    crate::SendOptions {
                        ttl_seconds: Some(ttl),
                        expires_at: None,
                    }
                };
                let _ = self.set_peer_send_options(peer_pubkey, Some(opts));
            }
            _ => {}
        }
    }

    pub(super) fn user_record_key(&self, pubkey: &PublicKey) -> String {
        format!("user/{}", hex::encode(pubkey.to_bytes()))
    }

    pub(super) fn user_record_key_prefix(&self) -> String {
        "user/".to_string()
    }

    pub(super) fn group_sender_event_info_prefix(&self) -> String {
        "group-sender-key/sender-event/".to_string()
    }

    pub(super) fn group_sender_event_info_key(&self, sender_event_pubkey: &PublicKey) -> String {
        format!(
            "{}{}",
            self.group_sender_event_info_prefix(),
            hex::encode(sender_event_pubkey.to_bytes())
        )
    }
}
