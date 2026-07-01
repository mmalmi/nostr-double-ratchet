use nostr::Keys;
use nostr_double_ratchet::{
    deterministic_link_invite_for_device_link_request, encode_compact_device_link_request,
    parse_compact_device_link_request,
};

#[test]
fn compact_device_link_request_round_trips() {
    let device = Keys::generate();
    let request = Keys::generate();
    let code = encode_compact_device_link_request(
        device.public_key(),
        &request.secret_key().to_secret_hex(),
    )
    .expect("encode compact request");

    assert_eq!(code.len(), 129);
    let parsed = parse_compact_device_link_request(&code).expect("parse compact request");
    assert_eq!(parsed.device_app_key_pubkey, device.public_key());
    assert_eq!(parsed.request_pubkey, request.public_key());
    assert_eq!(parsed.request_secret, request.secret_key().to_secret_hex());
}

#[test]
fn compact_device_link_request_rejects_malformed_inputs() {
    assert!(parse_compact_device_link_request("").is_err());
    assert!(parse_compact_device_link_request("npub1plainvalue").is_err());
    assert!(parse_compact_device_link_request("https://example.com").is_err());
    assert!(parse_compact_device_link_request(&format!(
        "{}.{}.extra",
        "1".repeat(64),
        "1".repeat(64)
    ))
    .is_err());
}

#[test]
fn deterministic_device_link_invite_matches_typescript_vector() {
    let request_secret = "0100000017000000c8010000d21e000000000000000000000000000000000000";
    let device_pubkey = "e".repeat(64);
    let request = parse_compact_device_link_request(&format!("{device_pubkey}.{request_secret}"))
        .expect("parse vector");

    let invite =
        deterministic_link_invite_for_device_link_request(&request).expect("create invite");
    let repeated =
        deterministic_link_invite_for_device_link_request(&request).expect("repeat invite");

    assert!(
        hex::encode(invite.inviter_ephemeral_private_key.unwrap()).starts_with("be3f1cca6354c294")
    );
    assert_eq!(
        invite.inviter_ephemeral_public_key,
        repeated.inviter_ephemeral_public_key
    );
    assert_eq!(invite.shared_secret, repeated.shared_secret);
    assert_eq!(invite.inviter.to_hex(), device_pubkey);
    assert_eq!(invite.device_id.as_deref(), Some(device_pubkey.as_str()));
    assert_eq!(invite.max_uses, Some(1));
    assert_eq!(invite.created_at.get(), 0);
    assert_eq!(invite.purpose.as_deref(), Some("link"));
}
