use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::nostr_client::{connect_client, send_event_or_ignore};
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
    let url = invite.get_url("https://chat.iris.to")?;
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

    // For multi-device compatibility, default device_id to our identity pubkey.
    let device_id = match device_id {
        Some(id) => normalize_device_id(&id),
        None => pubkey_hex.clone(),
    };

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
    let client = connect_client(config).await?;
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

#[derive(Serialize)]
struct InviteAccepted {
    invite_id: String,
    chat_id: String,
    their_pubkey: String,
}

#[derive(Serialize)]
struct LinkInviteAccepted {
    invite_id: String,
    owner_pubkey: String,
    device_pubkey: String,
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

    let response = result.ok_or_else(|| anyhow::anyhow!("Failed to process invite acceptance"))?;

    if invite.purpose.as_deref() == Some("link") {
        let owner_pubkey = response
            .owner_public_key
            .unwrap_or(response.invitee_identity);
        let owner_pubkey_hex = owner_pubkey.to_hex();
        let mut config = config.clone();
        config.set_linked_owner(&owner_pubkey_hex)?;

        storage.delete_invite(invite_id)?;

        output.success(
            "link.accepted",
            LinkInviteAccepted {
                invite_id: invite_id.to_string(),
                owner_pubkey: owner_pubkey_hex,
                device_pubkey: invite.inviter.to_hex(),
            },
        );

        return Ok(());
    }

    let session = response.session;
    let their_pubkey = response
        .owner_public_key
        .unwrap_or(response.invitee_identity);

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
        message_ttl_seconds: None,
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

    #[tokio::test]
    async fn test_accept_link_invite_sets_linked_owner() {
        let (temp, config, storage) = setup();
        let output = Output::new(true);

        let device_pubkey_hex = config.public_key().unwrap();
        let device_pubkey =
            nostr_double_ratchet::utils::pubkey_from_hex(&device_pubkey_hex).unwrap();

        let mut invite =
            nostr_double_ratchet::Invite::create_new(device_pubkey, None, None).unwrap();
        invite.purpose = Some("link".to_string());
        let serialized = invite.serialize().unwrap();

        storage
            .save_invite(&StoredInvite {
                id: "link".to_string(),
                label: Some("link".to_string()),
                url: invite.get_url("https://iris.to").unwrap(),
                created_at: 0,
                serialized,
            })
            .unwrap();

        let owner_keys = nostr::Keys::generate();
        let owner_pubkey = owner_keys.public_key();

        let (_session, response_event) = invite
            .accept_with_owner(
                owner_pubkey,
                owner_keys.secret_key().to_secret_bytes(),
                None,
                Some(owner_pubkey),
            )
            .unwrap();

        accept(
            "link",
            &nostr::JsonUtil::as_json(&response_event),
            &config,
            &storage,
            &output,
        )
        .await
        .unwrap();

        let updated = Config::load(temp.path()).unwrap();
        assert_eq!(updated.linked_owner, Some(owner_pubkey.to_hex()));
        assert!(storage.list_chats().unwrap().is_empty());
    }
}
