use anyhow::Result;
use nostr_double_ratchet::{
    FileStorageAdapter, Invite, SessionManager, SessionManagerEvent, StorageAdapter,
    INVITE_EVENT_KIND, INVITE_RESPONSE_KIND,
};
use std::sync::Arc;

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
use crate::storage::{Storage, StoredChat};

pub(super) struct PublicInviteJoinResult {
    pub chat: StoredChat,
    pub response_event: Option<nostr::Event>,
}

fn build_session_manager(
    config: &Config,
    storage: &Storage,
) -> Result<(
    SessionManager,
    crossbeam_channel::Receiver<SessionManagerEvent>,
    nostr::Keys,
    String,
)> {
    let our_private_key = config.private_key_bytes()?;
    let our_pubkey_hex = config.public_key()?;
    let our_pubkey = nostr::PublicKey::from_hex(&our_pubkey_hex)?;
    let owner_pubkey_hex = config.owner_public_key_hex()?;
    let owner_pubkey = nostr::PublicKey::from_hex(&owner_pubkey_hex)?;

    let session_manager_store: Arc<dyn StorageAdapter> = Arc::new(FileStorageAdapter::new(
        storage.data_dir().join("session_manager"),
    )?);

    let (sm_tx, sm_rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_pubkey,
        our_private_key,
        our_pubkey_hex,
        owner_pubkey,
        sm_tx,
        Some(session_manager_store),
        None,
    );
    manager.init()?;

    let keys = nostr::Keys::new(nostr::SecretKey::from_slice(&our_private_key)?);
    Ok((manager, sm_rx, keys, owner_pubkey_hex))
}

fn import_chats_into_session_manager(
    storage: &Storage,
    manager: &SessionManager,
    my_owner_pubkey_hex: &str,
) -> Result<()> {
    let known: std::collections::HashMap<(String, String), String> = manager
        .export_active_sessions()
        .into_iter()
        .filter_map(|(owner, device_id, state)| {
            serde_json::to_string(&state)
                .ok()
                .map(|json| ((owner.to_hex(), device_id), json))
        })
        .collect();

    for chat in storage.list_chats()? {
        if chat.their_pubkey == my_owner_pubkey_hex {
            continue;
        }

        let owner_pubkey = match nostr::PublicKey::from_hex(&chat.their_pubkey) {
            Ok(pk) => pk,
            Err(_) => continue,
        };
        manager.setup_user(owner_pubkey);

        let device_id = chat.device_id.clone().unwrap_or_else(|| chat.id.clone());
        if known
            .get(&(owner_pubkey.to_hex(), device_id.clone()))
            .is_some_and(|known_state| known_state == &chat.session_state)
        {
            continue;
        }

        let state: nostr_double_ratchet::SessionState =
            match serde_json::from_str(&chat.session_state) {
                Ok(state) => state,
                Err(_) => continue,
            };

        manager.import_session_state(owner_pubkey, Some(device_id), state)?;
    }

    Ok(())
}

fn upsert_chat_from_session_manager(
    storage: &Storage,
    manager: &SessionManager,
    owner_pubkey: nostr::PublicKey,
    device_id: String,
) -> Result<StoredChat> {
    let owner_hex = owner_pubkey.to_hex();
    let session_state = manager
        .export_active_sessions()
        .into_iter()
        .find_map(|(owner, device, state)| {
            (owner == owner_pubkey && device == device_id).then_some(state)
        })
        .or(manager.export_active_session_state(owner_pubkey)?)
        .ok_or_else(|| anyhow::anyhow!("SessionManager did not expose active session"))?;
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
    manager_rx: &crossbeam_channel::Receiver<SessionManagerEvent>,
    signing_keys: &nostr::Keys,
    client: &nostr_sdk::Client,
) -> Result<Option<nostr::Event>> {
    let mut invite_response: Option<nostr::Event> = None;

    while let Ok(event) = manager_rx.try_recv() {
        match event {
            SessionManagerEvent::Publish(unsigned) => {
                let signed = unsigned.sign_with_keys(signing_keys)?;
                if signed.kind.as_u16() == INVITE_RESPONSE_KIND as u16 && invite_response.is_none()
                {
                    invite_response = Some(signed.clone());
                }
                send_event_or_ignore(client, signed).await?;
            }
            SessionManagerEvent::PublishSigned(signed) => {
                if signed.kind.as_u16() == INVITE_RESPONSE_KIND as u16 && invite_response.is_none()
                {
                    invite_response = Some(signed.clone());
                }
                send_event_or_ignore(client, signed).await?;
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
    let (manager, manager_rx, signing_keys, owner_pubkey_hex) =
        build_session_manager(config, storage)?;
    import_chats_into_session_manager(storage, &manager, &owner_pubkey_hex)?;

    let accepted = manager.accept_invite(&invite, invite.owner_public_key)?;

    let client = connect_client(config).await?;
    let response_event = flush_session_manager_events(&manager_rx, &signing_keys, &client).await?;

    let chat = upsert_chat_from_session_manager(
        storage,
        &manager,
        accepted.owner_pubkey,
        accepted.device_id,
    )?;

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

    drop(client);
    join_via_invite(invite, config, storage).await
}
