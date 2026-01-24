use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// CLI configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// User's private key (hex encoded)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,

    /// Default relays
    #[serde(default = "default_relays")]
    pub relays: Vec<String>,

    /// Path to the config file
    #[serde(skip)]
    pub path: PathBuf,
}

fn default_relays() -> Vec<String> {
    vec![
        "wss://relay.damus.io".to_string(),
        "wss://nos.lol".to_string(),
        "wss://relay.primal.net".to_string(),
        "wss://relay.snort.social".to_string(),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            private_key: None,
            relays: default_relays(),
            path: PathBuf::new(),
        }
    }
}

impl Config {
    /// Load config from the data directory
    pub fn load(data_dir: &Path) -> Result<Self> {
        let config_path = data_dir.join("config.json");

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .context("Failed to read config file")?;
            let mut config: Config = serde_json::from_str(&content)
                .context("Failed to parse config file")?;
            config.path = config_path;
            Ok(config)
        } else {
            Ok(Config {
                path: config_path,
                ..Default::default()
            })
        }
    }

    /// Save config to disk
    pub fn save(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(self)
            .context("Failed to serialize config")?;
        std::fs::write(&self.path, content)
            .context("Failed to write config file")?;
        Ok(())
    }

    /// Set the private key and save
    pub fn set_private_key(&mut self, key: &str) -> Result<()> {
        self.private_key = Some(key.to_string());
        self.save()
    }

    /// Clear the private key and save
    pub fn clear_private_key(&mut self) -> Result<()> {
        self.private_key = None;
        self.save()
    }

    /// Check if logged in
    pub fn is_logged_in(&self) -> bool {
        self.private_key.is_some()
    }

    /// Get the private key bytes
    pub fn private_key_bytes(&self) -> Result<[u8; 32]> {
        let key = self.private_key.as_ref()
            .context("Not logged in")?;
        let bytes = hex::decode(key)
            .context("Invalid private key format")?;
        bytes.try_into()
            .map_err(|_| anyhow::anyhow!("Private key must be 32 bytes"))
    }

    /// Get the public key (hex)
    pub fn public_key(&self) -> Result<String> {
        let sk_bytes = self.private_key_bytes()?;
        let sk = nostr::SecretKey::from_slice(&sk_bytes)?;
        let keys = nostr::Keys::new(sk);
        Ok(keys.public_key().to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.private_key.is_none());
        assert!(!config.relays.is_empty());
        assert!(!config.is_logged_in());
    }

    #[test]
    fn test_config_load_nonexistent() {
        let temp = TempDir::new().unwrap();
        let config = Config::load(temp.path()).unwrap();
        assert!(config.private_key.is_none());
    }

    #[test]
    fn test_config_save_and_load() {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();

        // Set a key
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        config.set_private_key(test_key).unwrap();

        // Load again
        let loaded = Config::load(temp.path()).unwrap();
        assert_eq!(loaded.private_key, Some(test_key.to_string()));
        assert!(loaded.is_logged_in());
    }

    #[test]
    fn test_config_clear_private_key() {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();

        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        config.set_private_key(test_key).unwrap();
        assert!(config.is_logged_in());

        config.clear_private_key().unwrap();
        assert!(!config.is_logged_in());
    }

    #[test]
    fn test_config_public_key() {
        let temp = TempDir::new().unwrap();
        let mut config = Config::load(temp.path()).unwrap();

        // Use a known test key
        let test_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        config.set_private_key(test_key).unwrap();

        let pubkey = config.public_key().unwrap();
        assert_eq!(pubkey.len(), 64); // Hex public key is 64 chars
    }
}
