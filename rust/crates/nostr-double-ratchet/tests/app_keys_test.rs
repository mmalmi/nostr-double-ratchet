use nostr::Keys;
use nostr_double_ratchet::Result;
use nostr_double_ratchet::{
    build_app_keys_device_authorization_filter, resolve_app_keys_owner_for_device, AppKeys,
    DeviceEntry, APP_KEYS_ENCRYPTED_DEVICE_LABELS_FACT,
};

#[test]
fn test_app_keys_roundtrip_and_merge() -> Result<()> {
    let owner_keys = Keys::generate();
    let device1 = Keys::generate();
    let device2 = Keys::generate();

    let app_keys = AppKeys::new(vec![
        DeviceEntry::new(device1.public_key(), 100),
        DeviceEntry::new(device2.public_key(), 200),
    ]);

    let event = app_keys.get_event(owner_keys.public_key());
    let signed = event.sign_with_keys(&owner_keys)?;

    let parsed = AppKeys::from_event(&signed)?;
    assert_eq!(parsed.get_all_devices().len(), 2);
    assert!(parsed.get_device(&device1.public_key()).is_some());
    assert!(parsed.get_device(&device2.public_key()).is_some());

    // Merge prefers earlier created_at for duplicates
    let mut other = AppKeys::new(vec![DeviceEntry::new(device1.public_key(), 50)]);
    other.add_device(DeviceEntry::new(device2.public_key(), 300));

    let merged = app_keys.merge(&other);
    let merged_device1 = merged.get_device(&device1.public_key()).unwrap();
    assert_eq!(merged_device1.created_at, 50);

    Ok(())
}

#[test]
fn test_app_keys_encrypts_labels_in_event_content() -> Result<()> {
    let owner_keys = Keys::generate();
    let device = Keys::generate();

    let mut app_keys = AppKeys::new(vec![DeviceEntry::new(device.public_key(), 100)]);
    app_keys.set_device_labels(
        device.public_key(),
        Some("Sirius MacBook".to_string()),
        Some("NDR Desktop".to_string()),
        Some(150),
    );

    let event = app_keys.get_encrypted_event(&owner_keys)?;

    assert!(event.content.is_empty());
    assert!(event.tags.iter().any(|tag| {
        let values = tag.clone().to_vec();
        values.first().map(|value| value.as_str()) == Some(APP_KEYS_ENCRYPTED_DEVICE_LABELS_FACT)
            && values.get(1).is_some_and(|value| !value.is_empty())
    }));
    assert!(!event.content.contains("Sirius MacBook"));
    assert!(!event.content.contains("NDR Desktop"));

    Ok(())
}

#[test]
fn test_app_keys_owner_can_decrypt_labels_but_public_parsing_cannot() -> Result<()> {
    let owner_keys = Keys::generate();
    let device = Keys::generate();

    let mut app_keys = AppKeys::new(vec![DeviceEntry::new(device.public_key(), 100)]);
    app_keys.set_device_labels(
        device.public_key(),
        Some("Office Laptop".to_string()),
        Some("NDR Mobile".to_string()),
        Some(200),
    );

    let signed = app_keys
        .get_encrypted_event(&owner_keys)?
        .sign_with_keys(&owner_keys)?;

    let parsed_public = AppKeys::from_event(&signed)?;
    assert!(parsed_public
        .get_device_labels(&device.public_key())
        .is_none());

    let parsed_owner = AppKeys::from_event_with_labels(&signed, &owner_keys)?;
    let labels = parsed_owner
        .get_device_labels(&device.public_key())
        .unwrap();
    assert_eq!(labels.device_label.as_deref(), Some("Office Laptop"));
    assert_eq!(labels.client_label.as_deref(), Some("NDR Mobile"));
    assert_eq!(labels.updated_at, 200);

    Ok(())
}

#[test]
fn test_app_keys_device_authorization_filter_and_owner_resolution() -> Result<()> {
    let owner_keys = Keys::generate();
    let device = Keys::generate();
    let other_device = Keys::generate();
    let app_keys = AppKeys::new(vec![DeviceEntry::new(device.public_key(), 100)]);

    let filter = build_app_keys_device_authorization_filter(device.public_key());
    let filter_json = serde_json::to_value(&filter)?;
    assert_eq!(filter_json["kinds"], serde_json::json!([37368]));
    assert_eq!(
        filter_json["#p"],
        serde_json::json!([device.public_key().to_hex()])
    );

    let signed = app_keys
        .get_event_at(owner_keys.public_key(), 1700000300)
        .sign_with_keys(&owner_keys)?;

    assert_eq!(
        resolve_app_keys_owner_for_device(&signed, device.public_key())?,
        Some(owner_keys.public_key())
    );
    assert_eq!(
        resolve_app_keys_owner_for_device(&signed, other_device.public_key())?,
        None
    );

    Ok(())
}
