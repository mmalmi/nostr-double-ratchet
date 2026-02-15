//! E2E test: commands that require an identity should auto-generate one on first run.

mod common;

use tempfile::TempDir;
use tokio::process::Command;

fn setup_ndr_dir(relay_url: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let config_content = serde_json::json!({
        "relays": [relay_url]
    });
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn test_group_create_autogenerates_identity_without_warnings() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Fresh profile: config has relays but no private key.
    let dir = setup_ndr_dir(&relay_url);

    let member = nostr::Keys::generate().public_key().to_hex();

    let output = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(dir.path())
        .args(["group", "create", "--name", "Test", "--members", &member])
        .output()
        .await
        .expect("Failed to run ndr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "ndr failed: stderr={stderr} stdout={stdout}"
    );
    assert!(
        !stderr.contains("Generated new identity"),
        "unexpected autogen warning: {stderr}"
    );

    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse JSON output: {e}\nOutput: {stdout}"));
    assert_eq!(v["status"], "ok");

    let cfg_raw = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    let cfg: serde_json::Value = serde_json::from_str(&cfg_raw).unwrap();
    let sk = cfg["private_key"].as_str().expect("expected private_key");
    assert_eq!(sk.len(), 64);

    relay.stop().await;
}
