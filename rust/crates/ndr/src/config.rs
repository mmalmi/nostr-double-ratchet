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
        "wss://temp.iris.to".to_string(),
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

    /// Ensure we have an identity, auto-generating one if needed.
    /// Returns (public_key_hex, was_generated)
    pub fn ensure_identity(&mut self) -> Result<(String, bool)> {
        if self.private_key.is_some() {
            return Ok((self.public_key()?, false));
        }

        // Generate a new keypair
        let keys = nostr::Keys::generate();
        let sk_hex = keys.secret_key().to_secret_hex();
        self.private_key = Some(sk_hex);
        self.save()?;

        Ok((keys.public_key().to_hex(), true))
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

    /// Resolve relays with environment overrides and optional local relay detection.
    pub fn resolved_relays(&self) -> Vec<String> {
        resolve_relays(&self.relays)
    }
}

fn resolve_relays(config_relays: &[String]) -> Vec<String> {
    let mut base = match parse_env_list("NOSTR_RELAYS") {
        Some(list) => list,
        None => config_relays.to_vec(),
    };

    base = base
        .into_iter()
        .filter_map(|r| normalize_relay_url(&r))
        .collect();

    if !prefer_local_relay() {
        return dedupe_relays(base);
    }

    let mut combined = detect_local_relay_urls();
    combined.extend(base);
    dedupe_relays(combined)
}

fn detect_local_relay_urls() -> Vec<String> {
    let mut relays = Vec::new();

    if let Some(list) = parse_env_list("NOSTR_LOCAL_RELAY")
        .or_else(|| parse_env_list("HTREE_LOCAL_RELAY"))
    {
        for raw in list {
            if let Some(url) = normalize_relay_url(&raw) {
                relays.push(url);
            }
        }
    }

    if let Some(port) = local_daemon_port() {
        if local_port_open(port) {
            relays.push(format!("ws://127.0.0.1:{port}/ws"));
        }
    }

    let mut ports = parse_env_ports("NOSTR_LOCAL_RELAY_PORTS");
    if ports.is_empty() {
        ports.push(4869);
    }

    for port in ports {
        if port == 0 {
            continue;
        }
        if local_port_open(port) {
            relays.push(format!("ws://127.0.0.1:{port}"));
        }
    }

    dedupe_relays(relays)
}

fn local_daemon_port() -> Option<u16> {
    if let Ok(addr) = std::env::var("HTREE_DAEMON_ADDR") {
        if let Some(port) = parse_port(&addr) {
            return Some(port);
        }
    }
    if let Ok(url) = std::env::var("HTREE_DAEMON_URL") {
        if let Some(port) = parse_port(&url) {
            return Some(port);
        }
    }
    Some(8080)
}

fn parse_port(addr: &str) -> Option<u16> {
    if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
        return Some(sock.port());
    }
    if let Some((_, port_str)) = addr.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            return Some(port);
        }
    }
    None
}

fn prefer_local_relay() -> bool {
    for key in ["NOSTR_PREFER_LOCAL", "HTREE_PREFER_LOCAL_RELAY"] {
        if let Ok(val) = std::env::var(key) {
            let val = val.trim().to_lowercase();
            return !matches!(val.as_str(), "0" | "false" | "no" | "off");
        }
    }
    true
}

fn parse_env_list(var: &str) -> Option<Vec<String>> {
    let value = std::env::var(var).ok()?;
    let mut items = Vec::new();
    for part in value.split(|c| c == ',' || c == ';' || c == '\n' || c == '\t' || c == ' ') {
        let trimmed = part.trim();
        if !trimmed.is_empty() {
            items.push(trimmed.to_string());
        }
    }
    if items.is_empty() { None } else { Some(items) }
}

fn parse_env_ports(var: &str) -> Vec<u16> {
    let Some(list) = parse_env_list(var) else {
        return Vec::new();
    };
    list.into_iter()
        .filter_map(|item| item.parse::<u16>().ok())
        .collect()
}

fn normalize_relay_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let trimmed = trimmed.trim_end_matches('/');
    let lower = trimmed.to_lowercase();
    if lower.starts_with("ws://") || lower.starts_with("wss://") {
        return Some(trimmed.to_string());
    }
    if lower.starts_with("http://") {
        return Some(format!("ws://{}", &trimmed[7..]));
    }
    if lower.starts_with("https://") {
        return Some(format!("wss://{}", &trimmed[8..]));
    }
    Some(format!("ws://{}", trimmed))
}

fn local_port_open(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = Duration::from_millis(100);
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

fn dedupe_relays(relays: Vec<String>) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for relay in relays {
        let key = relay.trim_end_matches('/').to_lowercase();
        if seen.insert(key) {
            out.push(relay);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn clear(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(prev) = &self.prev {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
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

    #[test]
    fn test_resolved_relays_prefers_local() {
        let _lock = ENV_LOCK.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let _prefer = EnvGuard::set("NOSTR_PREFER_LOCAL", "1");
        let _ports = EnvGuard::set("NOSTR_LOCAL_RELAY_PORTS", &port.to_string());
        let _relays = EnvGuard::clear("NOSTR_RELAYS");

        let config = Config {
            relays: vec!["wss://relay.example".to_string()],
            ..Config::default()
        };

        let resolved = config.resolved_relays();
        assert!(!resolved.is_empty());
        assert_eq!(resolved[0], format!("ws://127.0.0.1:{port}"));
        assert!(resolved.contains(&"wss://relay.example".to_string()));
    }

    #[test]
    fn test_resolved_relays_env_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _prefer = EnvGuard::set("NOSTR_PREFER_LOCAL", "0");
        let _relays = EnvGuard::set("NOSTR_RELAYS", "wss://relay.one,wss://relay.two");

        let config = Config {
            relays: vec!["wss://relay.example".to_string()],
            ..Config::default()
        };

        let resolved = config.resolved_relays();
        assert_eq!(
            resolved,
            vec!["wss://relay.one".to_string(), "wss://relay.two".to_string()]
        );
    }
}
