use anyhow::Result;
use nostr_double_ratchet::{Invite, INVITE_EVENT_KIND};

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
use crate::storage::{Storage, StoredChat};

pub(super) struct PublicInviteJoinResult {
    pub chat: StoredChat,
    pub response_event: nostr::Event,
}

/// Create a chat by fetching and accepting the peer's most appropriate public invite.
///
/// This is what Iris-style "npub chat links" expect: the link contains only the peer identity,
/// and the joining client fetches the invite event from relays.
pub(super) async fn join_via_public_invite(
    target_pubkey_hex: &str,
    config: &Config,
    storage: &Storage,
) -> Result<PublicInviteJoinResult> {
    use nostr_sdk::Filter;
    use std::time::Duration;

    let target_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(target_pubkey_hex)?;
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;

    let client = connect_client(config).await?;

    let filter = Filter::new()
        .kind(nostr::Kind::Custom(INVITE_EVENT_KIND as u16))
        .author(target_pubkey)
        .limit(20);

    let events = client
        .fetch_events(vec![filter], Some(Duration::from_secs(10)))
        .await?;

    let has_tag = |event: &nostr::Event, name: &str, value: &str| {
        event.tags.iter().any(|t| {
            let parts = t.as_slice();
            parts.first().map(|s| s.as_str()) == Some(name)
                && parts.get(1).map(|s| s.as_str()) == Some(value)
        })
    };

    let public_invite = events.iter().find_map(|event| {
        if has_tag(event, "d", "double-ratchet/invites/public") {
            Invite::from_event(event).ok()
        } else {
            None
        }
    });

    let invite = public_invite
        .or_else(|| {
            events
                .iter()
                .find_map(|event| Invite::from_event(event).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("No public invite found for {}", target_pubkey_hex))?;

    let their_pubkey_hex = invite.inviter.to_hex();
    let (session, response_event) =
        invite.accept_with_owner(our_pubkey, our_private_key, None, Some(owner_pubkey))?;

    let session_state = serde_json::to_string(&session.state)?;
    let chat_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let chat = StoredChat {
        id: chat_id.clone(),
        their_pubkey: their_pubkey_hex,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state,
        message_ttl_seconds: None,
    };

    storage.save_chat(&chat)?;
    send_event_or_ignore(&client, response_event.clone()).await?;

    Ok(PublicInviteJoinResult {
        chat,
        response_event,
    })
}
