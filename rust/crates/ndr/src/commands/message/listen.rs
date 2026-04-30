#![allow(clippy::needless_borrow, clippy::too_many_arguments)]

use anyhow::{Context, Result};

use nostr_double_ratchet::{
    group::GroupData, GroupDecryptedEvent, NdrRuntime, Session, SessionManagerEvent,
    CHAT_MESSAGE_KIND, CHAT_SETTINGS_KIND, GROUP_METADATA_KIND, REACTION_KIND, RECEIPT_KIND,
    TYPING_KIND,
};

use crate::commands::owner_claim::{
    fetch_latest_app_keys, fetch_latest_app_keys_snapshot,
    fetch_latest_app_keys_snapshot_with_timeout, resolve_verified_owner_pubkey,
};
use crate::commands::runtime_support::{build_runtime, sync_chats_from_runtime};
use crate::commands::session_delivery::is_double_ratchet_invite_event;
use crate::config::Config;
use crate::nostr_client::{
    fetch_events_best_effort, send_event_or_ignore, subscribe_filters_best_effort,
};
use crate::output::Output;
use crate::state_sync::{
    apply_chat_settings, apply_group_metadata, extract_control_stamp_from_value,
    GroupMetadataApplyOutcome,
};
use crate::storage::{Storage, StoredChat, StoredGroupMessage, StoredMessage, StoredReaction};

use super::common::{
    collect_chat_pubkeys, extract_e_tag, extract_e_tags, extract_expiration_tag_seconds,
    is_expired, parse_chat_settings_ttl_seconds,
};
use super::types::{IncomingMessage, IncomingReaction};

const PEER_APP_KEYS_REFRESH_INTERVAL_MS: u64 = 2_000;
const MAX_PENDING_SESSION_MANAGER_MESSAGE_EVENTS: usize = 256;
const MAX_SEEN_EVENT_IDS: usize = 20_000;

fn build_runtime_context(
    config: &Config,
    storage: &Storage,
) -> Result<(
    NdrRuntime,
    String,
    nostr::PublicKey,
    String,
    nostr::PublicKey,
)> {
    let (runtime, _signing_keys, owner_pubkey_hex) = build_runtime(config, storage)?;
    let our_pubkey = runtime.get_our_pubkey();
    let our_pubkey_hex = our_pubkey.to_hex();
    let owner_pubkey = runtime.get_owner_pubkey();

    Ok((
        runtime,
        our_pubkey_hex,
        our_pubkey,
        owner_pubkey_hex,
        owner_pubkey,
    ))
}

fn collect_chat_pubkeys_with_runtime(
    storage: &Storage,
    runtime: &NdrRuntime,
    chat_id: Option<&str>,
) -> Result<Vec<nostr::PublicKey>> {
    let mut seen = std::collections::HashSet::new();
    let mut pubkeys = Vec::new();
    for pubkey in collect_chat_pubkeys(storage, chat_id)? {
        if seen.insert(pubkey.to_hex()) {
            pubkeys.push(pubkey);
        }
    }

    for pubkey in runtime.get_all_message_push_author_pubkeys() {
        if seen.insert(pubkey.to_hex()) {
            pubkeys.push(pubkey);
        }
    }

    Ok(pubkeys)
}

async fn refresh_peer_app_keys_snapshots(
    storage: &Storage,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
    my_owner_pubkey_hex: &str,
    chat_id: Option<&str>,
) -> Result<()> {
    const APP_KEYS_POLL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
    const DEVICE_INVITE_POLL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    let chats = if let Some(id) = chat_id {
        vec![storage
            .get_chat(id)?
            .ok_or_else(|| anyhow::anyhow!("Chat not found: {}", id))?]
    } else {
        storage.list_chats()?
    };

    let mut seen_owners = std::collections::HashSet::new();
    for chat in chats {
        if chat.their_pubkey == my_owner_pubkey_hex
            || !seen_owners.insert(chat.their_pubkey.clone())
        {
            continue;
        }

        let Ok(owner_pubkey) = nostr::PublicKey::from_hex(&chat.their_pubkey) else {
            continue;
        };

        if let Some(snapshot) = fetch_latest_app_keys_snapshot_with_timeout(
            client,
            relays,
            owner_pubkey,
            APP_KEYS_POLL_FETCH_TIMEOUT,
        )
        .await?
        {
            let sibling_devices: Vec<nostr::PublicKey> = snapshot
                .app_keys
                .get_all_devices()
                .into_iter()
                .map(|device| device.identity_pubkey)
                .filter(|device_pubkey| *device_pubkey != owner_pubkey)
                .collect();
            runtime.ingest_app_keys_snapshot(owner_pubkey, snapshot.app_keys, snapshot.created_at);

            if sibling_devices.is_empty() {
                continue;
            }

            let invite_events = fetch_events_best_effort(
                client,
                relays,
                nostr_sdk::Filter::new()
                    .kind(nostr::Kind::Custom(
                        nostr_double_ratchet::INVITE_EVENT_KIND as u16,
                    ))
                    .authors(sibling_devices.clone())
                    .limit(50),
                DEVICE_INVITE_POLL_FETCH_TIMEOUT,
            )
            .await?;

            for event in invite_events {
                if !sibling_devices.contains(&event.pubkey) {
                    continue;
                }

                let expected_d = format!("double-ratchet/invites/{}", event.pubkey.to_hex());
                let matches_device_invite = event.tags.iter().any(|tag| {
                    let parts = tag.as_slice();
                    parts.first().map(|value| value.as_str()) == Some("d")
                        && parts.get(1).map(|value| value.as_str()) == Some(expected_d.as_str())
                });
                if matches_device_invite {
                    runtime.process_received_event(event);
                }
            }
        }
    }

    Ok(())
}

async fn refresh_pending_invite_response_app_keys(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
) -> Result<()> {
    const APP_KEYS_POLL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    for owner_pubkey in runtime.pending_invite_response_owner_pubkeys() {
        if let Some(snapshot) = fetch_latest_app_keys_snapshot_with_timeout(
            client,
            relays,
            owner_pubkey,
            APP_KEYS_POLL_FETCH_TIMEOUT,
        )
        .await?
        {
            runtime.ingest_app_keys_snapshot(owner_pubkey, snapshot.app_keys, snapshot.created_at);
        }
    }

    Ok(())
}

