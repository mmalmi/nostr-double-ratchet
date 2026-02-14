//! E2E test: ndr listen handles group metadata and group messages over WebSocket relay

mod common;

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

struct Listener {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

static NDR_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();
static E2E_TEST_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();

fn e2e_test_lock() -> std::sync::MutexGuard<'static, ()> {
    match E2E_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
    {
        Ok(guard) => guard,
        // Keep running remaining tests even if a previous test panicked while
        // holding the lock.
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn expected_ndr_binary_path() -> PathBuf {
    let path = workspace_root().join("target/debug/ndr");
    #[cfg(windows)]
    {
        path.set_extension("exe");
    }
    path
}

fn resolve_ndr_binary_path() -> PathBuf {
    if let Ok(bin_path) = std::env::var("CARGO_BIN_EXE_ndr") {
        return PathBuf::from(bin_path);
    }

    let path = expected_ndr_binary_path();
    if path.exists() {
        return path;
    }

    let status = std::process::Command::new("cargo")
        .arg("build")
        .arg("-q")
        .arg("-p")
        .arg("ndr")
        .current_dir(workspace_root())
        .status()
        .expect("failed to run cargo build -p ndr");
    assert!(
        status.success(),
        "failed to build ndr binary for integration tests"
    );
    path
}

fn ndr_command() -> Command {
    let bin_path = NDR_BIN_PATH
        .get_or_init(resolve_ndr_binary_path)
        .to_path_buf();
    let mut cmd = Command::new(bin_path);
    cmd.env("NOSTR_PREFER_LOCAL", "0");
    cmd
}

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let command = format!(
        "ndr --json --data-dir {} {}",
        data_dir.display(),
        args.join(" ")
    );
    let mut cmd = ndr_command();
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        cmd.arg("--json")
            .arg("--data-dir")
            .arg(data_dir)
            .args(args)
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("Timed out running command: {}", command))
    .unwrap_or_else(|e| panic!("Failed to run ndr command '{}': {}", command, e));

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
    let mut cmd = ndr_command();
    let mut child = cmd
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
    let debug_wait = std::env::var_os("NDR_TEST_DEBUG_WAIT").is_some();
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
                if debug_wait {
                    println!("[wait:{}] {}", event_name, trimmed);
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

async fn read_until_event_with_content(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    event_name: &str,
    content: Option<&str>,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let debug_wait = std::env::var_os("NDR_TEST_DEBUG_WAIT").is_some();
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
                if debug_wait {
                    println!("[wait:{}] {}", event_name, trimmed);
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json.get("event").and_then(|v| v.as_str()) != Some(event_name) {
                        continue;
                    }
                    if let Some(expected) = content {
                        if json.get("content").and_then(|v| v.as_str()) != Some(expected) {
                            continue;
                        }
                    }
                    return Some(json);
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

fn group_secret_bytes(data_dir: &Path, group_id: &str) -> [u8; 32] {
    let storage = ndr::storage::Storage::open(data_dir).expect("open storage");
    let group = storage
        .get_group(group_id)
        .expect("read group")
        .expect("group exists");
    let secret_hex = group.data.secret.expect("group secret present");
    let secret_vec = hex::decode(secret_hex).expect("group secret hex");
    assert_eq!(secret_vec.len(), 32, "group secret should be 32 bytes");
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&secret_vec);
    secret
}

async fn publish_shared_channel_inner(relay_url: &str, secret: [u8; 32], inner_json: &str) {
    let channel = nostr_double_ratchet::SharedChannel::new(&secret).expect("shared channel");
    let event = channel
        .create_event(inner_json)
        .expect("create shared channel event");
    let client = nostr_sdk::Client::default();
    client.add_relay(relay_url).await.expect("add relay");
    client.connect().await;
    let _ = client.send_event(event).await;
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
    let _guard = e2e_test_lock();
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

        // Bob must accept the group to enable SharedChannel subscriptions.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

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
async fn test_group_metadata_reaches_linked_second_device_without_owner_mirror() {
    let _guard = e2e_test_lock();
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_primary_dir = setup_ndr_dir(&relay_url);
    let bob_secondary_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_primary_login = run_ndr(bob_primary_dir.path(), &["login", bob_sk]).await;
    let bob_owner_pubkey = bob_primary_login["data"]["pubkey"]
        .as_str()
        .expect("bob owner pubkey")
        .to_string();

    // Secondary device creates a private link invite.
    let link = run_ndr(bob_secondary_dir.path(), &["link", "create"]).await;
    let link_url = link["data"]["url"]
        .as_str()
        .expect("link invite url")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_primary_child: Option<Child> = None;
    let mut bob_secondary_child: Option<Child> = None;

    let result = async {
        // Start secondary listen before accepting link so it can process link_accepted.
        let (child, mut bob_secondary_stdout) = start_ndr_listen(bob_secondary_dir.path()).await;
        bob_secondary_child = Some(child);
        assert!(
            read_until_command(&mut bob_secondary_stdout, "listen", Duration::from_secs(5)).await,
            "Bob secondary should print listen message"
        );

        // Main Bob device accepts secondary's link invite.
        let _ = run_ndr(bob_primary_dir.path(), &["link", "accept", &link_url]).await;

        let linked = read_until_event(
            &mut bob_secondary_stdout,
            "link_accepted",
            Duration::from_secs(15),
        )
        .await
        .expect("Bob secondary should receive link_accepted");
        assert_eq!(
            linked["owner_pubkey"].as_str(),
            Some(bob_owner_pubkey.as_str())
        );

        // Start Alice listen to process invite responses.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Establish Alice <-> Bob owner chat.
        let invite = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "linked-group-test"],
        )
        .await;
        let invite_url = invite["data"]["url"]
            .as_str()
            .expect("invite url")
            .to_string();
        let _ = run_ndr(bob_primary_dir.path(), &["chat", "join", &invite_url]).await;

        let session_event = read_until_event(
            &mut alice_stdout,
            "session_created",
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive session_created");
        assert_eq!(
            session_event["their_pubkey"].as_str(),
            Some(bob_owner_pubkey.as_str())
        );

        // Kickoff so Alice can send.
        let kickoff = "kickoff-bob-linked";
        let _ = run_ndr(bob_primary_dir.path(), &["send", &alice_pubkey, kickoff]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive kickoff");
        assert_eq!(kickoff_event["content"].as_str(), Some(kickoff));

        // Start Bob primary listen.
        let (child, mut bob_primary_stdout) = start_ndr_listen(bob_primary_dir.path()).await;
        bob_primary_child = Some(child);
        assert!(
            read_until_command(&mut bob_primary_stdout, "listen", Duration::from_secs(5)).await,
            "Bob primary should print listen message"
        );

        // Wait until Bob secondary discovers/creates owner chat with Alice.
        assert!(
            wait_for_chat_with_pubkey(
                bob_secondary_dir.path(),
                &alice_pubkey,
                Duration::from_secs(20)
            )
            .await,
            "Bob secondary should discover chat with Alice owner"
        );

        // Alice creates group with Bob owner.
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Linked Device Group",
                "--members",
                &bob_owner_pubkey,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Both Bob devices should receive the created group metadata.
        let bob_primary_created = read_until_event(
            &mut bob_primary_stdout,
            "group_metadata",
            Duration::from_secs(15),
        )
        .await
        .expect("Bob primary should receive group_metadata created");
        assert_eq!(
            bob_primary_created["group_id"].as_str(),
            Some(group_id.as_str())
        );
        assert_eq!(bob_primary_created["action"].as_str(), Some("created"));

        let bob_secondary_created = read_until_event(
            &mut bob_secondary_stdout,
            "group_metadata",
            Duration::from_secs(15),
        )
        .await
        .expect("Bob secondary should receive group_metadata created");
        assert_eq!(
            bob_secondary_created["group_id"].as_str(),
            Some(group_id.as_str())
        );
        assert_eq!(bob_secondary_created["action"].as_str(), Some("created"));

        // Both devices accept to enable group subscriptions.
        let _ = run_ndr(bob_primary_dir.path(), &["group", "accept", &group_id]).await;
        let _ = run_ndr(bob_secondary_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(700)).await;

        // Alice sends one group message; both Bob devices should receive it.
        let msg = "hello linked devices";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg]).await;

        let bob_primary_msg = read_until_event_with_content(
            &mut bob_primary_stdout,
            "group_message",
            Some(msg),
            Duration::from_secs(20),
        )
        .await
        .expect("Bob primary should receive group_message");
        assert_eq!(
            bob_primary_msg["group_id"].as_str(),
            Some(group_id.as_str())
        );

        // Safety invariant: do not owner-mirror peer group messages to linked devices.
        let bob_secondary_msg = read_until_event_with_content(
            &mut bob_secondary_stdout,
            "group_message",
            Some(msg),
            Duration::from_secs(3),
        )
        .await;
        assert!(
            bob_secondary_msg.is_none(),
            "Bob secondary should not receive owner-mirrored peer group_message"
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_primary_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_secondary_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}

#[tokio::test]
async fn test_group_sender_key_rotation() {
    let _guard = e2e_test_lock();
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
        let invite = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "rotation-test"],
        )
        .await;
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
            Some(bob_pubkey.as_str())
        );

        // Bob sends a kickoff message so Alice can send later.
        let kickoff_text = "kickoff";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, kickoff_text]).await;
        let _kickoff_event =
            read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
                .await
                .expect("Alice should receive kickoff message");

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
                "Rotation Group",
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
        let created = read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
            .await
            .expect("Bob should receive group_metadata created");
        assert_eq!(created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(created["action"].as_str(), Some("created"));

        // Bob must accept the group to enable SharedChannel subscriptions.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Alice sends a group message (establishes sender key).
        let msg1 = "first message";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg1]).await;
        let bob_msg1 = read_until_event_with_content(
            &mut bob_stdout,
            "group_message",
            Some(msg1),
            Duration::from_secs(10),
        )
        .await
        .expect("Bob should receive first group_message");
        assert_eq!(bob_msg1["group_id"].as_str(), Some(group_id.as_str()));

        // Alice rotates sender key.
        let rotate = run_ndr(alice_dir.path(), &["group", "rotate-sender-key", &group_id]).await;
        let rotated_key_id = rotate["data"]["key_id"].as_u64().expect("rotate key_id") as u32;

        // Bob should observe the new distribution (ignore any earlier group_sender_key events).
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_rotation = false;
        while Instant::now() < deadline {
            let ev =
                read_until_event(&mut bob_stdout, "group_sender_key", Duration::from_secs(2)).await;
            if let Some(ev) = ev {
                if ev["group_id"].as_str() == Some(group_id.as_str())
                    && ev["sender_pubkey"].as_str() == Some(alice_pubkey.as_str())
                    && ev["key_id"].as_u64() == Some(rotated_key_id as u64)
                {
                    saw_rotation = true;
                    break;
                }
            }
        }
        assert!(saw_rotation, "Bob should receive rotated group_sender_key");

        // Message after rotation should decrypt.
        let msg2 = "after rotation";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, msg2]).await;
        let bob_msg2 = read_until_event_with_content(
            &mut bob_stdout,
            "group_message",
            Some(msg2),
            Duration::from_secs(10),
        )
        .await
        .expect("Bob should receive group_message after rotation");
        assert_eq!(bob_msg2["group_id"].as_str(), Some(group_id.as_str()));

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
async fn test_group_chat_six_participants_everyone_receives() {
    let _guard = e2e_test_lock();
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    struct Participant {
        name: &'static str,
        dir: TempDir,
        pubkey: String,
    }

    let participants_spec = [
        (
            "alice",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        (
            "bob",
            "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
        ),
        (
            "carol",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "dave",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
        (
            "erin",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ),
        (
            "frank",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        ),
    ];

    let mut participants: Vec<Participant> = Vec::new();
    for (name, secret) in participants_spec {
        let dir = setup_ndr_dir(&relay_url);
        let login = run_ndr(dir.path(), &["login", secret]).await;
        let pubkey = login["data"]["pubkey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        participants.push(Participant { name, dir, pubkey });
    }

    let mut listeners: Vec<Listener> = Vec::new();
    let result = async {
        // Create full mesh of 1:1 sessions.
        for i in 0..participants.len() {
            for j in (i + 1)..participants.len() {
                let inviter = &participants[i];
                let invitee = &participants[j];
                let label = format!("mesh-{}-{}", inviter.name, invitee.name);
                let invite = run_ndr(inviter.dir.path(), &["invite", "create", "-l", &label]).await;
                let invite_id = invite["data"]["id"]
                    .as_str()
                    .expect("invite id")
                    .to_string();
                let invite_url = invite["data"]["url"]
                    .as_str()
                    .expect("invite url")
                    .to_string();
                tokio::time::sleep(Duration::from_millis(500)).await;
                let join = run_ndr(invitee.dir.path(), &["chat", "join", &invite_url]).await;
                let response_event = join["data"]["response_event"]
                    .as_str()
                    .expect("response event")
                    .to_string();
                let _ = run_ndr(
                    inviter.dir.path(),
                    &["invite", "accept", &invite_id, &response_event],
                )
                .await;
            }
        }

        // Start listeners for all participants.
        for p in &participants {
            let (child, mut stdout) = start_ndr_listen(p.dir.path()).await;
            assert!(
                read_until_command(&mut stdout, "listen", Duration::from_secs(5)).await,
                "{} should print listen message",
                p.name
            );
            listeners.push(Listener { child, stdout });
        }

        // Kickoff so inviters can send to invitees.
        for i in 0..participants.len() {
            for j in (i + 1)..participants.len() {
                let inviter = &participants[i];
                let invitee = &participants[j];
                let kickoff = format!("kickoff-{}-{}", invitee.name, inviter.name);
                let _ = run_ndr(invitee.dir.path(), &["send", &inviter.pubkey, &kickoff]).await;
                let listener = listeners.get_mut(i).expect("inviter listener should exist");
                let kickoff_event = read_until_event_with_content(
                    &mut listener.stdout,
                    "message",
                    Some(&kickoff),
                    Duration::from_secs(10),
                )
                .await
                .expect("inviter should receive kickoff message");
                assert_eq!(kickoff_event["content"].as_str(), Some(kickoff.as_str()));
            }
        }

        // Alice creates a group with everyone else.
        let alice = &participants[0];
        let members_csv = participants[1..]
            .iter()
            .map(|p| p.pubkey.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let group_create = run_ndr(
            alice.dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Six Pack Group",
                "--members",
                &members_csv,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Everyone else should receive the group metadata.
        for listener in listeners.iter_mut().skip(1) {
            let event = read_until_event(
                &mut listener.stdout,
                "group_metadata",
                Duration::from_secs(10),
            )
            .await
            .expect("member should receive group_metadata");
            assert_eq!(event["group_id"].as_str(), Some(group_id.as_str()));
            assert_eq!(event["action"].as_str(), Some("created"));
        }

        // Everyone must accept the group to subscribe to the SharedChannel.
        for p in participants.iter().skip(1) {
            let _ = run_ndr(p.dir.path(), &["group", "accept", &group_id]).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Each participant sends a group message; everyone else should receive it.
        for sender_idx in 0..participants.len() {
            let sender = &participants[sender_idx];
            let msg = format!("group-msg-{}", sender.name);
            let _ = run_ndr(sender.dir.path(), &["group", "send", &group_id, &msg]).await;

            for recipient_idx in 0..participants.len() {
                if recipient_idx == sender_idx {
                    continue;
                }
                let listener = listeners
                    .get_mut(recipient_idx)
                    .expect("recipient listener should exist");
                let event = read_until_event_with_content(
                    &mut listener.stdout,
                    "group_message",
                    Some(&msg),
                    Duration::from_secs(10),
                )
                .await
                .expect("recipient should receive group_message");
                assert_eq!(event["group_id"].as_str(), Some(group_id.as_str()));
                assert_eq!(event["content"].as_str(), Some(msg.as_str()));
            }
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    for listener in listeners {
        stop_child(listener.child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}

#[tokio::test]
async fn test_listen_group_add_member_and_fanout() {
    let _guard = e2e_test_lock();
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

        // Bob accepts so he subscribes to SharedChannel before Carol is added / messages are sent.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Alice sends a group message before Carol is added.
        // This ensures sender keys are already established on the *old* shared channel secret.
        let pre_text = "hello bob (pre-add)";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, pre_text]).await;

        // Bob should receive the pre-add group message.
        let bob_msg_pre = read_until_event_with_content(
            &mut bob_stdout,
            "group_message",
            Some(pre_text),
            Duration::from_secs(10),
        )
        .await
        .expect("Bob should receive pre-add group_message");
        assert_eq!(bob_msg_pre["group_id"].as_str(), Some(group_id.as_str()));

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

        // Carol accepts so she subscribes to SharedChannel before messages are sent.
        let _ = run_ndr(carol_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

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
            read_until_event(&mut carol_stdout, "group_message", Duration::from_secs(20))
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
async fn test_group_chat_two_strangers_can_exchange_messages() {
    let _guard = e2e_test_lock();
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);
    let carol_dir = setup_ndr_dir(&relay_url);

    // Use deterministic keys so the test is stable and debugging is easier.
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
        // Alice creates invite for Bob, Bob joins, Alice accepts response.
        let invite_bob = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "strangers-bob"],
        )
        .await;
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

        // Alice creates invite for Carol, Carol joins, Alice accepts response.
        let invite_carol = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "strangers-carol"],
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

        // Start Alice listen so kickoff messages can be processed (required before Alice can send).
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Bob sends a kickoff message so Alice can send to Bob.
        let bob_kickoff = "kickoff-bob";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, bob_kickoff]).await;
        let kickoff_event = read_until_event_with_content(
            &mut alice_stdout,
            "message",
            Some(bob_kickoff),
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive Bob kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(bob_kickoff));

        // Carol sends a kickoff message so Alice can send to Carol.
        let carol_kickoff = "kickoff-carol";
        let _ = run_ndr(carol_dir.path(), &["send", &alice_pubkey, carol_kickoff]).await;
        let kickoff_event = read_until_event_with_content(
            &mut alice_stdout,
            "message",
            Some(carol_kickoff),
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive Carol kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(carol_kickoff));

        // Ensure Bob and Carol do not have 1:1 chats with each other.
        let bob_chats = run_ndr(bob_dir.path(), &["chat", "list"]).await;
        let bob_has_carol = bob_chats["data"]["chats"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|c| c.get("their_pubkey").and_then(|v| v.as_str()) == Some(carol_pubkey.as_str()));
        assert!(
            !bob_has_carol,
            "Bob should not have a direct chat with Carol"
        );

        let carol_chats = run_ndr(carol_dir.path(), &["chat", "list"]).await;
        let carol_has_bob = carol_chats["data"]["chats"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|c| c.get("their_pubkey").and_then(|v| v.as_str()) == Some(bob_pubkey.as_str()));
        assert!(
            !carol_has_bob,
            "Carol should not have a direct chat with Bob"
        );

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

        // Alice creates a group including Bob + Carol.
        let members_csv = format!("{},{}", bob_pubkey, carol_pubkey);
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Strangers Group",
                "--members",
                &members_csv,
            ],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Bob should receive group metadata creation (and see Carol in the member list).
        let bob_created =
            read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Bob should receive group_metadata created");
        assert_eq!(bob_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(bob_created["action"].as_str(), Some("created"));
        let bob_members = bob_created["members"]
            .as_array()
            .expect("group_metadata members should be an array");
        assert!(
            bob_members
                .iter()
                .any(|m| m.as_str() == Some(alice_pubkey.as_str())),
            "Bob should see Alice as a member"
        );
        assert!(
            bob_members
                .iter()
                .any(|m| m.as_str() == Some(bob_pubkey.as_str())),
            "Bob should see himself as a member"
        );
        assert!(
            bob_members
                .iter()
                .any(|m| m.as_str() == Some(carol_pubkey.as_str())),
            "Bob should see Carol as a member"
        );

        // Carol should receive group metadata creation (and see Bob in the member list).
        let carol_created =
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(10))
                .await
                .expect("Carol should receive group_metadata created");
        assert_eq!(carol_created["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(carol_created["action"].as_str(), Some("created"));
        let carol_members = carol_created["members"]
            .as_array()
            .expect("group_metadata members should be an array");
        assert!(
            carol_members
                .iter()
                .any(|m| m.as_str() == Some(alice_pubkey.as_str())),
            "Carol should see Alice as a member"
        );
        assert!(
            carol_members
                .iter()
                .any(|m| m.as_str() == Some(bob_pubkey.as_str())),
            "Carol should see Bob as a member"
        );
        assert!(
            carol_members
                .iter()
                .any(|m| m.as_str() == Some(carol_pubkey.as_str())),
            "Carol should see herself as a member"
        );

        // Members accept the group so shared-channel subscriptions are enabled.
        // (The creator's group is already accepted.)
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        let _ = run_ndr(carol_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Admin sends a group message; both members should receive it.
        let admin_msg = "hello group (from admin)";
        let _ = run_ndr(alice_dir.path(), &["group", "send", &group_id, admin_msg]).await;
        let _ = read_until_event_with_content(
            &mut bob_stdout,
            "group_message",
            Some(admin_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Bob should receive admin group_message");
        let _ = read_until_event_with_content(
            &mut carol_stdout,
            "group_message",
            Some(admin_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Carol should receive admin group_message");

        // Bob sends a group message; Carol should receive it (they have no prior 1:1 chat).
        let bob_msg = "hello from bob (group)";
        let _ = run_ndr(bob_dir.path(), &["group", "send", &group_id, bob_msg]).await;
        let _ = read_until_event_with_content(
            &mut alice_stdout,
            "group_message",
            Some(bob_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive Bob group_message");
        let _ = read_until_event_with_content(
            &mut carol_stdout,
            "group_message",
            Some(bob_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Carol should receive Bob group_message");

        // Carol sends a group message; Bob should receive it.
        let carol_msg = "hello from carol (group)";
        let _ = run_ndr(carol_dir.path(), &["group", "send", &group_id, carol_msg]).await;
        let _ = read_until_event_with_content(
            &mut alice_stdout,
            "group_message",
            Some(carol_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Alice should receive Carol group_message");
        let _ = read_until_event_with_content(
            &mut bob_stdout,
            "group_message",
            Some(carol_msg),
            Duration::from_secs(10),
        )
        .await
        .expect("Bob should receive Carol group_message");

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
    let _guard = e2e_test_lock();
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

        // Bob and Carol accept so they subscribe to SharedChannel.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        let _ = run_ndr(carol_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

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
            read_until_event(&mut carol_stdout, "group_metadata", Duration::from_secs(20))
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
    let _guard = e2e_test_lock();
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

#[tokio::test]
async fn test_group_accept_shared_channel_rejects_unsigned_and_impersonated_invites() {
    let _guard = e2e_test_lock();
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);
    let carol_dir = setup_ndr_dir(&relay_url);
    let attacker_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    let carol_sk = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let attacker_sk = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

    let _attacker_login = run_ndr(attacker_dir.path(), &["login", attacker_sk]).await;

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;

    let result = async {
        // Establish Alice <-> Bob session.
        let invite_bob = run_ndr(
            alice_dir.path(),
            &["invite", "create", "-l", "impersonation-bob"],
        )
        .await;
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

        // Start Alice listen so kickoff can be processed.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Kick off ratchet so Alice can send metadata.
        let kickoff_text = "kickoff-bob-impersonation";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, kickoff_text]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive Bob kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(kickoff_text));

        // Bob listens for metadata + shared-channel announcements.
        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        // Alice creates group with Bob + Carol.
        let members_arg = format!("{},{}", bob_pubkey, carol_pubkey);
        let group_create = run_ndr(
            alice_dir.path(),
            &[
                "group",
                "create",
                "--name",
                "Impersonation Test Group",
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

        // Bob accepts so shared-channel subscription is active.
        let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;
        tokio::time::sleep(Duration::from_millis(600)).await;

        assert!(
            !wait_for_chat_with_pubkey(bob_dir.path(), &carol_pubkey, Duration::from_secs(1)).await,
            "Bob should not have a Carol chat before attack events"
        );

        let attacker_invite = run_ndr(
            attacker_dir.path(),
            &["invite", "create", "-l", "attacker-shared"],
        )
        .await;
        let attacker_invite_url = attacker_invite["data"]["url"]
            .as_str()
            .expect("attacker invite url")
            .to_string();

        let secret = group_secret_bytes(bob_dir.path(), &group_id);

        // Attack 1: unsigned inner rumor payload claiming Carol.
        let unsigned_inner = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "pubkey": carol_pubkey.clone(),
            "created_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            "kind": nostr_double_ratchet::GROUP_INVITE_RUMOR_KIND,
            "tags": [["l", group_id.clone()]],
            "content": serde_json::json!({
                "inviteUrl": attacker_invite_url.clone(),
                "groupId": group_id.clone(),
                "ownerPubkey": carol_pubkey.clone(),
            }).to_string()
        })
        .to_string();
        publish_shared_channel_inner(&relay_url, secret, &unsigned_inner).await;

        assert!(
            !wait_for_chat_with_pubkey(bob_dir.path(), &carol_pubkey, Duration::from_secs(3)).await,
            "Bob must reject unsigned shared-channel inner payloads"
        );

        // Attack 2: signed inner event by attacker, claiming Carol as owner.
        let attacker_sk_bytes = hex::decode(attacker_sk).expect("attacker secret hex");
        let attacker_sk_array: [u8; 32] = attacker_sk_bytes
            .try_into()
            .expect("attacker secret must be 32 bytes");
        let attacker_secret_key =
            nostr::SecretKey::from_slice(&attacker_sk_array).expect("attacker secret key");
        let attacker_keys = nostr::Keys::new(attacker_secret_key);

        let signed_content = serde_json::json!({
            "inviteUrl": attacker_invite_url,
            "groupId": group_id,
            "ownerPubkey": carol_pubkey,
        })
        .to_string();
        let inner_unsigned = nostr::EventBuilder::new(
            nostr::Kind::Custom(nostr_double_ratchet::GROUP_INVITE_RUMOR_KIND as u16),
            signed_content,
        )
        .tag(nostr::Tag::parse(&["l".to_string(), group_id.clone()]).expect("group tag"))
        .build(attacker_keys.public_key());
        let inner_signed = inner_unsigned
            .sign_with_keys(&attacker_keys)
            .expect("signed inner event");
        publish_shared_channel_inner(&relay_url, secret, &nostr::JsonUtil::as_json(&inner_signed))
            .await;

        assert!(
            !wait_for_chat_with_pubkey(bob_dir.path(), &carol_pubkey, Duration::from_secs(3)).await,
            "Bob must reject signed shared-channel invites with unverified owner claim"
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
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}
