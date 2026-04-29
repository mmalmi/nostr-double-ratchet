use anyhow::Result;
use nostr::PublicKey;
use nostr_double_ratchet::{Invite, NdrRuntime, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND};

use crate::commands::owner_claim::fetch_latest_app_keys_snapshot_with_timeout;
use crate::config::Config;

fn has_tag_value(event: &nostr::Event, name: &str, value: &str) -> bool {
    event.tags.iter().any(|tag| {
        let parts = tag.as_slice();
        parts.first().map(|part| part.as_str()) == Some(name)
            && parts.get(1).map(|part| part.as_str()) == Some(value)
    })
}

pub(crate) fn is_double_ratchet_invite_event(event: &nostr::Event) -> bool {
    if event.kind.as_u16() != INVITE_EVENT_KIND as u16 {
        return false;
    }

    let has_invite_label = has_tag_value(event, "l", "double-ratchet/invites");
    let has_invite_d = event.tags.iter().any(|tag| {
        let parts = tag.as_slice();
        parts.first().map(|part| part.as_str()) == Some("d")
            && parts
                .get(1)
                .map(|part| part.as_str().starts_with("double-ratchet/invites/"))
                .unwrap_or(false)
    });

    has_invite_label && has_invite_d
}

pub(crate) fn is_double_ratchet_public_invite_event(event: &nostr::Event) -> bool {
    is_double_ratchet_invite_event(event)
        && has_tag_value(event, "d", "double-ratchet/invites/public")
}

fn session_state_can_send(state: &nostr_double_ratchet::SessionState) -> bool {
    state.their_next_nostr_public_key.is_some() && state.our_current_nostr_key.is_some()
}

fn has_send_capable_session(runtime: &NdrRuntime, recipient: PublicKey) -> bool {
    runtime
        .export_active_sessions()
        .into_iter()
        .any(|(owner, _, state)| owner == recipient && session_state_can_send(&state))
}

fn known_recipient_device_count(runtime: &NdrRuntime, recipient: PublicKey) -> usize {
    runtime
        .get_stored_user_record_json(recipient)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<nostr_double_ratchet::StoredUserRecord>(&json).ok())
        .map(|record| {
            record
                .devices
                .len()
                .max(record.known_device_identities.len())
        })
        .unwrap_or(0)
}

