use anyhow::{Context, Result};
use std::net::SocketAddr;

use crate::config::Config;
use crate::storage::Storage;

pub type NearbyService = nostr_double_ratchet::NearbyLanService;

pub async fn start(
    config: &Config,
    _storage: &Storage,
    peer_id: impl Into<String>,
) -> Result<Option<NearbyService>> {
    if !config.nearby_enabled() {
        return Ok(None);
    }

    let mut lan_config = nostr_double_ratchet::NearbyLanConfig::new(peer_id);
    lan_config.bind_addr = config.nearby_bind_addr()?;
    lan_config.explicit_peers = config
        .nearby_peer_addresses()
        .into_iter()
        .filter(nostr_double_ratchet::is_allowed_nearby_peer)
        .collect();

    nostr_double_ratchet::NearbyLanService::start(lan_config)
        .await
        .map(Some)
        .context("Failed to start nearby LAN")
}

pub fn is_allowed_nearby_peer(addr: &SocketAddr) -> bool {
    nostr_double_ratchet::is_allowed_nearby_peer(addr)
}
