use ndr_ffi::{
    create_signed_app_keys_event, generate_keypair, parse_app_keys_event,
    resolve_latest_app_keys_devices, FfiDeviceEntry,
};

fn find_entry<'a>(entries: &'a [FfiDeviceEntry], pubkey_hex: &str) -> &'a FfiDeviceEntry {
    entries
        .iter()
        .find(|entry| entry.identity_pubkey_hex == pubkey_hex)
        .expect("device entry present")
}

#[test]
fn app_keys_event_roundtrip_preserves_labels_when_owner_key_is_available() {
    let owner = generate_keypair();
    let linked = generate_keypair();

    let event_json = create_signed_app_keys_event(
        owner.public_key_hex.clone(),
        owner.private_key_hex.clone(),
        vec![
            FfiDeviceEntry {
                identity_pubkey_hex: owner.public_key_hex.clone(),
                created_at: 10,
                device_label: Some("MacBook".to_string()),
                client_label: Some("iris chat".to_string()),
            },
            FfiDeviceEntry {
                identity_pubkey_hex: linked.public_key_hex.clone(),
                created_at: 20,
                device_label: Some("Mini".to_string()),
                client_label: Some("iris chat".to_string()),
            },
        ],
    )
    .expect("create signed app keys event");

    let parsed_without_owner =
        parse_app_keys_event(event_json.clone(), None).expect("parse without owner key");
    assert_eq!(
        find_entry(&parsed_without_owner, &owner.public_key_hex).device_label,
        None
    );
    assert_eq!(
        find_entry(&parsed_without_owner, &linked.public_key_hex).client_label,
        None
    );

    let parsed_with_owner =
        parse_app_keys_event(event_json.clone(), Some(owner.private_key_hex.clone()))
            .expect("parse with owner key");
    assert_eq!(
        find_entry(&parsed_with_owner, &owner.public_key_hex).device_label,
        Some("MacBook".to_string())
    );
    assert_eq!(
        find_entry(&parsed_with_owner, &linked.public_key_hex).client_label,
        Some("iris chat".to_string())
    );

    let resolved = resolve_latest_app_keys_devices(vec![event_json], Some(owner.private_key_hex))
        .expect("resolve latest with owner key");
    assert_eq!(
        find_entry(&resolved, &linked.public_key_hex).device_label,
        Some("Mini".to_string())
    );
}