async fn backfill_recent_pairwise_session_messages(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
    pubkeys: &std::collections::HashSet<String>,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_group_sender_pubkeys: &mut std::collections::HashSet<String>,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
    owner_pubkey_hex: &str,
    nearby: Option<&crate::nearby::NearbyService>,
) -> Result<()> {
    let author_pubkeys: Vec<nostr::PublicKey> = pubkeys
        .iter()
        .filter_map(|hex| nostr::PublicKey::from_hex(hex).ok())
        .collect();
    if author_pubkeys.is_empty() {
        return Ok(());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let filter = nostr_sdk::Filter::new()
        .kind(nostr::Kind::Custom(
            nostr_double_ratchet::MESSAGE_EVENT_KIND as u16,
        ))
        .authors(author_pubkeys)
        .since(nostr::Timestamp::from(now.saturating_sub(3600)));
    let mut events =
        fetch_events_best_effort(client, relays, filter, std::time::Duration::from_secs(3)).await?;
    events.sort_by_key(|event| (event.created_at.as_secs(), event.id.to_hex()));

    let mut handled_any = false;
    for event in events {
        let current_event_id = event.id.to_hex();
        if seen_event_ids.contains(&current_event_id) {
            continue;
        }
        seen_event_ids.insert(current_event_id.clone());
        seen_event_ids_order.push_back(current_event_id.clone());
        if seen_event_ids_order.len() > MAX_SEEN_EVENT_IDS {
            if let Some(old) = seen_event_ids_order.pop_front() {
                seen_event_ids.remove(&old);
            }
        }

        let session_manager_result = process_session_manager_event(
            &event,
            runtime,
            client,
            config,
            storage,
            output,
            subscribed_manager_filters,
            nearby,
        )
        .await?;
        let handled_group_routed = apply_session_group_decrypts(
            &session_manager_result.session_group_decrypts,
            None,
            runtime,
            client,
            relays,
            config,
            storage,
            output,
            subscribed_group_sender_pubkeys,
            seen_event_ids,
            seen_event_ids_order,
        )
        .await?;
        if session_manager_result.handled_any && !session_manager_result.current_event_group_routed
        {
            handled_any = true;
        }
        if handled_group_routed {
            handled_any = true;
        }
        if session_manager_result.handled_current
            || session_manager_result.current_event_group_routed
        {
            continue;
        }

        let is_pairwise_session_message =
            event.kind.as_u16() as u32 == nostr_double_ratchet::MESSAGE_EVENT_KIND
                && event.tags.iter().any(|tag| {
                    tag.as_slice().first().map(|value| value.as_str()) == Some("header")
                });
        if is_pairwise_session_message && !session_manager_result.current_event_group_routed {
            queue_pending_session_manager_message_event(pending_events, pending_order, &event);
        }
    }

    if handled_any {
        sync_chats_from_runtime(storage, runtime, owner_pubkey_hex)?;
    }
    retry_pending_session_manager_message_events(
        pending_events,
        pending_order,
        runtime,
        client,
        config,
        storage,
        output,
        subscribed_group_sender_pubkeys,
        subscribed_manager_filters,
        owner_pubkey_hex,
        seen_event_ids,
        seen_event_ids_order,
        nearby,
    )
    .await?;

    Ok(())
}

fn accepted_runtime_groups(storage: &Storage) -> Result<Vec<GroupData>> {
    let mut groups: Vec<GroupData> = storage
        .list_groups()?
        .into_iter()
        .filter(|group| group.data.accepted == Some(true))
        .map(|group| group.data)
        .collect();
    groups.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(groups)
}

fn sync_runtime_groups(runtime: &NdrRuntime, storage: &Storage) -> Result<()> {
    Ok(runtime.sync_groups(accepted_runtime_groups(storage)?)?)
}

fn refresh_runtime_state_from_storage(
    runtime: &NdrRuntime,
    storage: &Storage,
    owner_pubkey_hex: &str,
) -> Result<()> {
    runtime.reload_from_storage()?;
    sync_chats_from_runtime(storage, runtime, owner_pubkey_hex)?;
    sync_runtime_groups(runtime, storage)?;
    Ok(())
}

async fn apply_session_group_decrypts(
    session_group_decrypts: &[SessionGroupDecrypt],
    skip_event_id: Option<&str>,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_group_sender_pubkeys: &mut std::collections::HashSet<String>,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
) -> Result<bool> {
    use nostr_double_ratchet::GROUP_SENDER_KEY_DISTRIBUTION_KIND;

    let mut handled_any = false;

    for (
        sender_owner_pubkey,
        sender_device_pubkey,
        content_json,
        group_event_id,
        group_timestamp,
    ) in session_group_decrypts
    {
        if skip_event_id.is_some_and(|skip| group_event_id.as_deref() == Some(skip)) {
            continue;
        }

        let decrypted_event: serde_json::Value = match serde_json::from_str(content_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(gid) = extract_group_id_tag(&decrypted_event) else {
            continue;
        };
        let rumor_kind = decrypted_event["kind"]
            .as_u64()
            .unwrap_or(CHAT_MESSAGE_KIND as u64) as u32;
        let content = decrypted_event["content"]
            .as_str()
            .unwrap_or(content_json)
            .to_string();
        let from_pubkey_hex = sender_owner_pubkey.to_hex();
        let timestamp = *group_timestamp;

        if rumor_kind == GROUP_METADATA_KIND {
            if let Some(metadata) = nostr_double_ratchet::group::parse_group_metadata(&content) {
                let my_owner_pubkey = config.owner_public_key_hex()?;
                if let Some(stamp) = extract_control_stamp_from_value(
                    &decrypted_event,
                    group_event_id.as_deref(),
                    timestamp,
                ) {
                    match apply_group_metadata(
                        storage,
                        &gid,
                        &from_pubkey_hex,
                        metadata,
                        stamp,
                        timestamp * 1000,
                        &my_owner_pubkey,
                    )? {
                        GroupMetadataApplyOutcome::Updated { group: updated, .. } => {
                            output.event(
                                "group_metadata",
                                serde_json::json!({
                                    "group_id": gid,
                                    "action": "updated",
                                    "sender_pubkey": from_pubkey_hex,
                                    "name": updated.name,
                                    "members": updated.members,
                                    "admins": updated.admins,
                                }),
                            );
                        }
                        GroupMetadataApplyOutcome::Removed => {
                            output.event(
                                "group_metadata",
                                serde_json::json!({
                                    "group_id": gid,
                                    "action": "removed",
                                    "sender_pubkey": from_pubkey_hex,
                                }),
                            );
                        }
                        GroupMetadataApplyOutcome::Created(group_data) => {
                            output.event(
                                "group_metadata",
                                serde_json::json!({
                                    "group_id": gid,
                                    "action": "created",
                                    "sender_pubkey": from_pubkey_hex,
                                    "name": group_data.name,
                                    "members": group_data.members,
                                    "admins": group_data.admins,
                                }),
                            );
                        }
                        GroupMetadataApplyOutcome::Ignored
                        | GroupMetadataApplyOutcome::Rejected => {}
                    }
                    sync_runtime_groups(runtime, storage)?;
                }
            }
            handled_any = true;
            continue;
        }

        if rumor_kind == GROUP_SENDER_KEY_DISTRIBUTION_KIND {
            let dist =
                match serde_json::from_str::<nostr_double_ratchet::SenderKeyDistribution>(&content)
                {
                    Ok(d) => d,
                    Err(_) => continue,
                };
            if dist.group_id != gid {
                continue;
            }

            let sender_device_pubkey = sender_device_pubkey.unwrap_or(*sender_owner_pubkey);
            let sender_device_pubkey_hex = sender_device_pubkey.to_hex();

            sync_runtime_groups(runtime, storage)?;
            let runtime_event = match serde_json::from_str::<nostr::UnsignedEvent>(content_json) {
                Ok(event) => event,
                Err(_) => continue,
            };
            let drained = runtime.group_handle_incoming_session_event(
                &runtime_event,
                *sender_owner_pubkey,
                Some(sender_device_pubkey),
            );
            for decrypted in drained {
                apply_runtime_group_event(&decrypted, storage, output)?;
            }
            *subscribed_group_sender_pubkeys = sync_group_outer_subscriptions(
                runtime,
                client,
                relays,
                storage,
                output,
                seen_event_ids,
                seen_event_ids_order,
            )
            .await?;

            output.event(
                "group_sender_key",
                serde_json::json!({
                    "group_id": gid,
                    "sender_pubkey": from_pubkey_hex,
                    "sender_device_pubkey": sender_device_pubkey_hex,
                    "key_id": dist.key_id,
                    "iteration": dist.iteration,
                    "sender_event_pubkey": dist.sender_event_pubkey,
                    "transport": "session",
                }),
            );
            handled_any = true;
            continue;
        }

        if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
            let now_seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let expires_at = extract_expiration_tag_seconds(&decrypted_event);
            if is_expired(expires_at, now_seconds) {
                continue;
            }
            let msg_id = fallback_event_id(group_event_id.as_deref(), &decrypted_event);
            storage.save_group_message(&StoredGroupMessage {
                id: msg_id.clone(),
                group_id: gid.clone(),
                sender_pubkey: from_pubkey_hex.clone(),
                content: content.clone(),
                timestamp,
                is_outgoing: false,
                expires_at,
            })?;
            output.event(
                "group_message",
                serde_json::json!({
                    "group_id": gid,
                    "message_id": msg_id,
                    "sender_pubkey": from_pubkey_hex,
                    "sender_device_pubkey": sender_device_pubkey.as_ref().map(|pk| pk.to_hex()),
                    "content": content,
                    "timestamp": timestamp,
                    "transport": "session",
                }),
            );
            handled_any = true;
            continue;
        }

        if rumor_kind == REACTION_KIND {
            output.event(
                "group_reaction",
                serde_json::json!({
                    "group_id": gid,
                    "sender_pubkey": from_pubkey_hex,
                    "message_id": extract_e_tag(&decrypted_event),
                    "emoji": content,
                    "timestamp": timestamp,
                }),
            );
            handled_any = true;
            continue;
        }

        if rumor_kind == TYPING_KIND {
            output.event(
                "group_typing",
                serde_json::json!({
                    "group_id": gid,
                    "sender_pubkey": from_pubkey_hex,
                    "timestamp": timestamp,
                }),
            );
            handled_any = true;
        }
    }

    Ok(handled_any)
}

fn apply_runtime_group_event(
    decrypted: &GroupDecryptedEvent,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let decrypted_event_value = serde_json::to_value(&decrypted.inner)?;
    let rumor_kind = decrypted.inner.kind.as_u16() as u32;
    let sender_pubkey_hex = decrypted
        .sender_owner_pubkey
        .unwrap_or(decrypted.sender_device_pubkey)
        .to_hex();
    let sender_device_pubkey_hex = decrypted.sender_device_pubkey.to_hex();
    let timestamp = decrypted.outer_created_at;
    let content = decrypted.inner.content.clone();

    if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
        let now_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let expires_at = extract_expiration_tag_seconds(&decrypted_event_value);
        if is_expired(expires_at, now_seconds) {
            return Ok(());
        }

        let stored = StoredGroupMessage {
            id: decrypted.outer_event_id.clone(),
            group_id: decrypted.group_id.clone(),
            sender_pubkey: sender_pubkey_hex.clone(),
            content: content.clone(),
            timestamp,
            is_outgoing: false,
            expires_at,
        };
        storage.save_group_message(&stored)?;
        output.event(
            "group_message",
            serde_json::json!({
                "group_id": decrypted.group_id,
                "message_id": decrypted.outer_event_id,
                "sender_pubkey": sender_pubkey_hex,
                "sender_device_pubkey": sender_device_pubkey_hex,
                "content": content,
                "timestamp": timestamp,
            }),
        );
        return Ok(());
    }

    if rumor_kind == REACTION_KIND {
        let message_id = extract_e_tag(&decrypted_event_value);
        output.event(
            "group_reaction",
            serde_json::json!({
                "group_id": decrypted.group_id,
                "sender_pubkey": sender_pubkey_hex,
                "sender_device_pubkey": sender_device_pubkey_hex,
                "message_id": message_id,
                "emoji": content,
                "timestamp": timestamp,
            }),
        );
        return Ok(());
    }

    if rumor_kind == TYPING_KIND {
        output.event(
            "group_typing",
            serde_json::json!({
                "group_id": decrypted.group_id,
                "sender_pubkey": sender_pubkey_hex,
                "sender_device_pubkey": sender_device_pubkey_hex,
                "timestamp": timestamp,
            }),
        );
    }

    Ok(())
}

async fn backfill_recent_group_sender_events(
    client: &nostr_sdk::Client,
    relays: &[String],
    runtime: &NdrRuntime,
    sender_event_pubkey: &nostr::PublicKey,
    storage: &Storage,
    output: &Output,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let filter = nostr_sdk::Filter::new()
        .kind(nostr::Kind::Custom(
            nostr_double_ratchet::MESSAGE_EVENT_KIND as u16,
        ))
        .authors(vec![*sender_event_pubkey])
        .since(nostr::Timestamp::from(now.saturating_sub(3600)));
    let mut events =
        fetch_events_best_effort(client, relays, filter, std::time::Duration::from_secs(3)).await?;
    // Preserve relay order for same-second events. Sender-key messages are sequential, and
    // tie-breaking by event id can reorder a warmup/distribution follow-up pair.
    events.sort_by_key(|event| event.created_at.as_secs());

    for event in events {
        let event_id = event.id.to_hex();
        if !seen_event_ids.contains(&event_id) {
            seen_event_ids.insert(event_id.clone());
            seen_event_ids_order.push_back(event_id);
            if seen_event_ids_order.len() > MAX_SEEN_EVENT_IDS {
                if let Some(old) = seen_event_ids_order.pop_front() {
                    seen_event_ids.remove(&old);
                }
            }
        }

        if let Some(decrypted) = runtime.group_handle_outer_event(&event) {
            apply_runtime_group_event(&decrypted, storage, output)?;
        }
    }

    Ok(())
}

async fn sync_group_outer_subscriptions(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    relays: &[String],
    storage: &Storage,
    output: &Output,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
) -> Result<std::collections::HashSet<String>> {
    use nostr_double_ratchet::MESSAGE_EVENT_KIND;
    use nostr_sdk::Filter;

    let plan = runtime.group_outer_subscription_plan();
    for sender_event_pubkey in &plan.added_authors {
        let sender_filter = Filter::new()
            .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
            .authors(vec![*sender_event_pubkey]);
        subscribe_filters_best_effort(client, relays, vec![sender_filter]).await?;
        backfill_recent_group_sender_events(
            client,
            relays,
            runtime,
            sender_event_pubkey,
            storage,
            output,
            seen_event_ids,
            seen_event_ids_order,
        )
        .await?;
    }

    Ok(plan.authors.into_iter().map(|pk| pk.to_hex()).collect())
}

struct SessionManagerDecrypted {
    sender: nostr::PublicKey,
    sender_device: Option<nostr::PublicKey>,
    content: String,
    event_id: Option<String>,
}

