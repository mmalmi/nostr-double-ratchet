use nostr::Keys;
use nostr_double_ratchet::Result;
use nostr_double_ratchet_nostr::{AppKeys, DeviceEntry};

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

    assert!(!event.content.is_empty());
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
