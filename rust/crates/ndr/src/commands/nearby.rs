use anyhow::Result;

use crate::config::Config;
use crate::nearby::is_allowed_nearby_peer;
use crate::output::Output;

pub async fn status(config: &Config, output: &Output) -> Result<()> {
    output.success(
        "nearby",
        serde_json::json!({
            "enabled": config.nearby_enabled(),
            "configured_enabled": config.nearby_enabled,
            "bind": config.nearby_bind_addr()?.map(|addr| addr.to_string()).unwrap_or_else(|| "auto".to_string()),
            "peers": config.nearby_peers,
        }),
    );
    Ok(())
}

pub async fn set_enabled(config: &mut Config, enabled: bool, output: &Output) -> Result<()> {
    config.set_nearby_enabled(enabled)?;
    status(config, output).await
}

pub async fn peers(config: &Config, output: &Output) -> Result<()> {
    output.success(
        "nearby_peers",
        serde_json::json!({
            "peers": config.nearby_peers,
            "env_peers": config
                .nearby_peer_addresses()
                .into_iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>(),
        }),
    );
    Ok(())
}

pub async fn add_peer(config: &mut Config, address: &str, output: &Output) -> Result<()> {
    let addr: std::net::SocketAddr = address
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid nearby peer address: {address}"))?;
    if !is_allowed_nearby_peer(&addr) {
        anyhow::bail!("Nearby peers must be loopback or private LAN addresses");
    }
    config.add_nearby_peer(&addr.to_string())?;
    peers(config, output).await
}

pub async fn remove_peer(config: &mut Config, address: &str, output: &Output) -> Result<()> {
    config.remove_nearby_peer(address)?;
    peers(config, output).await
}
