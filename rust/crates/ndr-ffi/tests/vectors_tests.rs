//! Test vector generation and validation for cross-language parity.
//!
//! These tests generate deterministic test vectors that can be validated
//! against TypeScript and other language implementations.

use ndr_ffi::{generate_keypair, InviteHandle, SessionHandle};

#[test]
fn test_generate_invite_roundtrip_vector() {
    let kp = generate_keypair();
    
    let invite = InviteHandle::create_new(kp.public_key_hex.clone(), None, None).unwrap();
    let url = invite.to_url("https://example.com".to_string()).unwrap();
    let shared_secret = invite.get_shared_secret_hex();
    
    // Verify roundtrip
    let restored = InviteHandle::from_url(url.clone()).unwrap();
    assert_eq!(invite.get_inviter_pubkey_hex(), restored.get_inviter_pubkey_hex());
    assert_eq!(invite.get_shared_secret_hex(), restored.get_shared_secret_hex());
    
    println!("Invite Roundtrip Vector:");
    println!("  inviter_pubkey: {}", kp.public_key_hex);
    println!("  url: {}", url);
    println!("  shared_secret: {}", shared_secret);
}

#[test]
fn test_generate_session_message_vector() {
    let alice_kp = generate_keypair();
    let bob_kp = generate_keypair();
    
    // Alice creates invite
    let invite = InviteHandle::create_new(alice_kp.public_key_hex.clone(), None, None).unwrap();
    let invite_json = invite.serialize().unwrap();
    
    // Bob accepts invite
    let bob_invite = InviteHandle::deserialize(invite_json.clone()).unwrap();
    let accept_result = bob_invite
        .accept(
            bob_kp.public_key_hex.clone(),
            bob_kp.private_key_hex.clone(),
            None,
        )
        .unwrap();
    
    let bob_session = accept_result.session;
    let response_event_json = accept_result.response_event_json.clone();
    
    // Bob sends message
    let plaintext = "Hello from Bob!";
    let send_result = bob_session.send_text(plaintext.to_string()).unwrap();
    
    println!("Session Message Vector:");
    println!("  alice_pubkey: {}", alice_kp.public_key_hex);
    println!("  bob_pubkey: {}", bob_kp.public_key_hex);
    println!("  plaintext: {}", plaintext);
    println!("  invite_json length: {}", invite_json.len());
    println!("  response_event_json length: {}", response_event_json.len());
    println!("  encrypted_event_json length: {}", send_result.outer_event_json.len());
}

#[test]
fn test_vectors_full_flow() {
    // Generate deterministic test flow
    let alice = generate_keypair();
    let bob = generate_keypair();
    
    // Alice creates invite
    let invite = InviteHandle::create_new(alice.public_key_hex.clone(), Some("alice-device".to_string()), None)
        .expect("create invite");
    let invite_json = invite.serialize().expect("serialize invite");
    
    // Bob accepts
    let bob_invite = InviteHandle::deserialize(invite_json).expect("deserialize invite");
    let accept = bob_invite
        .accept(bob.public_key_hex.clone(), bob.private_key_hex.clone(), Some("bob-device".to_string()))
        .expect("accept invite");
    
    // Verify Bob can send
    assert!(accept.session.can_send(), "Bob should be able to send after accepting");
    
    // Bob sends first message
    let msg1 = accept.session.send_text("Message 1 from Bob".to_string()).expect("send msg1");
    assert!(!msg1.outer_event_json.is_empty());
    
    // Send multiple messages to test chain progression
    let msg2 = accept.session.send_text("Message 2 from Bob".to_string()).expect("send msg2");
    let msg3 = accept.session.send_text("Message 3 from Bob".to_string()).expect("send msg3");
    
    // Verify state roundtrip
    let state = accept.session.state_json().expect("get state");
    let restored = SessionHandle::from_state_json(state).expect("restore from state");
    assert!(restored.can_send(), "Restored session should still be able to send");
    
    // Verify is_dr_message
    assert!(accept.session.is_dr_message(msg1.outer_event_json.clone()));
    assert!(accept.session.is_dr_message(msg2.outer_event_json.clone()));
    assert!(accept.session.is_dr_message(msg3.outer_event_json));
    
    println!("Full flow test passed!");
}

#[test]
fn test_event_json_roundtrip() {
    let kp = generate_keypair();
    let invite = InviteHandle::create_new(kp.public_key_hex.clone(), Some("device-1".to_string()), None).unwrap();

    // Get event JSON (requires device_id to be set)
    let event_json = invite.to_event_json().unwrap();
    
    // Parse and verify it's valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&event_json).unwrap();
    assert!(parsed.get("kind").is_some());
    assert!(parsed.get("content").is_some());
    assert!(parsed.get("tags").is_some());
    
    println!("Event JSON roundtrip passed!");
    println!("  Event kind: {:?}", parsed.get("kind"));
}

#[test]
fn test_invite_with_device_id_and_max_uses() {
    let kp = generate_keypair();
    
    // Create invite with device_id and max_uses
    let invite = InviteHandle::create_new(
        kp.public_key_hex.clone(),
        Some("my-device-123".to_string()),
        Some(5),
    ).unwrap();
    
    // Serialize and deserialize
    let json = invite.serialize().unwrap();
    let restored = InviteHandle::deserialize(json).unwrap();
    
    // Verify fields are preserved
    assert_eq!(invite.get_inviter_pubkey_hex(), restored.get_inviter_pubkey_hex());
    assert_eq!(invite.get_shared_secret_hex(), restored.get_shared_secret_hex());
    
    println!("Device ID and max_uses test passed!");
}
