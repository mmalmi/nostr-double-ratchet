use anyhow::{Context, Result};
use nostr::ToBech32;
use serde::Serialize;
use std::path::Path;

use crate::config::Config;
use crate::output::Output;
use crate::storage::Storage;

#[derive(Serialize)]
struct LoginResult {
    pubkey: String,
    npub: String,
}

#[derive(Serialize)]
struct WhoamiResult {
    pubkey: String,
    npub: String,
    logged_in: bool,
}

/// Login with a private key (nsec or hex)
pub async fn login(key: &str, config: &Config, _storage: &Storage, output: &Output) -> Result<()> {
    // Parse the key - accept nsec or hex
    let hex_key = if key.starts_with("nsec1") {
        // Decode bech32 nsec
        use nostr::nips::nip19::FromBech32;
        let sk = nostr::SecretKey::from_bech32(key).context("Invalid nsec key")?;
        hex::encode(sk.to_secret_bytes())
    } else {
        // Assume hex
        if key.len() != 64 {
            anyhow::bail!("Invalid key length. Expected 64 hex characters or nsec.");
        }
        // Validate it's valid hex
        hex::decode(key).context("Invalid hex key")?;
        key.to_string()
    };

    // Verify the key is valid
    let sk = nostr::SecretKey::from_slice(&hex::decode(&hex_key)?).context("Invalid secret key")?;
    let keys = nostr::Keys::new(sk);
    let pubkey = keys.public_key();

    // Save to config
    let mut config = config.clone();
    config.set_private_key(&hex_key)?;

    let result = LoginResult {
        pubkey: pubkey.to_hex(),
        npub: pubkey.to_bech32().unwrap_or_default(),
    };

    output.success("login", result);
    Ok(())
}

/// Logout and clear all data
pub async fn logout(data_dir: &Path, output: &Output) -> Result<()> {
    // Clear config
    let mut config = Config::load(data_dir)?;
    config.clear_private_key()?;

    // Clear storage
    let storage = Storage::open(data_dir)?;
    storage.clear_all()?;

    output.success_message("logout", "Logged out and cleared all data");
    Ok(())
}

/// Show current identity
pub async fn whoami(config: &Config, output: &Output) -> Result<()> {
    if !config.is_logged_in() {
        let result = WhoamiResult {
            pubkey: String::new(),
            npub: String::new(),
            logged_in: false,
        };
        output.success("whoami", result);
        return Ok(());
    }

    let pubkey = config.public_key()?;
    let pk = nostr::PublicKey::from_hex(&pubkey)?;

    let result = WhoamiResult {
        pubkey,
        npub: pk.to_bech32().unwrap_or_default(),
        logged_in: true,
    };

    output.success("whoami", result);
    Ok(())
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

    #[tokio::test]
    async fn test_login_with_hex() {
        let (temp, config, storage) = setup();
        let output = Output::new(true);

        // Valid 32-byte hex key
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        login(key, &config, &storage, &output).await.unwrap();

        // Verify config was saved
        let loaded = Config::load(temp.path()).unwrap();
        assert!(loaded.is_logged_in());
    }

    #[tokio::test]
    async fn test_login_with_nsec() {
        let (_temp, config, storage) = setup();
        let output = Output::new(true);

        // Generate a valid nsec
        let keys = nostr::Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();

        login(&nsec, &config, &storage, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_logout_clears_data() {
        let (temp, mut config, storage) = setup();
        let output = Output::new(true);

        // Login first
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        config.set_private_key(key).unwrap();

        // Add some data
        storage
            .save_invite(&crate::storage::StoredInvite {
                id: "test".to_string(),
                label: None,
                url: "".to_string(),
                created_at: 0,
                serialized: "".to_string(),
            })
            .unwrap();

        logout(temp.path(), &output).await.unwrap();

        // Verify cleared
        let loaded = Config::load(temp.path()).unwrap();
        assert!(!loaded.is_logged_in());
        assert!(storage.list_invites().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_whoami_logged_in() {
        let (temp, mut config, _storage) = setup();
        let output = Output::new(true);

        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        config.set_private_key(key).unwrap();

        let config = Config::load(temp.path()).unwrap();
        whoami(&config, &output).await.unwrap();
    }

    #[tokio::test]
    async fn test_whoami_not_logged_in() {
        let (_temp, config, _storage) = setup();
        let output = Output::new(true);

        whoami(&config, &output).await.unwrap();
    }
}
