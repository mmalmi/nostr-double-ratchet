use nostr::Keys;
use nostr_double_ratchet::Result;
use nostr_double_ratchet_nostr::{
    AppKeys, DeviceEntry, NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT,
    NOSTR_IDENTITY_ROSTER_OP_KIND, NOSTR_IDENTITY_ROSTER_TYPE,
};

const PROFILE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

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
        values.first().map(|value| value.as_str())
            == Some(NOSTR_IDENTITY_ENCRYPTED_DEVICE_LABELS_FACT)
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
fn test_app_keys_project_canonical_nostr_identity_facets_without_labels() -> Result<()> {
    let admin = Keys::generate();
    let device = Keys::generate();
    let admin_pubkey = admin.public_key().to_hex();
    let device_pubkey = device.public_key().to_hex();

    let bootstrap = nostr_identity_roster_event(
        &admin,
        vec![
            vec!["op", "add_key"],
            vec!["key_pubkey", &admin_pubkey],
            vec!["key_purpose", "app"],
            vec!["key_capability", "admin"],
            vec!["key_capability", "write"],
            vec!["key_capability", "receive_secret_wraps"],
            vec!["key_capability", "decrypt_secret_epochs"],
            vec!["key_added_at", "10"],
            vec!["key_label", "Private laptop"],
        ],
        10,
    );
    let add_device = nostr_identity_roster_event(
        &admin,
        vec![
            vec!["op", "add_key"],
            vec!["key_pubkey", &device_pubkey],
            vec!["key_purpose", "app"],
            vec!["key_capability", "write"],
            vec!["key_capability", "receive_secret_wraps"],
            vec!["key_capability", "decrypt_secret_epochs"],
            vec!["key_added_at", "11"],
            vec!["key_label", "Phone"],
        ],
        11,
    );

    let projected =
        AppKeys::from_nostr_identity_roster_events(PROFILE_ID, [&add_device, &bootstrap])?;
    let devices = projected.get_all_devices();

    assert_eq!(devices.len(), 2);
    assert!(projected.get_device(&admin.public_key()).is_some());
    assert!(projected.get_device(&device.public_key()).is_some());
    assert!(!projected.serialize()?.contains("Private laptop"));
    assert!(!projected.serialize()?.contains("Phone"));

    Ok(())
}

fn nostr_identity_roster_event(
    signer: &Keys,
    facts: Vec<Vec<&str>>,
    created_at: u64,
) -> nostr::Event {
    use nostr::{EventBuilder, Kind, Tag, Timestamp};

    let mut tags = vec![
        Tag::parse(["i", PROFILE_ID, "subject"]).unwrap(),
        Tag::parse(["type", NOSTR_IDENTITY_ROSTER_TYPE]).unwrap(),
        Tag::parse(["schema", "1"]).unwrap(),
        Tag::parse(["actor_pubkey", &signer.public_key().to_hex()]).unwrap(),
        Tag::parse(["client_nonce", &format!("nonce-{created_at}")]).unwrap(),
        Tag::parse(["created_at", &created_at.to_string()]).unwrap(),
    ];
    for fact in facts {
        tags.push(Tag::parse(fact).unwrap());
    }

    EventBuilder::new(Kind::from(NOSTR_IDENTITY_ROSTER_OP_KIND as u16), "")
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at))
        .sign_with_keys(signer)
        .unwrap()
}
