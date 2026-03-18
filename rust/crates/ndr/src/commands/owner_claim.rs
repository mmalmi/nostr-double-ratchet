use std::time::Duration;

use anyhow::Result;
use nostr_sdk::{Client, Filter};

pub(crate) async fn resolve_verified_owner_pubkey(
    client: Option<&Client>,
    relays: &[String],
    response: &nostr_double_ratchet::InviteResponse,
) -> Result<Option<nostr::PublicKey>> {
    let owner_pubkey = response.resolved_owner_pubkey();
    if owner_pubkey == response.invitee_identity {
        return Ok(Some(owner_pubkey));
    }

    let Some(client) = client else {
        return Ok(None);
    };

    let app_keys = fetch_latest_app_keys(client, relays, owner_pubkey).await?;
    if response.has_verified_owner_claim(app_keys.as_ref()) {
        return Ok(Some(owner_pubkey));
    }

    Ok(None)
}

pub(crate) async fn fetch_latest_app_keys(
    client: &Client,
    relays: &[String],
    owner_pubkey: nostr::PublicKey,
) -> Result<Option<nostr_double_ratchet::AppKeys>> {
    Ok(fetch_latest_app_keys_snapshot(client, relays, owner_pubkey)
        .await?
        .map(|snapshot| snapshot.app_keys))
}

pub(crate) async fn fetch_latest_app_keys_snapshot(
    client: &Client,
    relays: &[String],
    owner_pubkey: nostr::PublicKey,
) -> Result<Option<nostr_double_ratchet::AppKeysSnapshot>> {
    fetch_latest_app_keys_snapshot_with_timeout(
        client,
        relays,
        owner_pubkey,
        Duration::from_secs(3),
    )
    .await
}

pub(crate) async fn fetch_latest_app_keys_snapshot_with_timeout(
    client: &Client,
    relays: &[String],
    owner_pubkey: nostr::PublicKey,
    timeout: Duration,
) -> Result<Option<nostr_double_ratchet::AppKeysSnapshot>> {
    let filter = Filter::new()
        .kind(nostr::Kind::Custom(
            nostr_double_ratchet::APP_KEYS_EVENT_KIND as u16,
        ))
        .author(owner_pubkey)
        .limit(20);

    let events =
        crate::nostr_client::fetch_events_best_effort(client, relays, filter, timeout).await?;

    Ok(nostr_double_ratchet::select_latest_app_keys_from_events(
        events.iter(),
    ))
}