pub(crate) async fn refresh_recipient_app_keys(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    recipient: PublicKey,
) -> Result<std::collections::HashSet<PublicKey>> {
    use std::collections::HashSet;
    use tokio::time::{Duration, Instant};

    const APP_KEYS_POLL_FETCH_TIMEOUT: Duration = Duration::from_secs(1);
    const APP_KEYS_DISCOVERY_WITH_EXISTING_DIRECT_SESSION: Duration = Duration::from_secs(1);
    const APP_KEYS_DISCOVERY_DEFAULT: Duration = Duration::from_secs(15);

    let relays = config.resolved_relays();
    let discovery_window = if has_send_capable_session(runtime, recipient)
        && known_recipient_device_count(runtime, recipient) <= 1
    {
        APP_KEYS_DISCOVERY_WITH_EXISTING_DIRECT_SESSION
    } else {
        APP_KEYS_DISCOVERY_DEFAULT
    };
    let deadline = Instant::now() + discovery_window;
    while Instant::now() <= deadline {
        if let Some(snapshot) = fetch_latest_app_keys_snapshot_with_timeout(
            client,
            &relays,
            recipient,
            APP_KEYS_POLL_FETCH_TIMEOUT,
        )
        .await?
        {
            let recipient_devices: HashSet<PublicKey> = snapshot
                .app_keys
                .get_all_devices()
                .into_iter()
                .map(|device| device.identity_pubkey)
                .collect();

            runtime.ingest_app_keys_snapshot(recipient, snapshot.app_keys, snapshot.created_at);
            return Ok(recipient_devices);
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(HashSet::new())
}

pub(crate) fn recipient_devices_missing_active_sessions(
    runtime: &NdrRuntime,
    recipient: PublicKey,
    recipient_devices: &std::collections::HashSet<PublicKey>,
) -> std::collections::HashSet<PublicKey> {
    use std::collections::HashSet;

    let active_device_ids: HashSet<String> = runtime
        .export_active_sessions()
        .into_iter()
        .filter_map(|(owner, device_id, state)| {
            (owner == recipient && session_state_can_send(&state)).then_some(device_id)
        })
        .collect();
    let our_device_id = runtime.get_device_id();
    let mut candidate_device_ids: HashSet<String> =
        recipient_devices.iter().map(PublicKey::to_hex).collect();
    candidate_device_ids.insert(recipient.to_hex());

    candidate_device_ids
        .iter()
        .filter_map(|device_id| {
            (device_id != our_device_id && !active_device_ids.contains(device_id))
                .then(|| PublicKey::from_hex(device_id).ok())
                .flatten()
        })
        .collect()
}

pub(crate) async fn process_recipient_device_invites(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    recipient: PublicKey,
    recipient_devices: &std::collections::HashSet<PublicKey>,
) -> Result<()> {
    use nostr_sdk::Filter;
    use tokio::time::{Duration, Instant};

    const DEVICE_INVITE_POLL_FETCH_TIMEOUT: Duration = Duration::from_secs(1);

    if recipient_devices.is_empty() {
        return Ok(());
    }

    let relays = config.resolved_relays();
    let discovery_window = if has_send_capable_session(runtime, recipient) {
        Duration::from_secs(3)
    } else {
        Duration::from_secs(15)
    };
    let deadline = Instant::now() + discovery_window;
    let mut processed = std::collections::HashSet::new();

    while Instant::now() <= deadline {
        let invite_events = crate::nostr_client::fetch_events_best_effort(
            client,
            &relays,
            Filter::new()
                .kind(nostr::Kind::Custom(INVITE_EVENT_KIND as u16))
                .authors(recipient_devices.iter().copied().collect::<Vec<_>>())
                .limit(50),
            DEVICE_INVITE_POLL_FETCH_TIMEOUT,
        )
        .await?;

        for event in invite_events {
            if !recipient_devices.contains(&event.pubkey) {
                continue;
            }
            let expected_d = format!("double-ratchet/invites/{}", event.pubkey.to_hex());
            let matches_device_invite = event.tags.iter().any(|tag| {
                let parts = tag.as_slice();
                parts.first().map(|value| value.as_str()) == Some("d")
                    && parts.get(1).map(|value| value.as_str()) == Some(expected_d.as_str())
            });
            if matches_device_invite && processed.insert(event.pubkey) {
                if let Ok(invite) = Invite::from_event(&event) {
                    let _ = runtime.accept_invite(&invite, Some(recipient));
                }
            }
        }

        if processed == *recipient_devices {
            break;
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    Ok(())
}

pub(crate) async fn backfill_recent_invite_responses(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
) -> Result<()> {
    use nostr_sdk::Filter;

    const INVITE_RESPONSE_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

    let Some(invite_response_pubkey) = runtime.current_device_invite_response_pubkey() else {
        return Ok(());
    };

    let relays = config.resolved_relays();
    let mut invite_responses = crate::nostr_client::fetch_events_best_effort(
        client,
        &relays,
        Filter::new()
            .kind(nostr::Kind::Custom(INVITE_RESPONSE_KIND as u16))
            .pubkeys(vec![invite_response_pubkey])
            .limit(100),
        INVITE_RESPONSE_FETCH_TIMEOUT,
    )
    .await?;
    invite_responses.sort_by_key(|event| (event.created_at.as_secs(), event.id.to_hex()));

    for event in invite_responses {
        runtime.process_received_event(event);
    }

    Ok(())
}

pub(crate) async fn prepare_recipient_delivery_sessions(
    runtime: &NdrRuntime,
    client: &nostr_sdk::Client,
    config: &Config,
    recipient: PublicKey,
) -> Result<()> {
    let recipient_devices = refresh_recipient_app_keys(runtime, client, config, recipient).await?;
    backfill_recent_invite_responses(runtime, client, config).await?;
    let invite_targets =
        recipient_devices_missing_active_sessions(runtime, recipient, &recipient_devices);
    process_recipient_device_invites(runtime, client, config, recipient, &invite_targets).await?;
    Ok(())
}
