use nostr::Keys;
use nostr_double_ratchet::{AppKeys, DeviceEntry, Result};

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
