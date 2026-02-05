use anyhow::Result;
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::Storage;

#[derive(Serialize)]
struct LinkCreated {
    id: String,
    url: String,
    device_pubkey: String,
    generated_identity: bool,
}

#[derive(Serialize)]
struct LinkAccepted {
    owner_pubkey: String,
    device_pubkey: String,
    response_event: String,
}

fn build_link_invite(device_pubkey_hex: &str) -> Result<nostr_double_ratchet::Invite> {
    let device_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(device_pubkey_hex)?;
    let mut invite = nostr_double_ratchet::Invite::create_new(device_pubkey, None, None)?;
    invite.purpose = Some("link".to_string());
    Ok(invite)
}

/// Create a private link invite for a new device.
pub async fn create(config: &Config, storage: &Storage, output: &Output) -> Result<()> {
    let mut config = config.clone();
    let (device_pubkey_hex, generated_identity) = config.ensure_identity()?;
    let invite = build_link_invite(&device_pubkey_hex)?;
    let url = invite.get_url("https://iris.to")?;

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let stored = crate::storage::StoredInvite {
        id: id.clone(),
        label: Some("link".to_string()),
        url: url.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        serialized: invite.serialize()?,
    };

    storage.save_invite(&stored)?;

    output.success(
        "link.create",
        LinkCreated {
            id,
            url,
            device_pubkey: device_pubkey_hex,
            generated_identity,
        },
    );
    Ok(())
}

/// Accept a link invite and publish the response event.
pub async fn accept(
    url: &str,
    config: &Config,
    _storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let invite = nostr_double_ratchet::Invite::from_url(url)?;
    if invite.purpose.as_deref() != Some("link") {
        anyhow::bail!("Invite is not a link invite.");
    }

    let owner_pubkey_hex = config.public_key()?;
    let owner_pubkey =
        nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;
    let owner_private_key = config.private_key_bytes()?;

    let (_session, response_event) = invite.accept_with_owner(
        owner_pubkey,
        owner_private_key,
        None,
        Some(owner_pubkey),
    )?;

    // Publish to relays if configured
    let relays = config.resolved_relays();
    if !relays.is_empty() {
        let client = Client::default();
        for relay in &relays {
            client.add_relay(relay).await?;
        }
        client.connect().await;
        send_event_or_ignore(&client, response_event.clone()).await?;
    }

    output.success(
        "link.accept",
        LinkAccepted {
            owner_pubkey: owner_pubkey_hex,
            device_pubkey: invite.inviter.to_hex(),
            response_event: nostr::JsonUtil::as_json(&response_event),
        },
    );

    Ok(())
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
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        (temp, config, storage)
    }

    #[test]
    fn test_build_link_invite_sets_purpose() {
        let device_pubkey_hex =
            "5d21f6f47fbcdd6b8f0be05a9e9b4a1fdb2f4d9cced65c0b6f965c2a3a9a1b5c";

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
        assert_eq!(invites.len(), 1);
        let invite =
            nostr_double_ratchet::Invite::deserialize(&invites[0].serialized).unwrap();
        assert_eq!(invite.purpose.as_deref(), Some("link"));
        assert!(invite.owner_public_key.is_none());

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
    }
}
