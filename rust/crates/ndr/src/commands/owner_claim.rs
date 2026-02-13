use std::time::Duration;

use anyhow::Result;
use nostr_sdk::{Client, Filter};

pub(crate) async fn resolve_verified_owner_pubkey(
    client: Option<&Client>,
    response: &nostr_double_ratchet::InviteResponse,
) -> Result<Option<nostr::PublicKey>> {
    let owner_pubkey = response.resolved_owner_pubkey();
    if owner_pubkey == response.invitee_identity {
        return Ok(Some(owner_pubkey));
    }

    let Some(client) = client else {
        return Ok(None);
    };

    let app_keys = fetch_latest_app_keys(client, owner_pubkey).await?;
    if response.has_verified_owner_claim(app_keys.as_ref()) {
        return Ok(Some(owner_pubkey));
    }

    Ok(None)
}

pub(crate) async fn fetch_latest_app_keys(
    client: &Client,
    owner_pubkey: nostr::PublicKey,
) -> Result<Option<nostr_double_ratchet::AppKeys>> {
    let filter = Filter::new()
        .kind(nostr::Kind::Custom(
            nostr_double_ratchet::APP_KEYS_EVENT_KIND as u16,
        ))
        .author(owner_pubkey)
        .limit(20);

    let events = client
        .fetch_events(vec![filter], Some(Duration::from_secs(3)))
        .await?;

    let mut latest: Option<(u64, nostr_double_ratchet::AppKeys)> = None;
    for event in events.iter() {
        if !nostr_double_ratchet::is_app_keys_event(event) {
            continue;
        }
        let created_at = event.created_at.as_u64();
        if let Ok(keys) = nostr_double_ratchet::AppKeys::from_event(event) {
            match latest {
                Some((ts, _)) if created_at < ts => {}
                _ => latest = Some((created_at, keys)),
            }
        }
    }
    Ok(latest.map(|(_, keys)| keys))
}
