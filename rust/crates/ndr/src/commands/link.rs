use anyhow::Result;
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::Storage;

const DEVICE_INVITE_ID: &str = "_device";

#[derive(Serialize)]
struct LinkCreated {
    id: String,
    url: String,
    device_pubkey: String,
    generated_identity: bool,
    device_invite_published: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_invite_publish_error: Option<String>,
}

#[derive(Serialize)]
struct LinkAccepted {
    owner_pubkey: String,
    device_pubkey: String,
    response_event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_keys_event: Option<String>,
}

fn now_secs() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn build_link_invite(device_pubkey_hex: &str) -> Result<nostr_double_ratchet::Invite> {
    let device_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(device_pubkey_hex)?;
    let mut invite = nostr_double_ratchet::Invite::create_new(device_pubkey, None, None)?;
    invite.purpose = Some("link".to_string());
    Ok(invite)
}

fn build_device_invite(device_pubkey_hex: &str) -> Result<nostr_double_ratchet::Invite> {
    let device_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(device_pubkey_hex)?;
    nostr_double_ratchet::Invite::create_new(
        device_pubkey,
        Some(device_pubkey_hex.to_string()),
        None,
    )
    .map_err(Into::into)
}

fn ensure_device_invite(
    config: &Config,
    storage: &Storage,
) -> Result<nostr_double_ratchet::Invite> {
    let device_pubkey_hex = config.public_key()?;

    if let Ok(Some(stored)) = storage.get_invite(DEVICE_INVITE_ID) {
        if let Ok(invite) = nostr_double_ratchet::Invite::deserialize(&stored.serialized) {
            let valid = invite.inviter.to_hex() == device_pubkey_hex
                && invite.device_id.as_deref() == Some(device_pubkey_hex.as_str())
                && invite.inviter_ephemeral_private_key.is_some()
                && invite.purpose.is_none();
            if valid {
                return Ok(invite);
            }
        }
    }

    let invite = build_device_invite(&device_pubkey_hex)?;
    let stored = crate::storage::StoredInvite {
        id: DEVICE_INVITE_ID.to_string(),
        label: Some("device".to_string()),
        url: invite.get_url("https://chat.iris.to")?,
        created_at: now_secs()?,
        serialized: invite.serialize()?,
    };
    storage.save_invite(&stored)?;
    Ok(invite)
}

