use anyhow::Result;
use nostr_double_ratchet::{
    Invite, NdrRuntime, SessionManagerEvent, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND,
};

use crate::commands::owner_claim::fetch_latest_app_keys_snapshot;
use crate::commands::runtime_support::build_runtime;
use crate::commands::session_delivery::{
    is_double_ratchet_invite_event, is_double_ratchet_public_invite_event,
};
use crate::config::Config;
use crate::nostr_client::{connect_client, fetch_events_best_effort, send_event_or_ignore};
use crate::storage::{Storage, StoredChat};

pub(super) struct PublicInviteJoinResult {
    pub chat: StoredChat,
    pub response_event: Option<nostr::Event>,
}

fn upsert_chat_from_runtime(
    storage: &Storage,
    runtime: &NdrRuntime,
    owner_pubkey: nostr::PublicKey,
    device_id: String,
) -> Result<StoredChat> {
    let owner_hex = owner_pubkey.to_hex();
    let session_state = runtime
        .export_active_sessions()
        .into_iter()
        .find_map(|(owner, device, state)| {
            (owner == owner_pubkey && device == device_id).then_some(state)
        })
        .or(runtime.export_active_session_state(owner_pubkey)?)
        .ok_or_else(|| anyhow::anyhow!("NdrRuntime did not expose active session"))?;
    let session_state_json = serde_json::to_string(&session_state)?;

    if let Some(mut existing_chat) = storage.get_chat_by_pubkey(&owner_hex)? {
        existing_chat.device_id = Some(device_id);
        existing_chat.session_state = session_state_json;
        storage.save_chat(&existing_chat)?;
        return Ok(existing_chat);
    }

    let chat = StoredChat {
        id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
        their_pubkey: owner_hex,
        device_id: Some(device_id),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state: session_state_json,
        message_ttl_seconds: None,
    };

    storage.save_chat(&chat)?;
    Ok(chat)
}

async fn flush_session_manager_events(
    runtime: &NdrRuntime,
    signing_keys: &nostr::Keys,
    client: &nostr_sdk::Client,
) -> Result<Option<nostr::Event>> {
    let mut invite_response: Option<nostr::Event> = None;

    for event in runtime.drain_events() {
        match event {
            SessionManagerEvent::Publish(unsigned) => {
                let signed = unsigned.sign_with_keys(signing_keys)?;
                if is_double_ratchet_invite_event(&signed) {
                    continue;
                }
                if signed.kind.as_u16() == INVITE_RESPONSE_KIND as u16 && invite_response.is_none()
                {
                    invite_response = Some(signed.clone());
                }
                send_event_or_ignore(client, signed).await?;
            }
            SessionManagerEvent::PublishSigned(signed) => {
                if is_double_ratchet_invite_event(&signed) {
                    continue;
                }
                if signed.kind.as_u16() == INVITE_RESPONSE_KIND as u16 && invite_response.is_none()
                {
                    invite_response = Some(signed.clone());
                }
                send_event_or_ignore(client, signed).await?;
            }
            SessionManagerEvent::PublishSignedForInnerEvent { event, .. } => {
                if is_double_ratchet_invite_event(&event) {
                    continue;
                }
                if event.kind.as_u16() == INVITE_RESPONSE_KIND as u16 && invite_response.is_none() {
                    invite_response = Some(event.clone());
                }
                send_event_or_ignore(client, event).await?;
            }
            SessionManagerEvent::Subscribe { .. }
            | SessionManagerEvent::Unsubscribe(_)
            | SessionManagerEvent::ReceivedEvent(_)
            | SessionManagerEvent::DecryptedMessage { .. } => {}
        }
    }

    Ok(invite_response)
}

pub(super) async fn join_via_invite(
    invite: Invite,
    config: &Config,
    storage: &Storage,
) -> Result<PublicInviteJoinResult> {
    let (runtime, signing_keys, _owner_pubkey_hex) = build_runtime(config, storage)?;

    let client = connect_client(config).await?;
    if let Some(claimed_owner_pubkey) = invite.owner_public_key {
        if claimed_owner_pubkey != invite.inviter {
            if let Some(snapshot) = fetch_latest_app_keys_snapshot(
                &client,
                &config.resolved_relays(),
                claimed_owner_pubkey,
            )
            .await?
            {
                runtime.ingest_app_keys_snapshot(
                    claimed_owner_pubkey,
                    snapshot.app_keys,
                    snapshot.created_at,
                );
            }
        }
    }

    let accepted = runtime.accept_invite(&invite, invite.owner_public_key)?;
    let response_event = flush_session_manager_events(&runtime, &signing_keys, &client).await?;

    let chat =
        upsert_chat_from_runtime(storage, &runtime, accepted.owner_pubkey, accepted.device_id)?;

    Ok(PublicInviteJoinResult {
        chat,
        response_event,
    })
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

    let client = connect_client(config).await?;

    let filter = Filter::new()
        .kind(nostr::Kind::Custom(INVITE_EVENT_KIND as u16))
        .author(target_pubkey)
        .limit(20);

    let events = fetch_events_best_effort(
        &client,
        &config.resolved_relays(),
        filter,
        Duration::from_secs(3),
    )
    .await?;

    let public_invite = events.iter().find_map(|event| {
        if is_double_ratchet_public_invite_event(event) {
            Invite::from_event(event).ok()
        } else {
            None
        }
    });

    let invite = public_invite
        .ok_or_else(|| anyhow::anyhow!("No public invite found for {}", target_pubkey_hex))?;

    drop(client);
    join_via_invite(invite, config, storage).await
}