struct SessionManagerProcessingResult {
    handled_any: bool,
    handled_current: bool,
    current_event_group_routed: bool,
    session_group_decrypts: Vec<SessionGroupDecrypt>,
}

type SessionGroupDecrypt = (
    nostr::PublicKey,
    Option<nostr::PublicKey>,
    String,
    Option<String>,
    u64,
);

fn extract_group_id_tag(decrypted_event: &serde_json::Value) -> Option<String> {
    decrypted_event["tags"].as_array().and_then(|tags| {
        tags.iter().find_map(|t| {
            let arr = t.as_array()?;
            if arr.first()?.as_str()? == "l" {
                arr.get(1)?.as_str().map(String::from)
            } else {
                None
            }
        })
    })
}

fn extract_peer_from_p_tag(decrypted_event: &serde_json::Value) -> Option<String> {
    let tags = decrypted_event["tags"].as_array()?;
    for t in tags {
        let arr = t.as_array()?;
        if arr.first()?.as_str()? != "p" {
            continue;
        }
        let pk_hex = arr.get(1)?.as_str()?;
        if let Ok(pk) = nostr::PublicKey::from_hex(pk_hex) {
            return Some(pk.to_hex());
        }
    }
    None
}

fn resolve_chat_for_one_to_one_decrypted(
    sender_owner_hex: &str,
    decrypted_event: &serde_json::Value,
    my_owner_pubkey_hex: &str,
    storage: &Storage,
) -> Result<Option<StoredChat>> {
    let peer_owner_hex = if sender_owner_hex == my_owner_pubkey_hex {
        match extract_peer_from_p_tag(decrypted_event) {
            Some(pk) if pk != my_owner_pubkey_hex => pk,
            _ => return Ok(None),
        }
    } else {
        sender_owner_hex.to_string()
    };

    if let Some(chat) = storage.get_chat_by_pubkey(&peer_owner_hex)? {
        return Ok(Some(chat));
    }

    if sender_owner_hex != my_owner_pubkey_hex {
        return Ok(None);
    }

    let chat = StoredChat {
        id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
        their_pubkey: peer_owner_hex,
        device_id: None,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state: "{}".to_string(),
        message_ttl_seconds: None,
    };
    storage.save_chat(&chat)?;
    Ok(Some(chat))
}

fn fallback_event_id(event_id: Option<&str>, decrypted_event: &serde_json::Value) -> String {
    decrypted_event
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| event_id.map(str::to_string))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn decrypted_content_is_group_routed(content_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content_json)
        .ok()
        .and_then(|event| extract_group_id_tag(&event))
        .is_some()
}

async fn process_session_manager_event(
    event: &nostr::Event,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    nearby: Option<&crate::nearby::NearbyService>,
) -> Result<SessionManagerProcessingResult> {
    let current_event_id = event.id.to_hex();
    let current_timestamp = event.created_at.as_secs();
    runtime.process_received_event(event.clone());
    let decrypted_events =
        flush_session_manager_events(runtime, client, config, subscribed_manager_filters, nearby)
            .await?;
    let session_group_decrypts: Vec<SessionGroupDecrypt> = decrypted_events
        .iter()
        .filter_map(|decrypted| {
            if !decrypted_content_is_group_routed(&decrypted.content) {
                return None;
            }
            let timestamp = if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
                current_timestamp
            } else {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(current_timestamp)
            };
            Some((
                decrypted.sender,
                decrypted.sender_device,
                decrypted.content.clone(),
                decrypted.event_id.clone(),
                timestamp,
            ))
        })
        .collect();
    let current_event_group_routed = session_group_decrypts
        .iter()
        .any(|(_, _, _, event_id, _)| event_id.as_deref() == Some(current_event_id.as_str()));

    let mut handled_any = false;
    let mut handled_current = false;
    for decrypted in decrypted_events {
        let timestamp = if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
            current_timestamp
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        };

        let handled = apply_session_manager_one_to_one_decrypted(
            decrypted.sender,
            &decrypted.content,
            decrypted.event_id.as_deref(),
            timestamp,
            config,
            storage,
            output,
        )?;
        if handled {
            handled_any = true;
            if decrypted.event_id.as_deref() == Some(current_event_id.as_str()) {
                handled_current = true;
            }
        }
    }

    Ok(SessionManagerProcessingResult {
        handled_any,
        handled_current,
        current_event_group_routed,
        session_group_decrypts,
    })
}

fn queue_pending_session_manager_message_event(
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    event: &nostr::Event,
) {
    let event_id = event.id.to_hex();
    if pending_events.contains_key(&event_id) {
        return;
    }

    pending_events.insert(event_id.clone(), event.clone());
    pending_order.push_back(event_id);

    if pending_order.len() > MAX_PENDING_SESSION_MANAGER_MESSAGE_EVENTS {
        if let Some(oldest_id) = pending_order.pop_front() {
            pending_events.remove(&oldest_id);
        }
    }
}

async fn retry_pending_session_manager_message_events(
    pending_events: &mut std::collections::HashMap<String, nostr::Event>,
    pending_order: &mut std::collections::VecDeque<String>,
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    storage: &Storage,
    output: &Output,
    subscribed_group_sender_pubkeys: &mut std::collections::HashSet<String>,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    owner_pubkey_hex: &str,
    seen_event_ids: &mut std::collections::HashSet<String>,
    seen_event_ids_order: &mut std::collections::VecDeque<String>,
    nearby: Option<&crate::nearby::NearbyService>,
) -> Result<()> {
    if pending_events.is_empty() {
        return Ok(());
    }

    let pending_ids: Vec<String> = pending_order.iter().cloned().collect();
    let mut handled_any = false;
    let mut handled_ids: Vec<String> = Vec::new();

    for pending_id in pending_ids {
        let Some(event) = pending_events.get(&pending_id).cloned() else {
            continue;
        };
        let relays = config.resolved_relays();

        let result = process_session_manager_event(
            &event,
            runtime,
            client,
            config,
            storage,
            output,
            subscribed_manager_filters,
            nearby,
        )
        .await?;
        let handled_group_routed = apply_session_group_decrypts(
            &result.session_group_decrypts,
            None,
            runtime,
            client,
            &relays,
            config,
            storage,
            output,
            subscribed_group_sender_pubkeys,
            seen_event_ids,
            seen_event_ids_order,
        )
        .await?;

        if result.handled_any && !result.current_event_group_routed {
            handled_any = true;
        }
        if handled_group_routed {
            handled_any = true;
        }
        if result.handled_current || result.current_event_group_routed {
            handled_ids.push(pending_id);
        }
    }

    for handled_id in handled_ids {
        pending_events.remove(&handled_id);
    }
    *pending_order = pending_order
        .iter()
        .filter(|event_id| pending_events.contains_key(*event_id))
        .cloned()
        .collect();

    if handled_any {
        sync_chats_from_runtime(storage, runtime, owner_pubkey_hex)?;
    }

    Ok(())
}

