//! CLI-level interop test
//!
//! Tests the full flow:
//! 1. ndr creates an invite
//! 2. Accept the invite (simulating TypeScript side)
//! 3. Send an encrypted message
//! 4. Receive and decrypt the message

use std::process::Command;
use tempfile::TempDir;

fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new("cargo")
        .env("NDR_IGNORE_PUBLISH_ERRORS", "1")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .expect("Failed to run ndr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        panic!("ndr failed: {} {}", stdout, stderr);
    }

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

#[test]
fn test_cli_encrypt_decrypt_roundtrip() {
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    // Alice's key
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // Bob's key
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    // Alice logs in
    let result = run_ndr(alice_dir.path(), &["login", alice_sk]);
    assert_eq!(result["status"], "ok");

    // Bob logs in
    let result = run_ndr(bob_dir.path(), &["login", bob_sk]);
    assert_eq!(result["status"], "ok");

    // Alice creates an invite
    let result = run_ndr(alice_dir.path(), &["invite", "create", "--label", "Test"]);
    assert_eq!(result["status"], "ok");
    let invite_url = result["data"]["url"].as_str().unwrap();
    let invite_id = result["data"]["id"].as_str().unwrap();

    // Bob joins via the invite URL
    let result = run_ndr(bob_dir.path(), &["chat", "join", invite_url]);
    assert_eq!(result["status"], "ok");
    let bob_chat_id = result["data"]["id"].as_str().unwrap();
    let response_event = result["data"]["response_event"].as_str().unwrap();

    // Alice processes Bob's acceptance
    let result = run_ndr(
        alice_dir.path(),
        &["invite", "accept", invite_id, response_event],
    );
    assert_eq!(result["status"], "ok");
    let alice_chat_id = result["data"]["chat_id"].as_str().unwrap();

    // Bob sends a message to Alice
    let result = run_ndr(bob_dir.path(), &["send", bob_chat_id, "Hello from Bob!"]);
    assert_eq!(result["status"], "ok");
    let encrypted_event = result["data"]["event"].as_str().unwrap();

    // Alice receives and decrypts the message
    let result = run_ndr(alice_dir.path(), &["receive", encrypted_event]);
    assert_eq!(result["status"], "ok");
    assert_eq!(result["data"]["content"], "Hello from Bob!");

    // Verify message was stored
    let result = run_ndr(alice_dir.path(), &["read", alice_chat_id]);
    assert_eq!(result["status"], "ok");
    let messages = result["data"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], "Hello from Bob!");
    assert_eq!(messages[0]["is_outgoing"], false);

    println!("CLI e2e test passed!");
}

#[test]
fn test_bidirectional_conversation() {
    let alice_dir = TempDir::new().unwrap();
    let bob_dir = TempDir::new().unwrap();

    let alice_sk = "1111111111111111111111111111111111111111111111111111111111111111";
    let bob_sk = "2222222222222222222222222222222222222222222222222222222222222222";

    // Both login
    run_ndr(alice_dir.path(), &["login", alice_sk]);
    run_ndr(bob_dir.path(), &["login", bob_sk]);

    // Alice creates invite, Bob joins
    let result = run_ndr(alice_dir.path(), &["invite", "create"]);
    let invite_url = result["data"]["url"].as_str().unwrap();
    let invite_id = result["data"]["id"].as_str().unwrap();

    let result = run_ndr(bob_dir.path(), &["chat", "join", invite_url]);
    let bob_chat_id = result["data"]["id"].as_str().unwrap();
    let response_event = result["data"]["response_event"].as_str().unwrap();

    let result = run_ndr(
        alice_dir.path(),
        &["invite", "accept", invite_id, response_event],
    );
    let alice_chat_id = result["data"]["chat_id"].as_str().unwrap();

    // Bob sends first message
    let result = run_ndr(bob_dir.path(), &["send", bob_chat_id, "Message 1 from Bob"]);
    let event1 = result["data"]["event"].as_str().unwrap();

    // Alice receives it
    let result = run_ndr(alice_dir.path(), &["receive", event1]);
    assert_eq!(result["data"]["content"], "Message 1 from Bob");

    // Alice replies (she's non-initiator, so she can reply after receiving)
    // Note: In double ratchet, Alice needs to receive first before she can send
    let result = run_ndr(
        alice_dir.path(),
        &["send", alice_chat_id, "Reply from Alice"],
    );
    let event2 = result["data"]["event"].as_str().unwrap();

    // Bob receives Alice's reply
    let result = run_ndr(bob_dir.path(), &["receive", event2]);
    assert_eq!(result["data"]["content"], "Reply from Alice");

    // Bob sends another message
    let result = run_ndr(bob_dir.path(), &["send", bob_chat_id, "Message 2 from Bob"]);
    let event3 = result["data"]["event"].as_str().unwrap();

    // Alice receives it
    let result = run_ndr(alice_dir.path(), &["receive", event3]);
    assert_eq!(result["data"]["content"], "Message 2 from Bob");

    println!("Bidirectional conversation test passed!");
}