fn render_qr(url: &str) -> Result<String> {
    // Implemented with an optional dependency (qrcode). If it fails for any reason,
    // we still want linking to work, so return the error to caller to decide.
    let code = qrcode::QrCode::new(url.as_bytes())
        .map_err(|e| anyhow::anyhow!("QR generation failed: {e}"))?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

/// Create a private link invite for a new device.
pub async fn create(config: &Config, storage: &Storage, output: &Output) -> Result<()> {
    let mut config = config.clone();
    let (device_pubkey_hex, generated_identity) = config.ensure_identity()?;
    let invite = build_link_invite(&device_pubkey_hex)?;
    let url = invite.get_url("https://chat.iris.to")?;

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let stored = crate::storage::StoredInvite {
        id: id.clone(),
        label: Some("link".to_string()),
        url: url.clone(),
        created_at: now_secs()?,
        serialized: invite.serialize()?,
    };

    storage.save_invite(&stored)?;

    // Ensure this device has a public Invite event (needed for multi-device AppKeys fanout).
    let device_invite = ensure_device_invite(&config, storage)?;
    let mut device_invite_published = false;
    let mut device_invite_publish_error = None::<String>;

    let relays = config.resolved_relays();
    if !relays.is_empty() {
        if let Ok(unsigned) = device_invite.get_event() {
            let sk_bytes = config.private_key_bytes()?;
            let sk = nostr::SecretKey::from_slice(&sk_bytes)?;
            let keys = nostr::Keys::new(sk);
            let event = unsigned
                .sign_with_keys(&keys)
                .map_err(|e| anyhow::anyhow!("Failed to sign device invite event: {}", e))?;

            let client = Client::default();
            for relay in &relays {
                client.add_relay(relay).await?;
            }
            client.connect().await;

            match client.send_event(event).await {
                Ok(_) => device_invite_published = true,
                Err(err) => {
                    device_invite_publish_error = Some(err.to_string());
                }
            }
        }
    }

    let created = LinkCreated {
        id,
        url: url.clone(),
        device_pubkey: device_pubkey_hex,
        generated_identity,
        device_invite_published,
        device_invite_publish_error: device_invite_publish_error.clone(),
    };

    if output.is_json() {
        output.success("link.create", created);
    } else {
        println!("Link this device by scanning this QR code with your main device:");
        if let Ok(qr) = render_qr(&url) {
            println!("{}", qr);
        } else {
            println!("(QR code unavailable)");
        }
        println!("{}", url);
        if let Some(err) = device_invite_publish_error {
            eprintln!(
                "Warning: failed to publish device invite event to relays (still created link invite): {}",
                err
            );
        }
    }
    Ok(())
}

/// Accept a link invite and publish the response event.
pub async fn accept(url: &str, config: &Config, storage: &Storage, output: &Output) -> Result<()> {
    let mut config = config.clone();
    if config.linked_owner.is_some() {
        anyhow::bail!("Linked devices cannot accept link invites.");
    }

    if !config.is_logged_in() {
        let _ = config.ensure_identity()?;
    }

    let invite = nostr_double_ratchet::Invite::from_url(url)?;
    if invite.purpose.as_deref() != Some("link") {
        anyhow::bail!("Invite is not a link invite.");
    }

    let owner_pubkey_hex = config.public_key()?;
    let owner_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;
    let owner_private_key = config.private_key_bytes()?;

    let (_session, response_event) =
        invite.accept_with_owner(owner_pubkey, owner_private_key, None, Some(owner_pubkey))?;

    // Publish to relays if configured
    let relays = config.resolved_relays();
    let mut client: Option<Client> = None;
    if !relays.is_empty() {
        let c = Client::default();
        for relay in &relays {
            c.add_relay(relay).await?;
        }
        c.connect().await;
        send_event_or_ignore(&c, response_event.clone()).await?;
        client = Some(c);
    }

    // Register the device in AppKeys (multi-device support), persist, and publish.
    let device_pubkey = invite.inviter;
    let mut app_keys = if let Some(c) = &client {
        fetch_latest_app_keys(c, owner_pubkey)
            .await?
            .or_else(|| storage.load_app_keys().ok().flatten())
            .unwrap_or_else(|| nostr_double_ratchet::AppKeys::new(Vec::new()))
    } else {
        storage
            .load_app_keys()?
            .unwrap_or_else(|| nostr_double_ratchet::AppKeys::new(Vec::new()))
    };

    let now = now_secs()?;
    app_keys.add_device(nostr_double_ratchet::DeviceEntry::new(owner_pubkey, now));
    app_keys.add_device(nostr_double_ratchet::DeviceEntry::new(device_pubkey, now));
    storage.save_app_keys(&app_keys)?;

    let mut app_keys_event_json = None::<String>;
    if let Some(c) = &client {
        let sk_bytes = config.private_key_bytes()?;
        let sk = nostr::SecretKey::from_slice(&sk_bytes)?;
        let keys = nostr::Keys::new(sk);
        let unsigned = app_keys.get_event(owner_pubkey);
        let signed = unsigned
            .sign_with_keys(&keys)
            .map_err(|e| anyhow::anyhow!("Failed to sign app-keys event: {}", e))?;
        send_event_or_ignore(c, signed.clone()).await?;
        app_keys_event_json = Some(nostr::JsonUtil::as_json(&signed));
    }

    output.success(
        "link.accept",
        LinkAccepted {
            owner_pubkey: owner_pubkey_hex,
            device_pubkey: invite.inviter.to_hex(),
            response_event: nostr::JsonUtil::as_json(&response_event),
            app_keys_event: app_keys_event_json,
        },
    );

    Ok(())
}

async fn fetch_latest_app_keys(
    client: &Client,
    owner_pubkey: nostr::PublicKey,
) -> Result<Option<nostr_double_ratchet::AppKeys>> {
    use nostr_sdk::Filter;
    use std::time::Duration;

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
    Ok(latest.map(|(_, k)| k))
}

async fn send_event_or_ignore(client: &Client, event: nostr::Event) -> Result<()> {
    match client.send_event(event).await {
        Ok(_) => Ok(()),
        Err(_) if should_ignore_publish_errors() => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn should_ignore_publish_errors() -> bool {
    for key in ["NDR_IGNORE_PUBLISH_ERRORS", "NOSTR_IGNORE_PUBLISH_ERRORS"] {
        if let Ok(val) = std::env::var(key) {
            let val = val.trim().to_lowercase();
            return matches!(val.as_str(), "1" | "true" | "yes" | "on");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        // Keep tests offline by default.
        config.relays = Vec::new();
        config.save().unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        (temp, config, storage)
    }

    #[test]
    fn test_build_link_invite_sets_purpose() {
        let device_pubkey_hex = "5d21f6f47fbcdd6b8f0be05a9e9b4a1fdb2f4d9cced65c0b6f965c2a3a9a1b5c";

        let invite = build_link_invite(device_pubkey_hex).unwrap();
        assert_eq!(invite.purpose.as_deref(), Some("link"));
        assert!(invite.owner_public_key.is_none());
    }

    #[tokio::test]
    async fn test_create_stores_link_invite() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        create(&config, &storage, &output).await.unwrap();

        let invites = storage.list_invites().unwrap();
        assert_eq!(invites.len(), 2);

        let link = invites
            .iter()
            .find(|i| i.label.as_deref() == Some("link"))
            .expect("expected link invite");
        let link_invite = nostr_double_ratchet::Invite::deserialize(&link.serialized).unwrap();
        assert_eq!(link_invite.purpose.as_deref(), Some("link"));
        assert!(link_invite.owner_public_key.is_none());

        let device = invites
            .iter()
            .find(|i| i.id == DEVICE_INVITE_ID)
            .expect("expected device invite");
        let device_invite = nostr_double_ratchet::Invite::deserialize(&device.serialized).unwrap();
        assert!(device_invite.purpose.is_none());
        let inviter_hex = device_invite.inviter.to_hex();
        assert_eq!(
            device_invite.device_id.as_deref(),
            Some(inviter_hex.as_str())
        );
        assert!(device_invite.inviter_ephemeral_private_key.is_some());

        let updated = Config::load(_temp.path()).unwrap();
        assert!(updated.is_logged_in());
    }

    #[tokio::test]
    async fn test_accept_uses_owner_identity() {
        let (_temp, mut config, storage) = setup();
        let output = Output::new(true);

        let owner_keys = nostr::Keys::generate();
        let owner_pubkey_hex = owner_keys.public_key().to_hex();
        config
            .set_private_key(&hex::encode(owner_keys.secret_key().to_secret_bytes()))
            .unwrap();

        let device_keys = nostr::Keys::generate();
        let invite = build_link_invite(&device_keys.public_key().to_hex()).unwrap();
        let url = invite.get_url("https://iris.to").unwrap();

        // Ensure no relays are used in this test
        let mut config = config.clone();
        config.relays = Vec::new();
        config.save().unwrap();
        std::env::set_var("NOSTR_PREFER_LOCAL", "0");

        accept(&url, &config, &storage, &output).await.unwrap();

        let updated = Config::load(_temp.path()).unwrap();
        assert_eq!(updated.linked_owner, None);
        assert_eq!(updated.public_key().unwrap(), owner_pubkey_hex);

        let app_keys = storage.load_app_keys().unwrap().expect("expected app keys");
        let devices = app_keys.get_all_devices();
        assert!(devices
            .iter()
            .any(|d| d.identity_pubkey == owner_keys.public_key()));
        assert!(devices
            .iter()
            .any(|d| d.identity_pubkey == device_keys.public_key()));
    }

    #[tokio::test]
    async fn test_accept_autogenerates_owner_identity_when_missing() {
        let (temp, config, storage) = setup();
        let output = Output::new(true);

        assert!(!config.is_logged_in());

        let device_keys = nostr::Keys::generate();
        let invite = build_link_invite(&device_keys.public_key().to_hex()).unwrap();
        let url = invite.get_url("https://iris.to").unwrap();

        accept(&url, &config, &storage, &output).await.unwrap();

        let updated = Config::load(temp.path()).unwrap();
        assert!(updated.is_logged_in());

        let owner_pk_hex = updated.public_key().unwrap();
        let owner_pk = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pk_hex).unwrap();

        let app_keys = storage.load_app_keys().unwrap().expect("expected app keys");
        let devices = app_keys.get_all_devices();
        assert!(devices.iter().any(|d| d.identity_pubkey == owner_pk));
        assert!(devices
            .iter()
            .any(|d| d.identity_pubkey == device_keys.public_key()));
    }
}
