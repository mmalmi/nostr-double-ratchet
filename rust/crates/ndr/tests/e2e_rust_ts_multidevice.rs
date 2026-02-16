//! Cross-language E2E test:
//! ndr creates invite -> TS multi-device user accepts -> iris-chat sends ->
//! ndr replies -> both iris-client and iris-chat receive replies.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates
        .parent()
        .unwrap() // rust
        .parent()
        .unwrap() // repo root
        .to_path_buf()
}

async fn run_ndr(data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .await
        .expect("failed to run ndr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        panic!("ndr failed: stdout={} stderr={}", stdout, stderr);
    }

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("failed to parse ndr output: {}\nstdout={}", e, stdout))
}

async fn start_ndr_listen(data_dir: &Path) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("failed to capture stdout"));
    (child, stdout)
}

async fn start_ts_script(
    relay_url: &str,
    invite_url: &str,
) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let ts_dir = repo_root().join("ts");
    let mut child = Command::new("npx")
        .arg("tsx")
        .arg("e2e/rust-ts-multidevice-e2e.ts")
        .arg(relay_url)
        .arg(invite_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("failed to capture stdout"));
    (child, stdout)
}

async fn read_until_marker(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    prefix: &str,
    timeout: Duration,
) -> Option<String> {
    tokio::time::timeout(timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return None,
                Ok(_) => {
                    let trimmed = line.trim();
                    println!("[TS] {}", trimmed);
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

async fn read_until_all_markers(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    markers: &[&str],
    timeout: Duration,
) -> bool {
    let started = Instant::now();
    let mut seen = vec![false; markers.len()];

    while started.elapsed() < timeout {
        let mut line = String::new();
        let read_result =
            tokio::time::timeout(Duration::from_millis(150), reader.read_line(&mut line)).await;

        if let Ok(Ok(n)) = read_result {
            if n == 0 {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            println!("[TS] {}", trimmed);

            for (idx, marker) in markers.iter().enumerate() {
                if trimmed.starts_with(marker) {
                    seen[idx] = true;
                }
            }

            if seen.iter().all(|found| *found) {
                return true;
            }
        }
    }

    false
}

async fn wait_for_ndr_message(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    expected_content: &str,
    timeout: Duration,
) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let mut line = String::new();
        let read_result =
            tokio::time::timeout(Duration::from_millis(150), reader.read_line(&mut line)).await;

        if let Ok(Ok(n)) = read_result {
            if n == 0 {
                continue;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            println!("[ndr listen] {}", trimmed);

            let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            if v["event"] == "message" && v["content"] == expected_content {
                return true;
            }
        }
    }

    false
}

async fn wait_for_ndr_session_created(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    timeout: Duration,
) -> Option<String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let mut line = String::new();
        let read_result =
            tokio::time::timeout(Duration::from_millis(150), reader.read_line(&mut line)).await;

        if let Ok(Ok(n)) = read_result {
            if n == 0 {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            println!("[ndr listen] {}", trimmed);

            let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            if v["event"] == "session_created" {
                if let Some(chat_id) = v["chat_id"].as_str() {
                    return Some(chat_id.to_string());
                }
            }
        }
    }

    None
}

#[tokio::test]
async fn test_ndr_invite_ts_multidevice_back_and_forth() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("failed to start relay");
    let relay_url = format!("ws://{}", addr);

    let ndr_dir = TempDir::new().unwrap();
    let ndr_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        ndr_dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    let login = run_ndr(ndr_dir.path(), &["login", ndr_sk]).await;
    assert_eq!(login["status"], "ok", "ndr login failed");

    let created = run_ndr(ndr_dir.path(), &["invite", "create", "-l", "multidevice"]).await;
    assert_eq!(created["status"], "ok", "ndr invite create failed");
    let invite_url = created["data"]["url"]
        .as_str()
        .expect("missing invite url")
        .to_string();

    let (mut ndr_listen_child, mut ndr_reader) = start_ndr_listen(ndr_dir.path()).await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    let (mut ts_child, mut ts_reader) = start_ts_script(&relay_url, &invite_url).await;

    read_until_marker(
        &mut ts_reader,
        "E2E_RELAY_CONNECTED:",
        Duration::from_secs(15),
    )
    .await
    .expect("TS did not connect");

    read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE2_INVITE_PUBLISHED:",
        Duration::from_secs(20),
    )
    .await
    .expect("TS did not publish iris-chat invite");

    read_until_marker(
        &mut ts_reader,
        "E2E_INVITE_RESPONSE_PUBLISHED",
        Duration::from_secs(20),
    )
    .await
    .expect("TS did not publish invite response");

    let chat_id = wait_for_ndr_session_created(&mut ndr_reader, Duration::from_secs(30))
        .await
        .expect("ndr did not create chat session");

    read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE2_SESSION_CREATED:",
        Duration::from_secs(45),
    )
    .await
    .expect("TS linked iris-chat did not create session");

    read_until_marker(
        &mut ts_reader,
        "E2E_OWNER_BOOTSTRAP_SENT:IRIS_CLIENT_BOOTSTRAP",
        Duration::from_secs(20),
    )
    .await
    .expect("iris-client did not send bootstrap");

    assert!(
        wait_for_ndr_message(
            &mut ndr_reader,
            "IRIS_CLIENT_BOOTSTRAP",
            Duration::from_secs(30),
        )
        .await,
        "ndr did not receive iris-client bootstrap message"
    );

    let _ = ndr_listen_child.kill().await;

    let kickoff = run_ndr(ndr_dir.path(), &["send", &chat_id, "NDR_KICKOFF"]).await;
    assert_eq!(
        kickoff["status"], "ok",
        "ndr failed to send kickoff message"
    );

    read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE2_RECEIVED:NDR_KICKOFF",
        Duration::from_secs(30),
    )
    .await
    .expect("iris-chat did not receive kickoff");

    read_until_marker(
        &mut ts_reader,
        "E2E_OWNER_ACK_SENT:IRIS_CLIENT_ACK_KICKOFF",
        Duration::from_secs(20),
    )
    .await
    .expect("iris-client did not send kickoff ack");

    read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE2_SENT:IRIS_CHAT_TO_NDR_1",
        Duration::from_secs(20),
    )
    .await
    .expect("iris-chat did not send initial message");

    let (mut ndr_listen_child, mut ndr_reader) = start_ndr_listen(ndr_dir.path()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        wait_for_ndr_message(
            &mut ndr_reader,
            "IRIS_CLIENT_ACK_KICKOFF",
            Duration::from_secs(30),
        )
        .await,
        "ndr did not receive iris-client kickoff ack"
    );

    assert!(
        wait_for_ndr_message(
            &mut ndr_reader,
            "IRIS_CHAT_TO_NDR_1",
            Duration::from_secs(30),
        )
        .await,
        "ndr did not receive iris-chat's message"
    );

    let _ = ndr_listen_child.kill().await;

    let send_reply_1 = run_ndr(ndr_dir.path(), &["send", &chat_id, "NDR_TO_IRIS_1"]).await;
    assert_eq!(
        send_reply_1["status"], "ok",
        "ndr failed to send first reply"
    );

    assert!(
        read_until_all_markers(
            &mut ts_reader,
            &[
                "E2E_OWNER_RECEIVED:NDR_TO_IRIS_1",
                "E2E_DEVICE2_RECEIVED:NDR_TO_IRIS_1",
                "E2E_OWNER_SENT:IRIS_CLIENT_TO_NDR_2",
            ],
            Duration::from_secs(30),
        )
        .await,
        "reply fanout/follow-up markers missing after first ndr reply"
    );

    let (mut ndr_listen_child, mut ndr_reader) = start_ndr_listen(ndr_dir.path()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        wait_for_ndr_message(
            &mut ndr_reader,
            "IRIS_CLIENT_TO_NDR_2",
            Duration::from_secs(30),
        )
        .await,
        "ndr did not receive iris-client follow-up"
    );

    let _ = ndr_listen_child.kill().await;

    let send_reply_2 = run_ndr(ndr_dir.path(), &["send", &chat_id, "NDR_TO_IRIS_2"]).await;
    assert_eq!(
        send_reply_2["status"], "ok",
        "ndr failed to send second reply"
    );

    read_until_marker(&mut ts_reader, "E2E_SUCCESS", Duration::from_secs(45))
        .await
        .expect("TS did not report multidevice success");

    let _ = ts_child.kill().await;
    relay.stop().await;
}
