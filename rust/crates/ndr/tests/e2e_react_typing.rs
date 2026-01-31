//! E2E test: reactions and typing between ndr CLI and TypeScript
//!
//! Flow:
//! 1. TS creates invite, ndr joins
//! 2. ndr sends first message (initiator) -> TS receives it
//! 3. TS sends a reply -> ndr receives it via listen
//! 4. ndr reacts to the reply, sends typing, sends follow-up message
//! 5. TS verifies it received the reaction, typing indicator, and follow-up

mod common;

use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
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

    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout)
    })
}

async fn start_ts_script(relay_url: &str) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .parent().unwrap()
        .to_path_buf();

    let ts_dir = repo_root.join("ts");

    let mut child = Command::new("npx")
        .arg("tsx")
        .arg("e2e/react-typing-e2e.ts")
        .arg(relay_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

async fn read_until_marker(reader: &mut BufReader<tokio::process::ChildStdout>, prefix: &str) -> Option<String> {
    let timeout = Duration::from_secs(30);

    tokio::time::timeout(timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return None,
                Ok(_) => {
                    let trimmed = line.trim();
                    println!("[TS] {}", trimmed);
                    if trimmed.starts_with(prefix) {
                        return Some(trimmed[prefix.len()..].to_string());
                    }
                }
                Err(_) => return None,
            }
        }
    }).await.ok().flatten()
}

#[tokio::test]
async fn test_react_and_typing_e2e() {
    // Start relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Start TS script
    let (mut ts_child, mut ts_reader) = start_ts_script(&relay_url).await;

    // Wait for TS to be ready
    let invite_url = read_until_marker(&mut ts_reader, "E2E_INVITE_URL:").await
        .expect("Failed to get invite URL");
    let _listening = read_until_marker(&mut ts_reader, "E2E_LISTENING").await
        .expect("TS not listening");
    println!("TS ready, invite URL: {}", invite_url);

    // Setup ndr (Bob)
    let bob_dir = TempDir::new().unwrap();
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    let config_content = serde_json::json!({ "relays": [&relay_url] });
    std::fs::write(
        bob_dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap()
    ).unwrap();

    // Bob logs in and joins
    let result = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    assert_eq!(result["status"], "ok", "Bob login failed");

    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let bob_chat_id = result["data"]["id"].as_str().unwrap().to_string();
    let response_event = result["data"]["response_event"].as_str().unwrap().to_string();
    println!("Bob joined chat: {}", bob_chat_id);

    // Publish response event to relay manually for reliability
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    use futures::{SinkExt, StreamExt};

    let (mut ws, _) = connect_async(&relay_url).await.expect("Failed to connect to relay");
    let event_msg = format!(r#"["EVENT",{}]"#, response_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send event");
    if let Some(Ok(Message::Text(response))) = ws.next().await {
        println!("Relay response: {}", response);
    }

    // Wait for TS to create session
    let _session_created = read_until_marker(&mut ts_reader, "E2E_SESSION_CREATED:").await
        .expect("TS failed to create session");
    println!("Session created");

    // ndr sends first message (ndr is initiator)
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Hello from ndr!"]).await;
    assert_eq!(result["status"], "ok", "ndr first send failed");
    let first_event = result["data"]["event"].as_str().unwrap().to_string();

    // Publish first message to relay
    let event_msg = format!(r#"["EVENT",{}]"#, first_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send first message");
    println!("ndr sent first message");

    // Wait for TS to receive first message and send reply
    let _got_first = read_until_marker(&mut ts_reader, "E2E_GOT_FIRST_MESSAGE:").await
        .expect("TS did not receive first message");
    println!("TS received first message");

    let reply_id = read_until_marker(&mut ts_reader, "E2E_REPLY_SENT:id=").await
        .expect("TS did not send reply");
    println!("TS sent reply with id: {}", reply_id);

    // Start ndr listen to receive the reply from TS
    let mut listen_child = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(bob_dir.path())
        .arg("listen")
        .arg("--chat")
        .arg(&bob_chat_id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen");

    let mut listen_reader = BufReader::new(listen_child.stdout.take().unwrap());

    let mut received_msg_id = String::new();
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(15) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(200),
            listen_reader.read_line(&mut line)
        ).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[ndr listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "message" {
                        received_msg_id = json["message_id"].as_str().unwrap_or("").to_string();
                        println!("ndr received reply: {}", received_msg_id);
                        break;
                    }
                }
            }
        }
    }
    let _ = listen_child.kill().await;
    assert!(!received_msg_id.is_empty(), "ndr did not receive the reply from TS");

    // ndr reacts to the reply
    let result = run_ndr(bob_dir.path(), &["react", &bob_chat_id, &received_msg_id, "👍"]).await;
    assert_eq!(result["status"], "ok", "ndr react failed");
    let react_event = result["data"]["event"].as_str().unwrap().to_string();
    println!("ndr sent reaction");

    // Publish reaction to relay
    let event_msg = format!(r#"["EVENT",{}]"#, react_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send reaction");

    // ndr sends typing indicator
    let result = run_ndr(bob_dir.path(), &["typing", &bob_chat_id]).await;
    assert_eq!(result["status"], "ok", "ndr typing failed");
    let typing_event = result["data"]["event"].as_str().unwrap().to_string();
    println!("ndr sent typing indicator");

    // Publish typing event to relay
    let event_msg = format!(r#"["EVENT",{}]"#, typing_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send typing event");

    // ndr sends a follow-up message
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Follow-up!"]).await;
    assert_eq!(result["status"], "ok", "ndr follow-up send failed");
    let followup_event = result["data"]["event"].as_str().unwrap().to_string();

    // Publish follow-up to relay
    let event_msg = format!(r#"["EVENT",{}]"#, followup_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send follow-up");
    println!("ndr sent follow-up message");

    // Wait for TS to confirm it received reaction, typing, and follow-up
    let reaction_ok = read_until_marker(&mut ts_reader, "E2E_REACTION_OK:").await;
    assert!(reaction_ok.is_some(), "TS did not receive reaction");
    println!("TS received reaction: {:?}", reaction_ok);

    let typing_ok = read_until_marker(&mut ts_reader, "E2E_TYPING_OK").await;
    assert!(typing_ok.is_some(), "TS did not receive typing indicator");
    println!("TS received typing indicator");

    let followup_ok = read_until_marker(&mut ts_reader, "E2E_FOLLOWUP_OK:").await;
    assert!(followup_ok.is_some(), "TS did not receive follow-up after reaction/typing");
    println!("TS received follow-up: {:?}", followup_ok);

    let all_ok = read_until_marker(&mut ts_reader, "E2E_ALL_OK").await;
    assert!(all_ok.is_some(), "TS did not report all OK");

    // Cleanup
    let _ = ts_child.kill().await;
    relay.stop().await;

    println!("Reaction + Typing E2E test passed!");
}
