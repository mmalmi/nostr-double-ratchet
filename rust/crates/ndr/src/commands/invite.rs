use anyhow::Result;
use nostr_sdk::Client;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredChat, StoredInvite};

#[derive(Serialize)]
struct InviteCreated {
    id: String,
    url: String,
    label: Option<String>,
}

#[derive(Serialize)]
struct InvitePublished {
    id: String,
    url: String,
    label: Option<String>,
    device_id: String,
    event: String,
}

#[derive(Serialize)]
struct InviteList {
    invites: Vec<InviteInfo>,
}

#[derive(Serialize)]
struct InviteInfo {
    id: String,
    label: Option<String>,
    url: String,
    created_at: u64,
}

/// Create a new invite
pub async fn create(
    label: Option<String>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let pubkey_hex = config.public_key()?;
    let pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&pubkey_hex)?;

    // Create invite using nostr-double-ratchet
    let invite = nostr_double_ratchet::Invite::create_new(pubkey, None, None)?;
    let url = invite.get_url("https://iris.to")?;
    let serialized = invite.serialize()?;

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let stored = StoredInvite {
        id: id.clone(),
        label: label.clone(),
        url: url.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        serialized,
    };

    storage.save_invite(&stored)?;

    output.success("invite.create", InviteCreated { id, url, label });

    Ok(())
}

fn normalize_device_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }
    trimmed.replace(' ', "-")
}

/// Create and publish a new invite event
pub async fn publish(
    label: Option<String>,
    device_id: Option<String>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    let pubkey_hex = config.public_key()?;
    let pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&pubkey_hex)?;

    let device_id = device_id.unwrap_or_else(|| "public".to_string());
    let device_id = normalize_device_id(&device_id);

    let invite = nostr_double_ratchet::Invite::create_new(pubkey, Some(device_id.clone()), None)?;
    let url = invite.get_url("https://iris.to")?;
    let serialized = invite.serialize()?;

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let stored = StoredInvite {
        id: id.clone(),
        label: label.clone(),
        url: url.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        serialized,
    };
    storage.save_invite(&stored)?;

    // Build and sign invite event
    let unsigned = invite.get_event()?;
    let sk_bytes = config.private_key_bytes()?;
    let sk = nostr::SecretKey::from_slice(&sk_bytes)?;
    let keys = nostr::Keys::new(sk);
    let event = unsigned
        .sign_with_keys(&keys)
        .map_err(|e| anyhow::anyhow!("Failed to sign invite event: {}", e))?;

    // Publish to relays
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    send_event_or_ignore(&client, event.clone()).await?;

    output.success(
        "invite.publish",
        InvitePublished {
            id,
            url,
            label,
            device_id,
            event: nostr::JsonUtil::as_json(&event),
        },
    );

    Ok(())
}

/// List all invites
pub async fn list(storage: &Storage, output: &Output) -> Result<()> {
    let invites = storage.list_invites()?;

    let invite_infos: Vec<InviteInfo> = invites
        .into_iter()
        .map(|i| InviteInfo {
            id: i.id,
            label: i.label,
            url: i.url,
            created_at: i.created_at,
        })
        .collect();

    output.success(
        "invite.list",
        InviteList {
            invites: invite_infos,
        },
    );
    Ok(())
}

/// Delete an invite
pub async fn delete(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    if storage.delete_invite(id)? {
        output.success_message("invite.delete", &format!("Deleted invite {}", id));
    } else {
        anyhow::bail!("Invite not found: {}", id);
    }
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

#[derive(Serialize)]
struct InviteAccepted {
    invite_id: String,
    chat_id: String,
    their_pubkey: String,
}

/// Process an invite acceptance event (creates a chat session for the inviter)
pub async fn accept(
    invite_id: &str,
    event_json: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    // Get our private key
    let our_private_key = config.private_key_bytes()?;

    // Load the invite
    let stored_invite = storage
        .get_invite(invite_id)?
        .ok_or_else(|| anyhow::anyhow!("Invite not found: {}", invite_id))?;

    // Deserialize the invite
    let invite = nostr_double_ratchet::Invite::deserialize(&stored_invite.serialized)?;

    // Parse the acceptance event
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json)
        .map_err(|e| anyhow::anyhow!("Invalid event JSON: {}", e))?;

    // Process the acceptance - creates session
    let result = invite.process_invite_response(&event, our_private_key)?;

    let (session, their_pubkey, _device_id) =
        result.ok_or_else(|| anyhow::anyhow!("Failed to process invite acceptance"))?;

    // Serialize session state
    let session_state = serde_json::to_string(&session.state)?;

    let chat_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let their_pubkey_hex = hex::encode(their_pubkey.to_bytes());

    let chat = StoredChat {
        id: chat_id.clone(),
        their_pubkey: their_pubkey_hex.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        last_message_at: None,
        session_state,
    };

    storage.save_chat(&chat)?;

    // Optionally delete the used invite
    storage.delete_invite(invite_id)?;

    output.success(
        "invite.accept",
        InviteAccepted {
            invite_id: invite_id.to_string(),
            chat_id,
            their_pubkey: their_pubkey_hex,
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        // Login with test key
        config
            .set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .unwrap();
        let config = Config::load(temp.path()).unwrap();
        let storage = Storage::open(temp.path()).unwrap();
        (temp, config, storage)
    }

    #[tokio::test]
    async fn test_create_invite() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        create(Some("Test".to_string()), &config, &storage, &output)
            .await
            .unwrap();

        let invites = storage.list_invites().unwrap();
        assert_eq!(invites.len(), 1);
        assert_eq!(invites[0].label, Some("Test".to_string()));
    }

    #[tokio::test]
    async fn test_list_invites() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        create(Some("One".to_string()), &config, &storage, &output)
            .await
            .unwrap();
        create(Some("Two".to_string()), &config, &storage, &output)
            .await
            .unwrap();

        list(&storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_delete_invite() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        create(None, &config, &storage, &output).await.unwrap();

        let invites = storage.list_invites().unwrap();
        let id = &invites[0].id;

        delete(id, &storage, &output).await.unwrap();

        assert!(storage.list_invites().unwrap().is_empty());
    }
}