pub(super) fn apply_session_manager_one_to_one_decrypted(
    sender_owner_pubkey: nostr::PublicKey,
    content_json: &str,
    event_id: Option<&str>,
    timestamp: u64,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<bool> {
    let decrypted_event: serde_json::Value = match serde_json::from_str(content_json) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if extract_group_id_tag(&decrypted_event).is_some() {
        // Group-routed events are handled by the group-specific path.
        return Ok(false);
    }

    let my_owner_pubkey_hex = config.owner_public_key_hex()?;
    let sender_owner_hex = sender_owner_pubkey.to_hex();
    let Some(chat) = resolve_chat_for_one_to_one_decrypted(
        &sender_owner_hex,
        &decrypted_event,
        &my_owner_pubkey_hex,
        storage,
    )?
    else {
        return Ok(false);
    };

    let rumor_kind = decrypted_event["kind"]
        .as_u64()
        .unwrap_or(CHAT_MESSAGE_KIND as u64) as u32;
    let content = decrypted_event["content"]
        .as_str()
        .unwrap_or(content_json)
        .to_string();
    let is_outgoing = sender_owner_hex == my_owner_pubkey_hex;
    let from_pubkey_hex = if is_outgoing {
        my_owner_pubkey_hex.clone()
    } else {
        sender_owner_hex.clone()
    };
    let mut updated_chat = chat.clone();

    if rumor_kind == CHAT_SETTINGS_KIND {
        if let Some(ttl) = parse_chat_settings_ttl_seconds(&content) {
            if let Some(stamp) =
                extract_control_stamp_from_value(&decrypted_event, event_id, timestamp)
            {
                let _ = apply_chat_settings(storage, &mut updated_chat, ttl, &stamp)?;
            }
            storage.save_chat(&updated_chat)?;
            output.event(
                "chat_settings",
                serde_json::json!({
                    "chat_id": updated_chat.id,
                    "from_pubkey": from_pubkey_hex,
                    "message_ttl_seconds": ttl,
                    "timestamp": timestamp,
                }),
            );
            return Ok(true);
        }
        return Ok(false);
    }

    if rumor_kind == RECEIPT_KIND {
        let message_ids: Vec<String> = extract_e_tags(&decrypted_event);
        storage.save_chat(&updated_chat)?;
        output.event(
            "receipt",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "type": content,
                "message_ids": message_ids,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    if rumor_kind == REACTION_KIND {
        let message_id = extract_e_tag(&decrypted_event);
        let reaction_id = fallback_event_id(event_id, &decrypted_event);
        let stored = StoredReaction {
            id: reaction_id,
            chat_id: chat.id.clone(),
            message_id: message_id.clone(),
            from_pubkey: from_pubkey_hex.clone(),
            emoji: content.clone(),
            timestamp,
            is_outgoing,
        };
        storage.save_reaction(&stored)?;
        storage.save_chat(&updated_chat)?;
        output.event(
            "reaction",
            IncomingReaction {
                chat_id: updated_chat.id.clone(),
                from_pubkey: from_pubkey_hex,
                message_id,
                emoji: content,
                timestamp,
            },
        );
        return Ok(true);
    }

    if rumor_kind == TYPING_KIND {
        storage.save_chat(&updated_chat)?;
        output.event(
            "typing",
            serde_json::json!({
                "chat_id": updated_chat.id,
                "from_pubkey": from_pubkey_hex,
                "timestamp": timestamp,
            }),
        );
        return Ok(true);
    }

    let now_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let expires_at = extract_expiration_tag_seconds(&decrypted_event);
    let msg_id = fallback_event_id(event_id, &decrypted_event);

    if !is_expired(expires_at, now_seconds) {
        let stored = StoredMessage {
            id: msg_id.clone(),
            chat_id: chat.id.clone(),
            from_pubkey: from_pubkey_hex.clone(),
            content: content.clone(),
            timestamp,
            is_outgoing,
            expires_at,
        };
        storage.save_message(&stored)?;
        updated_chat.last_message_at = Some(timestamp);
        storage.save_chat(&updated_chat)?;
        output.event(
            "message",
            IncomingMessage {
                chat_id: updated_chat.id.clone(),
                message_id: msg_id,
                from_pubkey: from_pubkey_hex,
                content,
                timestamp,
            },
        );
        return Ok(true);
    }

    storage.save_chat(&updated_chat)?;
    Ok(true)
}

async fn flush_session_manager_events(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    subscribed_manager_filters: &mut std::collections::HashSet<String>,
    nearby: Option<&crate::nearby::NearbyService>,
) -> Result<Vec<SessionManagerDecrypted>> {
    let mut decrypted = Vec::new();

    runtime.sync_direct_message_subscriptions()?;

    for event in runtime.drain_events() {
        match event {
            SessionManagerEvent::Publish(unsigned) => {
                let sk = nostr::SecretKey::from_slice(&config.private_key_bytes()?)?;
                let keys = nostr::Keys::new(sk);
                let signed = unsigned
                    .sign_with_keys(&keys)
                    .map_err(|e| anyhow::anyhow!("Failed to sign SessionManager event: {}", e))?;
                if is_double_ratchet_invite_event(&signed) {
                    continue;
                }
                publish_session_manager_signed_event(client, signed, nearby).await?;
            }
            SessionManagerEvent::PublishSigned(signed) => {
                if is_double_ratchet_invite_event(&signed) {
                    continue;
                }
                publish_session_manager_signed_event(client, signed, nearby).await?;
            }
            SessionManagerEvent::PublishSignedForInnerEvent { event, .. } => {
                if is_double_ratchet_invite_event(&event) {
                    continue;
                }
                publish_session_manager_signed_event(client, event, nearby).await?;
            }
            SessionManagerEvent::Subscribe { filter_json, .. } => {
                if !subscribed_manager_filters.insert(filter_json.clone()) {
                    continue;
                }
                let filter: nostr_sdk::Filter =
                    serde_json::from_str(&filter_json).with_context(|| {
                        format!("Failed to parse SessionManager filter: {}", filter_json)
                    })?;
                let relays = config.resolved_relays();
                subscribe_filters_best_effort(client, &relays, vec![filter]).await?;
            }
            SessionManagerEvent::Unsubscribe(_) => {
                // nostr-sdk Client API does not expose stable per-sub-id unsubscribe in this path.
            }
            SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                event_id,
            } => {
                decrypted.push(SessionManagerDecrypted {
                    sender,
                    sender_device,
                    content,
                    event_id,
                });
            }
            SessionManagerEvent::ReceivedEvent(_) => {}
        }
    }

    Ok(decrypted)
}

async fn publish_session_manager_signed_event(
    client: &nostr_sdk::Client,
    event: nostr::Event,
    nearby: Option<&crate::nearby::NearbyService>,
) -> Result<()> {
    let relay_result = send_event_or_ignore(client, event.clone()).await;
    let mut nearby_delivered = 0;
    if let Some(nearby) = nearby {
        nearby_delivered = nearby.publish_event(&event).await;
    }
    if relay_result.is_err() && nearby_delivered > 0 {
        return Ok(());
    }
    relay_result
}

/// Listen for new messages and invite responses
pub async fn listen(
    chat_id: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    use nostr_double_ratchet::{
        GROUP_INVITE_RUMOR_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND, INVITE_RESPONSE_KIND,
        MESSAGE_EVENT_KIND, SHARED_CHANNEL_KIND,
    };
    use nostr_sdk::{Client, Filter, RelayPoolNotification};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let mut config = config.clone();
    let chat_id_owned = chat_id.map(|s| s.to_string());
    let (runtime, my_pubkey, my_pubkey_key, owner_pubkey_hex, owner_pubkey) =
        build_runtime_context(&config, storage)?;
    // Clean up stale discovery queue entries (older than 24 hours).
    let _ = runtime.cleanup_discovery_queue(24 * 60 * 60 * 1000);

    refresh_runtime_state_from_storage(&runtime, storage, &owner_pubkey_hex)?;
    let our_private_key = config.private_key_bytes()?;

    // Prepare client (don't connect until we have something to subscribe to)
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    let mut connected = false;

    let scope = chat_id
        .map(|id| format!("chat {}", id))
        .unwrap_or_else(|| "all chats".to_string());

    type FilterState = (
        Vec<Filter>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
    );

    // Helper to build SharedChannel map from groups
    let build_channel_map = |storage: &Storage| -> Result<
        HashMap<String, (nostr_double_ratchet::SharedChannel, String)>,
    > {
        let mut channels = HashMap::new();
        for group in storage.list_groups()? {
            if group.data.accepted != Some(true) {
                continue;
            }
            if let Some(ref secret_hex) = group.data.secret {
                if let Ok(secret_bytes) = hex::decode(secret_hex) {
                    if secret_bytes.len() == 32 {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&secret_bytes);
                        if let Ok(channel) = nostr_double_ratchet::SharedChannel::new(&arr) {
                            let pk_hex = channel.public_key().to_hex();
                            channels.insert(pk_hex, (channel, group.data.id.clone()));
                        }
                    }
                }
            }
        }
        Ok(channels)
    };

    // Helper to build filters from current state
    // Limit relay backfill to recent events to avoid replaying entire history on restart.
    let since_timestamp = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        nostr::Timestamp::from(now.saturating_sub(3600))
    };

    let build_filters =
        |storage: &Storage,
         chat_id: Option<&str>,
         channel_map: &HashMap<String, (nostr_double_ratchet::SharedChannel, String)>,
         since: nostr::Timestamp|
         -> Result<FilterState> {
            let pubkeys_to_watch = collect_chat_pubkeys_with_runtime(storage, &runtime, chat_id)?;
            let subscribed_pubkeys: HashSet<String> =
                pubkeys_to_watch.iter().map(|pk| pk.to_hex()).collect();

            let mut filters = Vec::new();

            if !pubkeys_to_watch.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                        .authors(pubkeys_to_watch)
                        .since(since),
                );
            }

            let group_sender_pubkeys = runtime.group_known_sender_event_pubkeys();
            let group_sender_pubkeys_hex: HashSet<String> =
                group_sender_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !group_sender_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                        .authors(group_sender_pubkeys)
                        .since(since),
                );
            }

            let peer_owner_pubkeys: Vec<nostr::PublicKey> = storage
                .list_chats()?
                .into_iter()
                .filter(|chat| {
                    chat_id
                        .map(|selected_chat_id| chat.id == selected_chat_id)
                        .unwrap_or(true)
                })
                .filter_map(|chat| nostr::PublicKey::from_hex(&chat.their_pubkey).ok())
                .collect();
            let peer_app_keys_pubkeys: HashSet<String> =
                peer_owner_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !peer_owner_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(
                            nostr_double_ratchet::APP_KEYS_EVENT_KIND as u16,
                        ))
                        .authors(peer_owner_pubkeys)
                        .since(since),
                );
            }

            let stored_invites = storage.list_invites()?;
            let ephemeral_pubkeys: Vec<nostr::PublicKey> = stored_invites
                .iter()
                .filter_map(|stored| {
                    nostr_double_ratchet::Invite::deserialize(&stored.serialized)
                        .ok()
                        .map(|invite| invite.inviter_ephemeral_public_key)
                })
                .collect();
            let invite_pubkeys: HashSet<String> =
                ephemeral_pubkeys.iter().map(|pk| pk.to_hex()).collect();

            if !ephemeral_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16))
                        .pubkeys(ephemeral_pubkeys), // Invite response events intentionally randomize `created_at` over a wide
                                                     // window for metadata resistance, so applying a recent `since` cutoff can
                                                     // drop valid responses and break session bootstrap.
                );
            }

            // Add SharedChannel filters for accepted groups
            let channel_pubkeys: Vec<nostr::PublicKey> = channel_map
                .values()
                .map(|(ch, _)| ch.public_key())
                .collect();
            let channel_pubkeys_hex: HashSet<String> =
                channel_pubkeys.iter().map(|pk| pk.to_hex()).collect();
            if !channel_pubkeys.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(nostr::Kind::Custom(SHARED_CHANNEL_KIND as u16))
                        .authors(channel_pubkeys)
                        .since(since),
                );
            }

            Ok((
                filters,
                subscribed_pubkeys,
                invite_pubkeys,
                channel_pubkeys_hex,
                group_sender_pubkeys_hex,
                peer_app_keys_pubkeys,
            ))
        };

    // Build initial filters
    let mut channel_map = build_channel_map(storage)?;
    sync_runtime_groups(&runtime, storage)?;
    let (
        mut filters,
        mut subscribed_pubkeys,
        mut subscribed_invite_pubkeys,
        mut subscribed_channel_pubkeys,
        mut subscribed_group_sender_pubkeys,
        mut subscribed_peer_app_keys_pubkeys,
    ) = build_filters(storage, chat_id, &channel_map, since_timestamp)?;
    let mut last_refresh = Instant::now();
    let mut last_peer_app_keys_refresh =
        Instant::now() - Duration::from_millis(PEER_APP_KEYS_REFRESH_INTERVAL_MS);
    let mut subscribed_manager_filters: HashSet<String> = HashSet::new();

    // Deduplicate events across relays and overlapping subscriptions.
    // Without this, the same event can be processed multiple times (CPU heavy) and
    // can contribute to unbounded notification backlog (memory heavy).
    let mut seen_event_ids: HashSet<String> = HashSet::new();
    let mut seen_event_ids_order: VecDeque<String> = VecDeque::new();

    // If we receive sender-key messages before the sender-key distribution, keep them here and retry
    // when the distribution arrives.
    let mut pending_sender_key_messages: HashMap<(String, String, u32), Vec<nostr::Event>> =
        HashMap::new();

    // If we receive per-sender published group messages before the sender-key distribution, keep
    // them here and retry when the distribution arrives.
    // If a newly linked device sends its first message before we have processed that device's
    // invite response, keep the outer event here and retry it after session bootstrap lands.
    let mut pending_session_manager_message_events: HashMap<String, nostr::Event> = HashMap::new();
    let mut pending_session_manager_message_event_order: VecDeque<String> = VecDeque::new();

    let mut nearby = match crate::nearby::start(&config, storage, my_pubkey.clone()).await {
        Ok(service) => service,
        Err(err) => {
            output.event(
                "nearby",
                serde_json::json!({
                    "enabled": false,
                    "error": err.to_string(),
                }),
            );
            None
        }
    };

    output.success_message(
        "listen",
        &format!(
            "Listening for messages and invite responses on {}... (Ctrl+C to stop)",
            scope
        ),
    );

    // Create notifications receiver before any subscribe() call to avoid missing
    // backfilled events that relays may send immediately upon subscription.
    let mut notifications = client.notifications();

    let (_fs_tx, fs_rx) = mpsc::channel::<()>();

    // Subscribe only if we have filters
    let mut has_subscription = !filters.is_empty();
    if has_subscription {
        if !connected {
            client.connect().await;
            connected = true;
        }
        subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
        subscribed_group_sender_pubkeys = sync_group_outer_subscriptions(
            &runtime,
            &client,
            &relays,
            storage,
            output,
            &mut seen_event_ids,
            &mut seen_event_ids_order,
        )
        .await?;
        let _ = flush_session_manager_events(
            &runtime,
            &client,
            &config,
            &mut subscribed_manager_filters,
            nearby.as_ref(),
        )
        .await?;
        backfill_recent_pairwise_session_messages(
            &runtime,
            &client,
            &relays,
            &subscribed_pubkeys,
            &config,
            storage,
            output,
            &mut subscribed_group_sender_pubkeys,
            &mut subscribed_manager_filters,
            &mut pending_session_manager_message_events,
            &mut pending_session_manager_message_event_order,
            &mut seen_event_ids,
            &mut seen_event_ids_order,
            &owner_pubkey_hex,
            nearby.as_ref(),
        )
        .await?;
    }

    // Wait for invites/chats if we have nothing to subscribe to yet.
    // Poll storage even without filesystem events so brand-new dirs/files created by
    // sibling `ndr` processes are discovered quickly.
    while !has_subscription {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Drain fs events to avoid unbounded queue growth.
        while let Ok(()) = fs_rx.try_recv() {}

        refresh_runtime_state_from_storage(&runtime, storage, &owner_pubkey_hex)?;
        channel_map = build_channel_map(storage)?;
        let (
            new_filters,
            new_pubkeys,
            new_invite_pubkeys,
            new_channel_pubkeys,
            _new_group_sender_pubkeys,
            new_peer_app_keys_pubkeys,
        ) = build_filters(
            storage,
            chat_id_owned.as_deref(),
            &channel_map,
            since_timestamp,
        )?;
        if !new_filters.is_empty() {
            filters = new_filters;
            subscribed_pubkeys = new_pubkeys;
            subscribed_invite_pubkeys = new_invite_pubkeys;
            subscribed_channel_pubkeys = new_channel_pubkeys;
            subscribed_peer_app_keys_pubkeys = new_peer_app_keys_pubkeys;
            if !connected {
                client.connect().await;
                connected = true;
            }
            subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
            subscribed_group_sender_pubkeys = sync_group_outer_subscriptions(
                &runtime,
                &client,
                &relays,
                storage,
                output,
                &mut seen_event_ids,
                &mut seen_event_ids_order,
            )
            .await?;
            refresh_peer_app_keys_snapshots(
                storage,
                &runtime,
                &client,
                &relays,
                &owner_pubkey_hex,
                chat_id_owned.as_deref(),
            )
            .await?;
            refresh_pending_invite_response_app_keys(&runtime, &client, &relays).await?;
            last_peer_app_keys_refresh = Instant::now();
            let _ = flush_session_manager_events(
                &runtime,
                &client,
                &config,
                &mut subscribed_manager_filters,
                nearby.as_ref(),
            )
            .await?;
            retry_pending_session_manager_message_events(
                &mut pending_session_manager_message_events,
                &mut pending_session_manager_message_event_order,
                &runtime,
                &client,
                &config,
                storage,
                output,
                &mut subscribed_group_sender_pubkeys,
                &mut subscribed_manager_filters,
                &owner_pubkey_hex,
                &mut seen_event_ids,
                &mut seen_event_ids_order,
                nearby.as_ref(),
            )
            .await?;
            backfill_recent_pairwise_session_messages(
                &runtime,
                &client,
                &relays,
                &subscribed_pubkeys,
                &config,
                storage,
                output,
                &mut subscribed_group_sender_pubkeys,
                &mut subscribed_manager_filters,
                &mut pending_session_manager_message_events,
                &mut pending_session_manager_message_event_order,
                &mut seen_event_ids,
                &mut seen_event_ids_order,
                &owner_pubkey_hex,
                nearby.as_ref(),
            )
            .await?;
            has_subscription = true;
        }
    }

    // Handle incoming events - only start after we have a subscription
    loop {
        // Check for filesystem changes (new invites/chats created by other processes)
        let mut should_refresh = false;
        while let Ok(()) = fs_rx.try_recv() {
            should_refresh = true;
        }
        if should_refresh || last_refresh.elapsed() >= Duration::from_millis(100) {
            sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
            if connected {
                if last_peer_app_keys_refresh.elapsed()
                    >= Duration::from_millis(PEER_APP_KEYS_REFRESH_INTERVAL_MS)
                {
                    refresh_peer_app_keys_snapshots(
                        storage,
                        &runtime,
                        &client,
                        &relays,
                        &owner_pubkey_hex,
                        chat_id_owned.as_deref(),
                    )
                    .await?;
                    refresh_pending_invite_response_app_keys(&runtime, &client, &relays).await?;
                    last_peer_app_keys_refresh = Instant::now();
                }
                let _ = flush_session_manager_events(
                    &runtime,
                    &client,
                    &config,
                    &mut subscribed_manager_filters,
                    nearby.as_ref(),
                )
                .await?;
                retry_pending_session_manager_message_events(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &runtime,
                    &client,
                    &config,
                    storage,
                    output,
                    &mut subscribed_group_sender_pubkeys,
                    &mut subscribed_manager_filters,
                    &owner_pubkey_hex,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                    nearby.as_ref(),
                )
                .await?;
            }
            sync_runtime_groups(&runtime, storage)?;
            channel_map = build_channel_map(storage)?;
            let (
                new_filters,
                new_pubkeys,
                new_invite_pubkeys,
                new_channel_pubkeys,
                new_group_sender_pubkeys,
                new_peer_app_keys_pubkeys,
            ) = build_filters(
                storage,
                chat_id_owned.as_deref(),
                &channel_map,
                since_timestamp,
            )?;
            if !new_filters.is_empty()
                && (new_filters.len() != filters.len()
                    || new_pubkeys != subscribed_pubkeys
                    || new_invite_pubkeys != subscribed_invite_pubkeys
                    || new_channel_pubkeys != subscribed_channel_pubkeys
                    || new_group_sender_pubkeys != subscribed_group_sender_pubkeys
                    || new_peer_app_keys_pubkeys != subscribed_peer_app_keys_pubkeys)
            {
                filters = new_filters;
                subscribed_pubkeys = new_pubkeys;
                subscribed_invite_pubkeys = new_invite_pubkeys;
                subscribed_channel_pubkeys = new_channel_pubkeys;
                subscribed_peer_app_keys_pubkeys = new_peer_app_keys_pubkeys;
                subscribe_filters_best_effort(&client, &relays, filters.clone()).await?;
                subscribed_group_sender_pubkeys = sync_group_outer_subscriptions(
                    &runtime,
                    &client,
                    &relays,
                    storage,
                    output,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                )
                .await?;
                backfill_recent_pairwise_session_messages(
                    &runtime,
                    &client,
                    &relays,
                    &subscribed_pubkeys,
                    &config,
                    storage,
                    output,
                    &mut subscribed_group_sender_pubkeys,
                    &mut subscribed_manager_filters,
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                    &owner_pubkey_hex,
                    nearby.as_ref(),
                )
                .await?;
            }
            last_refresh = Instant::now();
        }

        // Wait for relay or nearby events with a timeout to allow fs checks.
        let incoming_event = if let Some(nearby_service) = nearby.as_mut() {
            tokio::select! {
                nearby_event = nearby_service.recv() => nearby_event.map(|incoming| incoming.event),
                notification = notifications.recv() => match notification {
                    Ok(RelayPoolNotification::Event { event, .. }) => Some((*event).clone()),
                    Ok(_) => None,
                    Err(_) => break,
                },
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => None,
            }
        } else {
            match tokio::time::timeout(
                tokio::time::Duration::from_millis(100),
                notifications.recv(),
            )
            .await
            {
                Ok(Ok(RelayPoolNotification::Event { event, .. })) => Some((*event).clone()),
                Ok(Ok(_)) => None,
                Ok(Err(_)) => break,
                Err(_) => None,
            }
        };

        let Some(event) = incoming_event else {
            continue;
        };

        {
            refresh_runtime_state_from_storage(&runtime, storage, &owner_pubkey_hex)?;
            let current_event_id = event.id.to_hex();
            if seen_event_ids.contains(&current_event_id) {
                continue;
            }
            seen_event_ids.insert(current_event_id.clone());
            seen_event_ids_order.push_back(current_event_id.clone());
            if seen_event_ids_order.len() > MAX_SEEN_EVENT_IDS {
                if let Some(old) = seen_event_ids_order.pop_front() {
                    seen_event_ids.remove(&old);
                }
            }
            let session_manager_result = process_session_manager_event(
                &event,
                &runtime,
                &client,
                &config,
                storage,
                output,
                &mut subscribed_manager_filters,
                nearby.as_ref(),
            )
            .await?;
            if event.kind.as_u16() as u32 == INVITE_RESPONSE_KIND {
                refresh_pending_invite_response_app_keys(&runtime, &client, &relays).await?;
                let _ = flush_session_manager_events(
                    &runtime,
                    &client,
                    &config,
                    &mut subscribed_manager_filters,
                    nearby.as_ref(),
                )
                .await?;
                sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
                retry_pending_session_manager_message_events(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &runtime,
                    &client,
                    &config,
                    storage,
                    output,
                    &mut subscribed_group_sender_pubkeys,
                    &mut subscribed_manager_filters,
                    &owner_pubkey_hex,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                    nearby.as_ref(),
                )
                .await?;
            }
            let session_group_decrypts = session_manager_result.session_group_decrypts;
            let current_event_group_routed = session_manager_result.current_event_group_routed;

            if session_manager_result.handled_any && !current_event_group_routed {
                sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
            }

            if session_manager_result.handled_current {
                continue;
            }

            let is_pairwise_session_message = event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND
                && event.tags.iter().any(|tag| {
                    tag.as_slice().first().map(|value| value.as_str()) == Some("header")
                });
            if is_pairwise_session_message && !current_event_group_routed {
                queue_pending_session_manager_message_event(
                    &mut pending_session_manager_message_events,
                    &mut pending_session_manager_message_event_order,
                    &event,
                );
            }

            let event_kind = event.kind.as_u16() as u32;
            let mut used_group_routed_fallback = false;

            if event_kind == MESSAGE_EVENT_KIND && current_event_group_routed {
                if apply_session_group_decrypts(
                    &session_group_decrypts,
                    None,
                    &runtime,
                    &client,
                    &relays,
                    &config,
                    storage,
                    output,
                    &mut subscribed_group_sender_pubkeys,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                )
                .await?
                {
                    sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
                }
                continue;
            }

            // Handle invite responses
            if event_kind == INVITE_RESPONSE_KIND {
                for stored_invite in storage.list_invites()? {
                    let invite = match nostr_double_ratchet::Invite::deserialize(
                        &stored_invite.serialized,
                    ) {
                        Ok(i) => i,
                        Err(_) => continue,
                    };

                    match invite.process_invite_response(&event, our_private_key) {
                        Ok(Some(response)) => {
                            let resolved_owner = response.resolved_owner_pubkey();
                            let their_pubkey = match resolve_verified_owner_pubkey(
                                Some(&client),
                                &relays,
                                &response,
                            )
                            .await
                            {
                                Ok(Some(pubkey)) => pubkey,
                                Ok(None) => {
                                    output.event(
                                        "invite_rejected",
                                        serde_json::json!({
                                            "invite_id": stored_invite.id,
                                            "owner_pubkey": resolved_owner.to_hex(),
                                            "device_pubkey": response.invitee_identity.to_hex(),
                                            "reason": "unverified_owner_claim",
                                        }),
                                    );
                                    continue;
                                }
                                Err(err) => {
                                    output.event(
                                        "invite_rejected",
                                        serde_json::json!({
                                            "invite_id": stored_invite.id,
                                            "owner_pubkey": resolved_owner.to_hex(),
                                            "device_pubkey": response.invitee_identity.to_hex(),
                                            "reason": format!("owner_verification_error: {}", err),
                                        }),
                                    );
                                    continue;
                                }
                            };

                            if invite.purpose.as_deref() == Some("link") {
                                let owner_pubkey_hex = their_pubkey.to_hex();
                                let peer_device_id = response
                                    .device_id
                                    .clone()
                                    .or_else(|| Some(response.invitee_identity.to_hex()));
                                runtime.import_session_state(
                                    their_pubkey,
                                    peer_device_id,
                                    response.session.state.clone(),
                                )?;
                                runtime.setup_user(their_pubkey)?;
                                let _ = flush_session_manager_events(
                                    &runtime,
                                    &client,
                                    &config,
                                    &mut subscribed_manager_filters,
                                    nearby.as_ref(),
                                )
                                .await?;

                                config.set_linked_owner(&owner_pubkey_hex)?;
                                // Publish the runtime's own device invite, not the CLI link invite:
                                // peer devices need this to establish sessions to the linked device.
                                let _ =
                                    crate::commands::session_delivery::publish_runtime_device_invite(
                                        &config, storage, &client,
                                    )
                                    .await;
                                storage.delete_invite(&stored_invite.id)?;

                                output.event(
                                    "link_accepted",
                                    serde_json::json!({
                                        "invite_id": stored_invite.id,
                                        "owner_pubkey": owner_pubkey_hex,
                                        "device_pubkey": invite.inviter.to_hex(),
                                    }),
                                );
                                continue;
                            }

                            if their_pubkey != response.invitee_identity {
                                if let Some(snapshot) =
                                    fetch_latest_app_keys_snapshot(&client, &relays, their_pubkey)
                                        .await?
                                {
                                    runtime.ingest_app_keys_snapshot(
                                        their_pubkey,
                                        snapshot.app_keys,
                                        snapshot.created_at,
                                    );
                                }
                            }

                            let peer_device_id = response
                                .device_id
                                .clone()
                                .or_else(|| Some(response.invitee_identity.to_hex()));
                            let session = response.session;
                            let response_state = session.state.clone();
                            let their_pubkey_hex = hex::encode(their_pubkey.to_bytes());
                            let mut import_state = response_state.clone();

                            let chat = if let Some(mut existing_chat) =
                                storage.get_chat_by_pubkey(&their_pubkey_hex)?
                            {
                                let selected_state = match serde_json::from_str::<
                                    nostr_double_ratchet::SessionState,
                                >(
                                    &existing_chat.session_state
                                ) {
                                    Ok(existing_state) => {
                                        let existing_can_send = Session::new(
                                            existing_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        let response_can_send = Session::new(
                                            response_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        if existing_can_send && !response_can_send {
                                            existing_state
                                        } else {
                                            response_state.clone()
                                        }
                                    }
                                    Err(_) => response_state.clone(),
                                };

                                import_state = selected_state.clone();
                                if peer_device_id.is_some() {
                                    existing_chat.device_id = peer_device_id.clone();
                                }
                                existing_chat.session_state =
                                    serde_json::to_string(&selected_state)?;
                                storage.save_chat(&existing_chat)?;
                                existing_chat
                            } else {
                                let new_chat = crate::storage::StoredChat {
                                    id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                                    their_pubkey: their_pubkey_hex.clone(),
                                    device_id: peer_device_id,
                                    created_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs(),
                                    last_message_at: None,
                                    session_state: serde_json::to_string(&response_state)?,
                                    message_ttl_seconds: None,
                                };
                                storage.save_chat(&new_chat)?;
                                new_chat
                            };
                            runtime.import_session_state(
                                their_pubkey,
                                chat.device_id.clone(),
                                import_state,
                            )?;
                            runtime.setup_user(their_pubkey)?;
                            let _ = flush_session_manager_events(
                                &runtime,
                                &client,
                                &config,
                                &mut subscribed_manager_filters,
                                nearby.as_ref(),
                            )
                            .await?;
                            sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
                            retry_pending_session_manager_message_events(
                                &mut pending_session_manager_message_events,
                                &mut pending_session_manager_message_event_order,
                                &runtime,
                                &client,
                                &config,
                                storage,
                                output,
                                &mut subscribed_group_sender_pubkeys,
                                &mut subscribed_manager_filters,
                                &owner_pubkey_hex,
                                &mut seen_event_ids,
                                &mut seen_event_ids_order,
                                nearby.as_ref(),
                            )
                            .await?;
                            storage.delete_invite(&stored_invite.id)?;

                            // Update subscription for new chat's ephemeral keys
                            let new_pubkeys = collect_chat_pubkeys_with_runtime(
                                storage,
                                &runtime,
                                chat_id_owned.as_deref(),
                            )?;
                            if !new_pubkeys.is_empty() {
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone())
                                    .since(since_timestamp);
                                subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                    .await?;
                                subscribed_pubkeys =
                                    new_pubkeys.iter().map(|pk| pk.to_hex()).collect();
                                backfill_recent_pairwise_session_messages(
                                    &runtime,
                                    &client,
                                    &relays,
                                    &subscribed_pubkeys,
                                    &config,
                                    storage,
                                    output,
                                    &mut subscribed_group_sender_pubkeys,
                                    &mut subscribed_manager_filters,
                                    &mut pending_session_manager_message_events,
                                    &mut pending_session_manager_message_event_order,
                                    &mut seen_event_ids,
                                    &mut seen_event_ids_order,
                                    &owner_pubkey_hex,
                                    nearby.as_ref(),
                                )
                                .await?;
                            }

                            output.event(
                                "session_created",
                                serde_json::json!({
                                    "invite_id": stored_invite.id,
                                    "chat_id": chat.id,
                                    "their_pubkey": their_pubkey_hex,
                                }),
                            );

                            break;
                        }
                        Ok(None) => continue,
                        Err(_) => continue,
                    }
                }
                continue;
            }

            // Shared-channel is only used for signed group-invite announcements that bootstrap
            // 1:1 sessions between members without an existing channel.
            if event_kind == SHARED_CHANNEL_KIND {
                let sender_hex = event.pubkey.to_hex();
                if let Some((channel, group_id)) = channel_map.get(&sender_hex) {
                    let Ok(inner_json) = channel.decrypt_event(&event) else {
                        continue;
                    };
                    let Ok(inner_event) = (|| {
                        let ev: nostr::Event = nostr::JsonUtil::from_json(inner_json.as_str())?;
                        Ok::<nostr::Event, nostr::event::Error>(ev)
                    })() else {
                        // Reject unsigned/non-event payloads.
                        continue;
                    };

                    // Inner payload must be an authentic signed event.
                    if inner_event.verify().is_err() {
                        continue;
                    }
                    if inner_event.kind.as_u16() as u32 != GROUP_INVITE_RUMOR_KIND {
                        continue;
                    }

                    // Skip our own device announcements.
                    let signer_device_pubkey = inner_event.pubkey;
                    if signer_device_pubkey.to_hex() == my_pubkey {
                        continue;
                    }

                    // Bind inner payload to the decrypted shared-channel group.
                    let tag_group_id = inner_event.tags.iter().find_map(|t| {
                        let v = t.clone().to_vec();
                        if v.first().map(|s| s.as_str()) == Some("l") {
                            v.get(1).cloned()
                        } else {
                            None
                        }
                    });
                    if let Some(tag_gid) = tag_group_id {
                        if tag_gid != *group_id {
                            continue;
                        }
                    }

                    let invite_data: serde_json::Value =
                        match serde_json::from_str(&inner_event.content) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                    let invite_url = invite_data
                        .get("inviteUrl")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if invite_url.is_empty() {
                        continue;
                    }
                    if invite_data.get("groupId").and_then(|v| v.as_str())
                        != Some(group_id.as_str())
                    {
                        continue;
                    }

                    let signer_device_hex = signer_device_pubkey.to_hex();
                    let claimed_owner_hex = invite_data
                        .get("ownerPubkey")
                        .and_then(|v| v.as_str())
                        .unwrap_or(signer_device_hex.as_str())
                        .to_string();
                    let Ok(claimed_owner_pubkey) = nostr::PublicKey::from_hex(&claimed_owner_hex)
                    else {
                        continue;
                    };

                    let Some(group) = storage.get_group(group_id)? else {
                        continue;
                    };
                    if !group.data.members.contains(&claimed_owner_hex) {
                        continue;
                    }

                    // The signer must either be the owner itself or an authorized delegate device.
                    let signer_owner_pubkey = if claimed_owner_pubkey == signer_device_pubkey {
                        claimed_owner_pubkey
                    } else {
                        let app_keys =
                            fetch_latest_app_keys(&client, &relays, claimed_owner_pubkey).await?;
                        let authorized = app_keys
                            .as_ref()
                            .and_then(|keys| keys.get_device(&signer_device_pubkey))
                            .is_some();
                        if !authorized {
                            continue;
                        }
                        claimed_owner_pubkey
                    };
                    let signer_owner_hex = signer_owner_pubkey.to_hex();

                    // Skip if it's our own owner identity.
                    if signer_owner_hex == owner_pubkey_hex {
                        continue;
                    }

                    // Deterministic tie-breaker:
                    // when two members publish group invites concurrently, only
                    // the lexicographically-smaller owner pubkey auto-accepts.
                    if owner_pubkey_hex >= signer_owner_hex {
                        continue;
                    }

                    // Parse invite and bind it to the signed announcer identity.
                    let Ok(invite) = nostr_double_ratchet::Invite::from_url(invite_url) else {
                        continue;
                    };
                    let invite_owner_pubkey = invite.owner_public_key.unwrap_or(invite.inviter);
                    if invite_owner_pubkey != signer_owner_pubkey {
                        continue;
                    }
                    if invite.inviter != signer_device_pubkey {
                        continue;
                    }

                    // If we already have a *sendable* session, skip auto-accept.
                    // For concurrent invites, an existing inviter-side session can be
                    // present but still non-sendable; in that case we must still
                    // accept to avoid deadlock.
                    let peer_chats: Vec<_> = storage
                        .list_chats()?
                        .into_iter()
                        .filter(|chat| chat.their_pubkey == signer_owner_hex)
                        .collect();
                    if peer_chats.iter().any(|existing_chat| {
                        let existing_can_send =
                            serde_json::from_str::<nostr_double_ratchet::SessionState>(
                                &existing_chat.session_state,
                            )
                            .map(|state| Session::new(state, existing_chat.id.clone()).can_send())
                            .unwrap_or(false);
                        existing_can_send || existing_chat.last_message_at.is_some()
                    }) {
                        continue;
                    }

                    if let Ok((mut accept_session, response_event)) = invite.accept_with_owner(
                        my_pubkey_key,
                        our_private_key,
                        None,
                        Some(owner_pubkey),
                    ) {
                        // Merge into existing chat if present; otherwise create one.
                        let mut chat = if let Some(mut existing_chat) =
                            storage.get_chat_by_pubkey(&signer_owner_hex)?
                        {
                            let selected_state =
                                match serde_json::from_str::<nostr_double_ratchet::SessionState>(
                                    &existing_chat.session_state,
                                ) {
                                    Ok(existing_state) => {
                                        let existing_can_send = Session::new(
                                            existing_state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        let accepted_can_send = Session::new(
                                            accept_session.state.clone(),
                                            existing_chat.id.clone(),
                                        )
                                        .can_send();
                                        if existing_can_send && !accepted_can_send {
                                            existing_state
                                        } else {
                                            accept_session.state.clone()
                                        }
                                    }
                                    Err(_) => accept_session.state.clone(),
                                };

                            accept_session.state = selected_state.clone();
                            existing_chat.device_id = Some(invite.inviter.to_hex());
                            existing_chat.session_state = serde_json::to_string(&selected_state)?;
                            storage.save_chat(&existing_chat)?;
                            existing_chat
                        } else {
                            let new_chat = crate::storage::StoredChat {
                                id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
                                their_pubkey: signer_owner_hex.clone(),
                                device_id: Some(invite.inviter.to_hex()),
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)?
                                    .as_secs(),
                                last_message_at: None,
                                session_state: serde_json::to_string(&accept_session.state)?,
                                message_ttl_seconds: None,
                            };
                            storage.save_chat(&new_chat)?;
                            new_chat
                        };

                        // Publish the response event.
                        send_event_or_ignore(&client, response_event).await?;

                        // Kick off the session by sending a lightweight typing event.
                        // The inviter is a non-initiator (see Invite::process_invite_response),
                        // so they must receive at least one message before they can send.
                        if let Ok(typing_event) = accept_session.send_typing() {
                            // Persist ratcheted state before network I/O.
                            chat.session_state = serde_json::to_string(&accept_session.state)?;
                            storage.save_chat(&chat)?;

                            // Best-effort; group functionality still works if this fails.
                            let _ = client.send_event(&typing_event).await;
                        }

                        runtime.import_session_state(
                            signer_owner_pubkey,
                            Some(invite.inviter.to_hex()),
                            accept_session.state.clone(),
                        )?;
                        runtime.setup_user(signer_owner_pubkey)?;
                        let _ = flush_session_manager_events(
                            &runtime,
                            &client,
                            &config,
                            &mut subscribed_manager_filters,
                            nearby.as_ref(),
                        )
                        .await?;

                        // Update subscription for new chat's keys.
                        let new_pubkeys = collect_chat_pubkeys_with_runtime(
                            storage,
                            &runtime,
                            chat_id_owned.as_deref(),
                        )?;
                        if !new_pubkeys.is_empty() {
                            let new_filter = Filter::new()
                                .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                .authors(new_pubkeys.clone())
                                .since(since_timestamp);
                            subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                .await?;
                            subscribed_pubkeys = new_pubkeys.iter().map(|pk| pk.to_hex()).collect();
                            backfill_recent_pairwise_session_messages(
                                &runtime,
                                &client,
                                &relays,
                                &subscribed_pubkeys,
                                &config,
                                storage,
                                output,
                                &mut subscribed_group_sender_pubkeys,
                                &mut subscribed_manager_filters,
                                &mut pending_session_manager_message_events,
                                &mut pending_session_manager_message_event_order,
                                &mut seen_event_ids,
                                &mut seen_event_ids_order,
                                &owner_pubkey_hex,
                                nearby.as_ref(),
                            )
                            .await?;
                        }

                        output.event(
                            "group_invite_accepted",
                            serde_json::json!({
                                "group_id": group_id,
                                "member_pubkey": signer_owner_hex,
                                "chat_id": chat.id,
                                "sender_device_pubkey": signer_device_hex,
                            }),
                        );
                    }
                }
                continue;
            }

            // Handle messages
            if event_kind == MESSAGE_EVENT_KIND {
                if let Some(decrypted) = runtime.group_handle_outer_event(&event) {
                    apply_runtime_group_event(&decrypted, storage, output)?;
                    continue;
                }

                let mut decrypted_current_event = false;
                for chat in storage.list_chats()? {
                    let session_state: nostr_double_ratchet::SessionState =
                        match serde_json::from_str(&chat.session_state) {
                            Ok(s) => s,
                            Err(_) => continue,
                        };

                    let mut session = Session::new(session_state, chat.id.clone());

                    match session.receive(&event) {
                        Ok(Some(decrypted_event_json)) => {
                            decrypted_current_event = true;
                            let decrypted_event: serde_json::Value =
                                serde_json::from_str(&decrypted_event_json)?;
                            let rumor_kind = decrypted_event["kind"]
                                .as_u64()
                                .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                as u32;
                            let content = decrypted_event["content"]
                                .as_str()
                                .unwrap_or(&decrypted_event_json)
                                .to_string();

                            let timestamp = event.created_at.as_secs();

                            // Check for group routing tag ["l", group_id]
                            let group_id = decrypted_event["tags"].as_array().and_then(|tags| {
                                tags.iter().find_map(|t| {
                                    let arr = t.as_array()?;
                                    if arr.first()?.as_str()? == "l" {
                                        arr.get(1)?.as_str().map(String::from)
                                    } else {
                                        None
                                    }
                                })
                            });

                            let from_pubkey_hex = chat.their_pubkey.clone();

                            // Always update session state first
                            let mut updated_chat = chat.clone();
                            updated_chat.session_state = serde_json::to_string(&session.state)?;

                            if let Some(ref gid) = group_id {
                                // === Group-routed event ===
                                if rumor_kind == GROUP_METADATA_KIND {
                                    // Group metadata update
                                    if let Some(metadata) =
                                        nostr_double_ratchet::group::parse_group_metadata(&content)
                                    {
                                        let my_device_pubkey = config.public_key()?;
                                        let my_owner_pubkey = config.owner_public_key_hex()?;
                                        let stamp = extract_control_stamp_from_value(
                                            &decrypted_event,
                                            Some(&event.id.to_hex()),
                                            timestamp,
                                        );

                                        if let Some(stamp) = stamp {
                                            match apply_group_metadata(
                                                storage,
                                                gid,
                                                &from_pubkey_hex,
                                                metadata,
                                                stamp,
                                                timestamp * 1000,
                                                &my_owner_pubkey,
                                            )? {
                                                GroupMetadataApplyOutcome::Updated {
                                                    previous_secret,
                                                    group: updated,
                                                } => {
                                                    // If the group's shared-channel secret rotated (e.g. membership changed),
                                                    // force *our* sender key to rotate too so newly-added members can decrypt.
                                                    if updated.secret != previous_secret {
                                                        let _ = storage.delete_group_sender_keys(
                                                            gid,
                                                            &my_device_pubkey,
                                                        )?;
                                                    }

                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "updated",
                                                            "sender_pubkey": from_pubkey_hex,
                                                            "name": updated.name,
                                                            "members": updated.members,
                                                            "admins": updated.admins,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Removed => {
                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "removed",
                                                            "sender_pubkey": from_pubkey_hex,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Created(group_data) => {
                                                    storage.save_chat(&updated_chat)?;
                                                    output.event(
                                                        "group_metadata",
                                                        serde_json::json!({
                                                            "group_id": gid,
                                                            "action": "created",
                                                            "sender_pubkey": from_pubkey_hex,
                                                            "name": group_data.name,
                                                            "members": group_data.members,
                                                            "admins": group_data.admins,
                                                        }),
                                                    );
                                                }
                                                GroupMetadataApplyOutcome::Ignored
                                                | GroupMetadataApplyOutcome::Rejected => {
                                                    storage.save_chat(&updated_chat)?;
                                                }
                                            }
                                            sync_runtime_groups(&runtime, storage)?;
                                        }
                                    }
                                } else if rumor_kind == GROUP_SENDER_KEY_DISTRIBUTION_KIND {
                                    // Sender-key distribution over the 1:1 Double Ratchet session (forward secrecy).
                                    let dist = match serde_json::from_str::<
                                        nostr_double_ratchet::SenderKeyDistribution,
                                    >(&content)
                                    {
                                        Ok(d) => d,
                                        Err(_) => continue,
                                    };

                                    if dist.group_id != *gid {
                                        continue;
                                    }

                                    let Ok(from_owner_pubkey) =
                                        nostr::PublicKey::from_hex(&from_pubkey_hex)
                                    else {
                                        continue;
                                    };
                                    let sender_device_pubkey = chat
                                        .device_id
                                        .as_ref()
                                        .and_then(|device_id| {
                                            nostr::PublicKey::from_hex(device_id).ok()
                                        })
                                        .unwrap_or(from_owner_pubkey);
                                    let sender_device_pubkey_hex = sender_device_pubkey.to_hex();

                                    sync_runtime_groups(&runtime, storage)?;
                                    let runtime_event =
                                        match serde_json::from_str::<nostr::UnsignedEvent>(
                                            &decrypted_event_json,
                                        ) {
                                            Ok(event) => event,
                                            Err(_) => continue,
                                        };
                                    let drained = runtime.group_handle_incoming_session_event(
                                        &runtime_event,
                                        from_owner_pubkey,
                                        Some(sender_device_pubkey),
                                    );
                                    for decrypted in drained {
                                        apply_runtime_group_event(&decrypted, storage, output)?;
                                    }
                                    subscribed_group_sender_pubkeys =
                                        sync_group_outer_subscriptions(
                                            &runtime,
                                            &client,
                                            &relays,
                                            storage,
                                            output,
                                            &mut seen_event_ids,
                                            &mut seen_event_ids_order,
                                        )
                                        .await?;

                                    // Retry any pending SharedChannel messages for this (group,sender,key_id).
                                    let pending_key = (
                                        gid.clone(),
                                        sender_device_pubkey_hex.clone(),
                                        dist.key_id,
                                    );
                                    if let Some(mut pending) =
                                        pending_sender_key_messages.remove(&pending_key)
                                    {
                                        // Best-effort: process in message-number order.
                                        pending.sort_by_key(|e| {
                                            e.tags
                                                .iter()
                                                .find_map(|t| {
                                                    let v = t.clone().to_vec();
                                                    if v.first().map(|s| s.as_str()) == Some("n") {
                                                        v.get(1)?.parse::<u64>().ok()
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .unwrap_or(0)
                                        });

                                        for env in pending {
                                            let key_id = dist.key_id;
                                            let n = env
                                                .tags
                                                .iter()
                                                .find_map(|t| {
                                                    let v = t.clone().to_vec();
                                                    if v.first().map(|s| s.as_str()) == Some("n") {
                                                        v.get(1)?.parse::<u32>().ok()
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .unwrap_or(0);

                                            if let Some(mut st) = storage
                                                .get_group_sender_key_state(
                                                    gid,
                                                    &sender_device_pubkey_hex,
                                                    key_id,
                                                )?
                                            {
                                                if let Ok(plaintext_json) =
                                                    st.decrypt(n, &env.content)
                                                {
                                                    storage.upsert_group_sender_key_state(
                                                        gid,
                                                        &sender_device_pubkey_hex,
                                                        &st,
                                                    )?;

                                                    if let Ok(decrypted_event) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            &plaintext_json,
                                                        )
                                                    {
                                                        let rumor_kind = decrypted_event["kind"]
                                                            .as_u64()
                                                            .unwrap_or(CHAT_MESSAGE_KIND as u64)
                                                            as u32;
                                                        let content = decrypted_event["content"]
                                                            .as_str()
                                                            .unwrap_or(&plaintext_json)
                                                            .to_string();

                                                        let timestamp = env.created_at.as_secs();
                                                        let now_seconds =
                                                            std::time::SystemTime::now()
                                                                .duration_since(
                                                                    std::time::UNIX_EPOCH,
                                                                )?
                                                                .as_secs();
                                                        let expires_at =
                                                            extract_expiration_tag_seconds(
                                                                &decrypted_event,
                                                            );

                                                        if rumor_kind == CHAT_MESSAGE_KIND
                                                            || rumor_kind == 14
                                                        {
                                                            if is_expired(expires_at, now_seconds) {
                                                                continue;
                                                            }
                                                            let msg_id = env.id.to_hex();
                                                            let stored = StoredGroupMessage {
                                                                id: msg_id.clone(),
                                                                group_id: gid.clone(),
                                                                sender_pubkey: from_pubkey_hex
                                                                    .clone(),
                                                                content: content.clone(),
                                                                timestamp,
                                                                is_outgoing: false,
                                                                expires_at,
                                                            };
                                                            storage.save_group_message(&stored)?;
                                                            output.event(
                                                                "group_message",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "message_id": msg_id,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "content": content,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        } else if rumor_kind == REACTION_KIND {
                                                            let message_id =
                                                                extract_e_tag(&decrypted_event);
                                                            output.event(
                                                                "group_reaction",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "message_id": message_id,
                                                                    "emoji": content,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        } else if rumor_kind == TYPING_KIND {
                                                            output.event(
                                                                "group_typing",
                                                                serde_json::json!({
                                                                    "group_id": gid,
                                                                    "sender_pubkey": from_pubkey_hex,
                                                                    "sender_device_pubkey": sender_device_pubkey_hex,
                                                                    "timestamp": timestamp,
                                                                }),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_sender_key",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "sender_device_pubkey": sender_device_pubkey_hex,
                                            "key_id": dist.key_id,
                                            "iteration": dist.iteration,
                                            "sender_event_pubkey": dist.sender_event_pubkey,
                                            "transport": "session",
                                        }),
                                    );
                                } else if rumor_kind == CHAT_MESSAGE_KIND || rumor_kind == 14 {
                                    // Group chat message
                                    let now_seconds = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs();
                                    let expires_at =
                                        extract_expiration_tag_seconds(&decrypted_event);

                                    let msg_id = event.id.to_hex();
                                    if !is_expired(expires_at, now_seconds) {
                                        let stored = StoredGroupMessage {
                                            id: msg_id.clone(),
                                            group_id: gid.clone(),
                                            sender_pubkey: from_pubkey_hex.clone(),
                                            content: content.clone(),
                                            timestamp,
                                            is_outgoing: false,
                                            expires_at,
                                        };
                                        storage.save_group_message(&stored)?;

                                        output.event(
                                            "group_message",
                                            serde_json::json!({
                                                "group_id": gid,
                                                "message_id": msg_id,
                                                "sender_pubkey": from_pubkey_hex,
                                                "content": content,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    }

                                    storage.save_chat(&updated_chat)?;
                                } else if rumor_kind == REACTION_KIND {
                                    let message_id = extract_e_tag(&decrypted_event);
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_reaction",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "message_id": message_id,
                                            "emoji": content,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else if rumor_kind == TYPING_KIND {
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "group_typing",
                                        serde_json::json!({
                                            "group_id": gid,
                                            "sender_pubkey": from_pubkey_hex,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                }
                            } else {
                                // === 1:1 event (no group tag) ===
                                if rumor_kind == CHAT_SETTINGS_KIND {
                                    if let Some(ttl) = parse_chat_settings_ttl_seconds(&content) {
                                        updated_chat.message_ttl_seconds = ttl;
                                        storage.save_chat(&updated_chat)?;
                                        output.event(
                                            "chat_settings",
                                            serde_json::json!({
                                                "chat_id": updated_chat.id,
                                                "from_pubkey": from_pubkey_hex,
                                                "message_ttl_seconds": ttl,
                                                "timestamp": timestamp,
                                            }),
                                        );
                                    }
                                } else if rumor_kind == RECEIPT_KIND {
                                    let receipt_type = content.clone();
                                    let message_ids: Vec<String> = extract_e_tags(&decrypted_event);

                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "receipt",
                                        serde_json::json!({
                                            "chat_id": updated_chat.id,
                                            "from_pubkey": from_pubkey_hex,
                                            "type": receipt_type,
                                            "message_ids": message_ids,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else if rumor_kind == REACTION_KIND {
                                    let message_id = extract_e_tag(&decrypted_event);
                                    let reaction_id = event.id.to_hex();
                                    let stored = StoredReaction {
                                        id: reaction_id,
                                        chat_id: chat.id.clone(),
                                        message_id: message_id.clone(),
                                        from_pubkey: from_pubkey_hex.clone(),
                                        emoji: content.clone(),
                                        timestamp,
                                        is_outgoing: false,
                                    };
                                    storage.save_reaction(&stored)?;
                                    storage.save_chat(&updated_chat)?;

                                    output.event(
                                        "reaction",
                                        IncomingReaction {
                                            chat_id: updated_chat.id.clone(),
                                            from_pubkey: from_pubkey_hex,
                                            message_id,
                                            emoji: content,
                                            timestamp,
                                        },
                                    );
                                } else if rumor_kind == TYPING_KIND {
                                    storage.save_chat(&updated_chat)?;
                                    output.event(
                                        "typing",
                                        serde_json::json!({
                                            "chat_id": updated_chat.id,
                                            "from_pubkey": from_pubkey_hex,
                                            "timestamp": timestamp,
                                        }),
                                    );
                                } else {
                                    // Chat message (kind 14 or default)
                                    let now_seconds = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)?
                                        .as_secs();
                                    let expires_at =
                                        extract_expiration_tag_seconds(&decrypted_event);

                                    let msg_id = event.id.to_hex();
                                    if !is_expired(expires_at, now_seconds) {
                                        let stored = StoredMessage {
                                            id: msg_id.clone(),
                                            chat_id: chat.id.clone(),
                                            from_pubkey: from_pubkey_hex.clone(),
                                            content: content.clone(),
                                            timestamp,
                                            is_outgoing: false,
                                            expires_at,
                                        };
                                        storage.save_message(&stored)?;
                                        updated_chat.last_message_at = Some(timestamp);

                                        storage.save_chat(&updated_chat)?;
                                        output.event(
                                            "message",
                                            IncomingMessage {
                                                chat_id: updated_chat.id.clone(),
                                                message_id: msg_id,
                                                from_pubkey: from_pubkey_hex,
                                                content,
                                                timestamp,
                                            },
                                        );
                                    }
                                }
                            }

                            storage.save_chat(&updated_chat)?;

                            // KEY FIX: Update subscription after receiving a message
                            // because the ratchet may have rotated ephemeral keys
                            let new_pubkeys = collect_chat_pubkeys_with_runtime(
                                storage,
                                &runtime,
                                chat_id_owned.as_deref(),
                            )?;
                            let new_pubkey_set: HashSet<String> =
                                new_pubkeys.iter().map(|pk| pk.to_hex()).collect();

                            if new_pubkey_set != subscribed_pubkeys {
                                // Keys changed, resubscribe
                                let new_filter = Filter::new()
                                    .kind(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16))
                                    .authors(new_pubkeys.clone())
                                    .since(since_timestamp);
                                subscribe_filters_best_effort(&client, &relays, vec![new_filter])
                                    .await?;
                                subscribed_pubkeys = new_pubkey_set;
                            }

                            break;
                        }
                        _ => continue,
                    }
                }

                if apply_session_group_decrypts(
                    &session_group_decrypts,
                    if decrypted_current_event {
                        Some(current_event_id.as_str())
                    } else {
                        None
                    },
                    &runtime,
                    &client,
                    &relays,
                    &config,
                    storage,
                    output,
                    &mut subscribed_group_sender_pubkeys,
                    &mut seen_event_ids,
                    &mut seen_event_ids_order,
                )
                .await?
                {
                    used_group_routed_fallback = true;
                }
            }

            if used_group_routed_fallback {
                sync_chats_from_runtime(storage, &runtime, &owner_pubkey_hex)?;
            }
        }
    }

    Ok(())
}
