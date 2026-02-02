//! E2E test: ndr listen handles group metadata and group messages over WebSocket relay

mod common;

use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &Path, args: &[&str]) -> serde_json::Value {
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

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

/// Start ndr listen in background and return (child, stdout_reader)
async fn start_ndr_listen(data_dir: &Path) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Setup ndr data directory with config for test relay
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

async fn read_until_command(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    command: &str,
    timeout: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return false, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json.get("command").and_then(|v| v.as_str()) == Some(command) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

async fn read_until_event(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    event_name: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return None, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json.get("event").and_then(|v| v.as_str()) == Some(event_name) {
                        return Some(json);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

async fn wait_for_chat_with_pubkey(
    data_dir: &Path,
    target_pubkey: &str,
    timeout: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let list = run_ndr(data_dir, &["chat", "list"]).await;
        if let Some(chats) = list
            .get("data")
            .and_then(|d| d.get("chats"))
            .and_then(|c| c.as_array())
        {
            if chats.iter().any(|chat| {
                chat.get("their_pubkey").and_then(|v| v.as_str()) == Some(target_pubkey)
            }) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

async fn stop_child(mut child: Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let _ = Command::new("kill")
                .arg("-INT")
                .arg(pid.to_string())
                .status()
                .await;
        }
    }

    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[tokio::test]
async fn test_listen_group_metadata_and_message() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    let bob_pubkey = bob_login["data"]["pubkey"]
        .as_str()
        .expect("bob pubkey")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;

    let result = async {
        // Start Alice listen so invite response can be processed.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Alice creates invite, Bob joins.
        let invite = run_ndr(alice_dir.path(), &["invite", "create", "-l", "group-test"]).await;
        let invite_url = invite["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        let _bob_join = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;

        // Alice should receive session_created.
        let session_event = read_until_event(
            &mut alice_stdout,
            "session_created",
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive session_created event");
        assert_eq!(
            session_event["their_pubkey"].as_str(),
            Some(bob_pubkey.as_str()),
            "Alice session_created should reference Bob pubkey"
        );

        // Bob sends a kickoff message so Alice can send later.
        let kickoff_text = "kickoff";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, kickoff_text]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(kickoff_text));

        // Start Bob listen.
        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        // Alice creates a group with Bob.
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Test Group",
                "--members",
                &bob_pubkey,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Bob should receive group metadata creation.
        let metadata_event =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata event");
        assert_eq!(metadata_event["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(metadata_event["action"].as_str(), Some("created"));

        // Alice sends a group message.
        let msg_text = "hello group";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg_text]).await;

        // Bob should receive group message.
        let msg_event = read_until_event(&mut bob_stdout, "group_message", Duration::from_secs(10))
            .await
            .expect("Bob should receive group_message event");
        assert_eq!(msg_event["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(msg_event["content"].as_str(), Some(msg_text));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}

#[tokio::test]
async fn test_listen_group_add_member_and_fanout() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);
    let carol_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    let carol_sk = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    let bob_pubkey = bob_login["data"]["pubkey"]
        .as_str()
        .expect("bob pubkey")
        .to_string();

    let carol_login = run_ndr(carol_dir.path(), &["login", carol_sk]).await;
    let carol_pubkey = carol_login["data"]["pubkey"]
        .as_str()
        .expect("carol pubkey")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;
    let mut carol_child: Option<Child> = None;

    let result = async {
        // Alice creates invite for Bob, Bob joins.
        let invite_bob = run_ndr(alice_dir.path(), &["invite", "create", "-l", "group-bob"]).await;
        let invite_bob_id = invite_bob["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_bob_url = invite_bob["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let bob_join = run_ndr(bob_dir.path(), &["chat", "join", &invite_bob_url]).await;
        let bob_response_event = bob_join["data"]["response_event"]
            .as_str()
            .expect("bob response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_bob_id, &bob_response_event],
        )
        .await;

        // Alice creates invite for Carol, Carol joins.
        let invite_carol =
            run_ndr(alice_dir.path(), &["invite", "create", "-l", "group-carol"]).await;
        let invite_carol_id = invite_carol["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_carol_url = invite_carol["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let carol_join = run_ndr(carol_dir.path(), &["chat", "join", &invite_carol_url]).await;
        let carol_response_event = carol_join["data"]["response_event"]
            .as_str()
            .expect("carol response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_carol_id, &carol_response_event],
        )
        .await;

        // Start Alice listen after chats are created.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Bob sends a kickoff message so Alice can send to Bob.
        let bob_kickoff = "kickoff-bob";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, bob_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Bob kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(bob_kickoff));

        // Carol sends a kickoff message so Alice can send to Carol.
        let carol_kickoff = "kickoff-carol";
        let _ = run_ndr(carol_dir.path(), &["send", &alice_pubkey, carol_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Carol kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(carol_kickoff));

        // Start Bob listen.
        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        // Start Carol listen.
        let (child, mut carol_stdout) = start_ndr_listen(carol_dir.path()).await;
        carol_child = Some(child);
        assert!(
            read_until_command(&mut carol_stdout, "listen", Duration::from_secs(5)).await,
            "Carol should print listen message"
        );

        // Alice creates a group with Bob only.
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Big Group",
                "--members",
                &bob_pubkey,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Bob should receive group metadata creation.
        let bob_created =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata created");
        assert_eq!(bob_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_created["action"].as_str(), Some("created"));

        // Alice adds Carol to the group.
        let _ = run_ndr(
            alice_dir.path(),
            &["group", "add-member", &group_id, &carol_pubkey],
        )
        .await;

        // Bob should receive group metadata update.
        let bob_updated =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata updated");
        assert_eq!(bob_updated["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_updated["action"].as_str(), Some("updated"));

        // Carol should receive group metadata creation.
        let carol_created =
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_metadata created");
        assert_eq!(carol_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_created["action"].as_str(), Some("created"));

        // Alice sends a group message to all.
        let msg_text = "hello everyone";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg_text]).await;

        // Bob should receive group message.
        let bob_msg = read_until_event(&mut bob_stdout, "group_message", Duration::from_secs(10))
            .await
            .expect("Bob should receive group_message");
        assert_eq!(bob_msg["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_msg["content"].as_str(), Some(msg_text));

        // Carol should receive group message.
        let carol_msg =
            read_until_event(&mut carol_stdout, "group_message", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_message");
        assert_eq!(carol_msg["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_msg["content"].as_str(), Some(msg_text));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_child {
        stop_child(child).await;
    }
    if let Some(child) = carol_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}

#[tokio::test]
async fn test_listen_group_remove_member() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);
    let carol_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    let carol_sk = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    let bob_pubkey = bob_login["data"]["pubkey"]
        .as_str()
        .expect("bob pubkey")
        .to_string();

    let carol_login = run_ndr(carol_dir.path(), &["login", carol_sk]).await;
    let carol_pubkey = carol_login["data"]["pubkey"]
        .as_str()
        .expect("carol pubkey")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;
    let mut carol_child: Option<Child> = None;

    let result = async {
        // Alice creates invite for Bob, Bob joins, Alice accepts.
        let invite_bob = run_ndr(alice_dir.path(), &["invite", "create", "-l", "remove-bob"]).await;
        let invite_bob_id = invite_bob["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_bob_url = invite_bob["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let bob_join = run_ndr(bob_dir.path(), &["chat", "join", &invite_bob_url]).await;
        let bob_response_event = bob_join["data"]["response_event"]
            .as_str()
            .expect("bob response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_bob_id, &bob_response_event],
        )
        .await;

        // Alice creates invite for Carol, Carol joins, Alice accepts.
        let invite_carol = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "remove-carol"],
        )
        .await;
        let invite_carol_id = invite_carol["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_carol_url = invite_carol["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let carol_join = run_ndr(carol_dir.path(), &["chat", "join", &invite_carol_url]).await;
        let carol_response_event = carol_join["data"]["response_event"]
            .as_str()
            .expect("carol response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_carol_id, &carol_response_event],
        )
        .await;

        // Start Alice listen after chats are created.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Bob sends kickoff so Alice can send to Bob.
        let bob_kickoff = "kickoff-bob-remove";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, bob_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Bob kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(bob_kickoff));

        // Carol sends kickoff so Alice can send to Carol.
        let carol_kickoff = "kickoff-carol-remove";
        let _ = run_ndr(carol_dir.path(), &["send", &alice_pubkey, carol_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Carol kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(carol_kickoff));

        // Start Bob and Carol listeners.
        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        let (child, mut carol_stdout) = start_ndr_listen(carol_dir.path()).await;
        carol_child = Some(child);
        assert!(
            read_until_command(&mut carol_stdout, "listen", Duration::from_secs(5)).await,
            "Carol should print listen message"
        );

        // Alice creates group with Bob + Carol.
        let members_arg = format!("{},{}", bob_pubkey, carol_pubkey);
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Removal Group",
                "--members",
                &members_arg,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Bob and Carol should both receive group metadata creation.
        let bob_created =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata created");
        assert_eq!(bob_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_created["action"].as_str(), Some("created"));

        let carol_created =
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_metadata created");
        assert_eq!(carol_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_created["action"].as_str(), Some("created"));

        // Alice removes Bob from the group.
        let _ = run_ndr(
            alice_dir.path(),
            &["group", "remove-member", &group_id, &bob_pubkey],
        )
        .await;

        // Bob should receive "removed".
        let bob_removed =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata removed");
        assert_eq!(bob_removed["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_removed["action"].as_str(), Some("removed"));

        // Carol should receive "updated".
        let carol_updated =
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_metadata updated");
        assert_eq!(carol_updated["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_updated["action"].as_str(), Some("updated"));

        // Alice sends a group message; Carol should receive it, Bob should not.
        let msg_text = "after-removal";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg_text]).await;

        let carol_msg =
            read_until_event(&mut carol_stdout, "group_message", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_message after removal");
        assert_eq!(carol_msg["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_msg["content"].as_str(), Some(msg_text));

        let bob_msg =
            read_until_event(&mut bob_stdout, "group_message", Duration::from_secs(2)).await;
        assert!(
            bob_msg.is_none(),
            "Bob should not receive group_message after removal"
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_child {
        stop_child(child).await;
    }
    if let Some(child) = carol_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}

#[tokio::test]
async fn test_group_accept_shared_channel_invite_opens_session() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);
    let carol_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    let carol_sk = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    let bob_pubkey = bob_login["data"]["pubkey"]
        .as_str()
        .expect("bob pubkey")
        .to_string();

    let carol_login = run_ndr(carol_dir.path(), &["login", carol_sk]).await;
    let carol_pubkey = carol_login["data"]["pubkey"]
        .as_str()
        .expect("carol pubkey")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;
    let mut carol_child: Option<Child> = None;

    let result = async {
        // Create 1:1 sessions with Alice so she can fan-out group metadata.
        let invite_bob = run_ndr(alice_dir.path(), &["invite", "create", "-l", "shared-bob"]).await;
        let invite_bob_id = invite_bob["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_bob_url = invite_bob["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let bob_join = run_ndr(bob_dir.path(), &["chat", "join", &invite_bob_url]).await;
        let bob_response_event = bob_join["data"]["response_event"]
            .as_str()
            .expect("bob response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_bob_id, &bob_response_event],
        )
        .await;

        let invite_carol = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "shared-carol"],
        )
        .await;
        let invite_carol_id = invite_carol["data"]["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        let invite_carol_url = invite_carol["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let carol_join = run_ndr(carol_dir.path(), &["chat", "join", &invite_carol_url]).await;
        let carol_response_event = carol_join["data"]["response_event"]
            .as_str()
            .expect("carol response event")
            .to_string();
        let _ = run_ndr(
            alice_dir.path(),
            &["invite", "accept", &invite_carol_id, &carol_response_event],
        )
        .await;

        // Start listeners so metadata and shared-channel invites are processed.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        let (child, mut carol_stdout) = start_ndr_listen(carol_dir.path()).await;
        carol_child = Some(child);
        assert!(
            read_until_command(&mut carol_stdout, "listen", Duration::from_secs(5)).await,
            "Carol should print listen message"
        );

        // Kick off ratchets so Alice can send.
        let bob_kickoff = "kickoff-bob-shared";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, bob_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Bob kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(bob_kickoff));

        let carol_kickoff = "kickoff-carol-shared";
        let _ = run_ndr(carol_dir.path(), &["send", &alice_pubkey, carol_kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Carol kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(carol_kickoff));

        // Alice creates group with Bob + Carol.
        let members_arg = format!("{},{}", bob_pubkey, carol_pubkey);
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Shared Channel Group",
                "--members",
                &members_arg,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        let bob_created =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata created");
        assert_eq!(bob_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_created["action"].as_str(), Some("created"));

        let carol_created =
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_metadata created");
        assert_eq!(carol_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_created["action"].as_str(), Some("created"));

        // Bob and Carol accept the group, which publishes invites on the shared channel.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        let _ = run_ndr(carol_dir.path(), &["group", "accept", &group_id]).await;

        // Shared-channel invites should create 1:1 chats between Bob and Carol.
        assert!(
            wait_for_chat_with_pubkey(bob_dir.path(), &carol_pubkey, Duration::from_secs(10)).await,
            "Bob should open a chat with Carol via shared channel"
        );
        assert!(
            wait_for_chat_with_pubkey(carol_dir.path(), &bob_pubkey, Duration::from_secs(10)).await,
            "Carol should open a chat with Bob via shared channel"
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_child {
        stop_child(child).await;
    }
    if let Some(child) = carol_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}
