//! E2E test: ndr CLI creates invite <-> TypeScript accepts
//!
//! This tests the reverse direction: ndr creates the invite, TypeScript accepts.
//! This catches the bug where ndr listen was filtering invite responses by the
//! main pubkey instead of the ephemeral pubkey from the invite.

mod common;

use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .await
        .expect("Failed to run ndr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        panic!("ndr failed: stdout={} stderr={}", stdout, stderr);
    }

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

/// Start ndr listen in background
async fn start_ndr_listen(
    data_dir: &std::path::Path,
) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Start the TypeScript e2e script that accepts an ndr invite
async fn start_ts_accept_script(
    relay_url: &str,
    invite_url: &str,
) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let ts_dir = repo_root.join("ts");

    let mut child = Command::new("npx")
        .arg("tsx")
        .arg("e2e/rust-ts-e2e.ts")
        .arg(relay_url)
        .arg(invite_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Read lines from stdout until we find a specific marker
async fn read_until_marker(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    prefix: &str,
) -> Option<String> {
    let timeout = Duration::from_secs(30);

    tokio::time::timeout(timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    println!("[output] {}", trimmed);
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        return Some(rest.to_string());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn test_ndr_creates_invite_ts_accepts() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup ndr (Alice - creates the invite)
    let alice_dir = TempDir::new().unwrap();
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // Configure ndr to use our test relay
    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        alice_dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    // Alice logs in
    let result = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    assert_eq!(result["status"], "ok", "Alice login failed");

    // Alice creates an invite
    let result = run_ndr(alice_dir.path(), &["invite", "create", "-l", "test"]).await;
    assert_eq!(result["status"], "ok", "Alice invite create failed");
    let invite_url = result["data"]["url"].as_str().unwrap().to_string();
    let invite_id = result["data"]["id"].as_str().unwrap().to_string();
    println!("Alice created invite: {} (id: {})", invite_url, invite_id);

    // Start ndr listen for Alice - this should pick up the invite response
    let (mut alice_listen_child, mut alice_listen_reader) =
        start_ndr_listen(alice_dir.path()).await;

    // Give ndr listen time to connect
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Start TypeScript to accept the invite
    let (mut ts_child, mut ts_reader) = start_ts_accept_script(&relay_url, &invite_url).await;

    // Wait for TypeScript to connect and accept
    let _ = read_until_marker(&mut ts_reader, "E2E_RELAY_CONNECTED:")
        .await
        .expect("TypeScript failed to connect to relay");
    println!("TypeScript connected to relay");

    let _ = read_until_marker(&mut ts_reader, "E2E_INVITE_ACCEPTED")
        .await
        .expect("TypeScript failed to accept invite");
    println!("TypeScript accepted the invite");

    let _ = read_until_marker(&mut ts_reader, "E2E_RESPONSE_PUBLISHED")
        .await
        .expect("TypeScript failed to publish response");
    println!("TypeScript published invite response");

    // Now ndr listen should receive the invite response and create a session
    // Look for the session_created event in ndr listen output
    let mut session_created = false;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(15) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            alice_listen_reader.read_line(&mut line),
        )
        .await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[ndr listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "session_created" {
                        println!("ndr received invite response and created session!");
                        session_created = true;
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = alice_listen_child.kill().await;
    let _ = ts_child.kill().await;
    relay.stop().await;

    assert!(
        session_created,
        "ndr listen did not receive the invite response from TypeScript. \
         This was the bug: ndr was filtering by main pubkey instead of ephemeral pubkey."
    );

    println!("E2E test passed: ndr creates invite, TypeScript accepts, ndr receives response!");
}
