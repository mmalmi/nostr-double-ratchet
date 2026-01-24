use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredInvite};

#[derive(Serialize)]
struct InviteCreated {
    id: String,
    url: String,
    label: Option<String>,
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

    output.success("invite.create", InviteCreated {
        id,
        url,
        label,
    });

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

    output.success("invite.list", InviteList { invites: invite_infos });
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

/// Listen for invite acceptances
pub async fn listen(
    config: &Config,
    _storage: &Storage,
    output: &Output,
) -> Result<()> {
    if !config.is_logged_in() {
        anyhow::bail!("Not logged in. Use 'ndr login <key>' first.");
    }

    output.success_message("invite.listen", "Listening for invite acceptances... (Ctrl+C to stop)");

    // TODO: Implement actual listening with nostr-sdk
    // For now, just a placeholder
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Config, Storage) {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();
        // Login with test key
        config.set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap();
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

        create(Some("One".to_string()), &config, &storage, &output).await.unwrap();
        create(Some("Two".to_string()), &config, &storage, &output).await.unwrap();

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
