use super::*;

pub(super) enum PeerSendOptionsUpdate {
    ClearOverride,
    SetOverride(crate::SendOptions),
    Ignore,
}

impl SessionManager {
    pub(super) fn current_unix_seconds() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub(super) fn expiration_tag_for_options(
        options: &crate::SendOptions,
        now_seconds: u64,
    ) -> Result<Option<Tag>> {
        let Some(expires_at) = crate::utils::resolve_expiration_seconds(options, now_seconds)?
        else {
            return Ok(None);
        };

        Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
            .map(Some)
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
    }

    pub(super) fn append_expiration_tag(
        tags: &mut Vec<Tag>,
        options: &crate::SendOptions,
        now_seconds: u64,
    ) -> Result<()> {
        if let Some(tag) = Self::expiration_tag_for_options(options, now_seconds)? {
            tags.push(tag);
        }
        Ok(())
    }

    pub(super) fn chat_settings_payload(message_ttl_seconds: u64) -> crate::ChatSettingsPayloadV1 {
        crate::ChatSettingsPayloadV1 {
            typ: "chat-settings".to_string(),
            v: 1,
            message_ttl_seconds: Some(message_ttl_seconds),
        }
    }

    pub(super) fn send_options_for_chat_ttl(message_ttl_seconds: u64) -> crate::SendOptions {
        if message_ttl_seconds == 0 {
            crate::SendOptions::default()
        } else {
            crate::SendOptions {
                ttl_seconds: Some(message_ttl_seconds),
                expires_at: None,
            }
        }
    }

    pub(super) fn chat_settings_update_from_payload(
        payload: &serde_json::Value,
    ) -> PeerSendOptionsUpdate {
        let typ = payload.get("type").and_then(|v| v.as_str());
        let v = payload.get("v").and_then(|v| v.as_u64());
        if typ != Some("chat-settings") || v != Some(1) {
            return PeerSendOptionsUpdate::Ignore;
        }

        match payload.get("messageTtlSeconds") {
            None => PeerSendOptionsUpdate::ClearOverride,
            Some(serde_json::Value::Null) => {
                PeerSendOptionsUpdate::SetOverride(crate::SendOptions::default())
            }
            Some(serde_json::Value::Number(n)) => {
                let Some(ttl) = n.as_u64() else {
                    return PeerSendOptionsUpdate::Ignore;
                };
                PeerSendOptionsUpdate::SetOverride(Self::send_options_for_chat_ttl(ttl))
            }
            _ => PeerSendOptionsUpdate::Ignore,
        }
    }
}
