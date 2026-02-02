use std::path::Path;

mod helpers {
    use std::path::Path;

    pub struct TestInstance {
        pub _temp: tempfile::TempDir,
        pub data_dir: std::path::PathBuf,
        pub pubkey: String,
    }

    pub fn create_instance() -> TestInstance {
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&data_dir).unwrap();

        // Generate an identity
        let mut config = ndr_config_load(&data_dir);
        let (pubkey, _) = config.ensure_identity().unwrap();

        TestInstance {
            _temp: temp,
            data_dir,
            pubkey,
        }
    }

    // Re-implement minimal config/storage access to avoid importing ndr internals
    // since ndr is a binary crate. We use the library types directly.

    pub fn ndr_config_load(data_dir: &Path) -> NdrConfig {
        let config_path = data_dir.join("config.json");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).unwrap();
            let mut config: NdrConfig = serde_json::from_str(&content).unwrap();
            config.path = config_path;
            config
        } else {
            NdrConfig {
                private_key: None,
                relays: vec![],
                path: config_path,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct NdrConfig {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub private_key: Option<String>,
        #[serde(default)]
        pub relays: Vec<String>,
        #[serde(skip)]
        pub path: std::path::PathBuf,
    }

    impl NdrConfig {
        pub fn ensure_identity(&mut self) -> anyhow::Result<(String, bool)> {
            if let Some(ref key) = self.private_key {
                let bytes = hex::decode(key)?;
                let sk = nostr::SecretKey::from_slice(&bytes)?;
                let keys = nostr::Keys::new(sk);
                return Ok((keys.public_key().to_hex(), false));
            }
            let keys = nostr::Keys::generate();
            let sk_hex = keys.secret_key().to_secret_hex();
            self.private_key = Some(sk_hex);
            let content = serde_json::to_string_pretty(self)?;
            std::fs::write(&self.path, content)?;
            Ok((keys.public_key().to_hex(), true))
        }

        #[allow(dead_code)]
        pub fn public_key(&self) -> String {
            let key = self.private_key.as_ref().unwrap();
            let bytes = hex::decode(key).unwrap();
            let sk = nostr::SecretKey::from_slice(&bytes).unwrap();
            let keys = nostr::Keys::new(sk);
            keys.public_key().to_hex()
        }
    }
}

/// Minimal storage wrapper for groups (mirrors ndr storage pattern)
struct GroupStorage {
    groups_dir: std::path::PathBuf,
}

impl GroupStorage {
    fn open(data_dir: &Path) -> Self {
        let groups_dir = data_dir.join("groups");
        std::fs::create_dir_all(&groups_dir).unwrap();
        GroupStorage { groups_dir }
    }

    fn save(&self, group: &nostr_double_ratchet::group::GroupData) {
        let path = self.groups_dir.join(format!("{}.json", group.id));
        let content = serde_json::to_string_pretty(group).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn get(&self, id: &str) -> Option<nostr_double_ratchet::group::GroupData> {
        let path = self.groups_dir.join(format!("{}.json", id));
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).unwrap();
        Some(serde_json::from_str(&content).unwrap())
    }

    fn list(&self) -> Vec<nostr_double_ratchet::group::GroupData> {
        let mut groups = Vec::new();
        for entry in std::fs::read_dir(&self.groups_dir).unwrap() {
            let entry = entry.unwrap();
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = std::fs::read_to_string(entry.path()).unwrap();
                groups.push(serde_json::from_str(&content).unwrap());
            }
        }
        groups
    }

    fn delete(&self, id: &str) -> bool {
        let path = self.groups_dir.join(format!("{}.json", id));
        if path.exists() {
            std::fs::remove_file(path).unwrap();
            true
        } else {
            false
        }
    }
}

#[test]
fn e2e_group_lifecycle() {
    use nostr_double_ratchet::group::*;

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();
    let carol = helpers::create_instance();

    let alice_storage = GroupStorage::open(&alice.data_dir);

    // 1. Alice creates group with Bob as member
    let group = create_group_data("Test Group", &alice.pubkey, &[&bob.pubkey]);
    alice_storage.save(&group);

    // 2. Verify group list shows the group
    let groups = alice_storage.list();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "Test Group");
    assert_eq!(
        groups[0].members,
        vec![alice.pubkey.as_str(), bob.pubkey.as_str()]
    );
    assert_eq!(groups[0].admins, vec![alice.pubkey.as_str()]);

    // 3. Alice updates group name
    let updated = update_group_data(
        &group,
        &GroupUpdate {
            name: Some("Renamed Group".to_string()),
            description: None,
            picture: None,
        },
        &alice.pubkey,
    )
    .unwrap();
    alice_storage.save(&updated);

    let loaded = alice_storage.get(&group.id).unwrap();
    assert_eq!(loaded.name, "Renamed Group");

    // 4. Alice adds Carol
    let updated = add_group_member(&loaded, &carol.pubkey, &alice.pubkey).unwrap();
    alice_storage.save(&updated);

    let loaded = alice_storage.get(&group.id).unwrap();
    assert!(loaded.members.contains(&carol.pubkey));
    assert_eq!(loaded.members.len(), 3);

    // 5. Alice removes Carol
    let updated = remove_group_member(&loaded, &carol.pubkey, &alice.pubkey).unwrap();
    alice_storage.save(&updated);

    let loaded = alice_storage.get(&group.id).unwrap();
    assert!(!loaded.members.contains(&carol.pubkey));
    assert_eq!(loaded.members.len(), 2);

    // 6. Alice promotes Bob to admin
    let updated = add_group_admin(&loaded, &bob.pubkey, &alice.pubkey).unwrap();
    alice_storage.save(&updated);

    let loaded = alice_storage.get(&group.id).unwrap();
    assert!(loaded.admins.contains(&bob.pubkey));
    assert_eq!(loaded.admins.len(), 2);

    // 7. Alice demotes Bob
    let updated = remove_group_admin(&loaded, &bob.pubkey, &alice.pubkey).unwrap();
    alice_storage.save(&updated);

    let loaded = alice_storage.get(&group.id).unwrap();
    assert!(!loaded.admins.contains(&bob.pubkey));
    assert_eq!(loaded.admins.len(), 1);

    // 8. Alice deletes group
    assert!(alice_storage.delete(&group.id));
    assert!(alice_storage.get(&group.id).is_none());
    assert!(alice_storage.list().is_empty());
}

#[test]
fn e2e_group_metadata_serialization_roundtrip() {
    use nostr_double_ratchet::group::*;

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();

    let alice_storage = GroupStorage::open(&alice.data_dir);

    // Create and save group
    let group = create_group_data("Serialization Test", &alice.pubkey, &[&bob.pubkey]);
    alice_storage.save(&group);

    // Build metadata content (as would be sent over the wire)
    let content = build_group_metadata_content(&group, false);
    let metadata = parse_group_metadata(&content).unwrap();

    assert_eq!(metadata.id, group.id);
    assert_eq!(metadata.name, group.name);
    assert_eq!(metadata.members, group.members);
    assert_eq!(metadata.admins, group.admins);
    assert_eq!(metadata.secret, group.secret);

    // Validate as if Bob received this creation metadata
    assert!(validate_metadata_creation(
        &metadata,
        &alice.pubkey,
        &bob.pubkey
    ));

    // Bob would apply it to create his local copy
    let bob_storage = GroupStorage::open(&bob.data_dir);
    let bob_group = GroupData {
        id: metadata.id.clone(),
        name: metadata.name,
        description: metadata.description,
        picture: metadata.picture,
        members: metadata.members,
        admins: metadata.admins,
        created_at: group.created_at,
        secret: metadata.secret,
        accepted: Some(true),
    };
    bob_storage.save(&bob_group);

    // Verify Bob's copy matches
    let bob_loaded = bob_storage.get(&group.id).unwrap();
    assert_eq!(bob_loaded.name, "Serialization Test");
    assert_eq!(bob_loaded.members.len(), 2);
}

#[test]
fn e2e_group_permission_checks() {
    use nostr_double_ratchet::group::*;

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();
    let carol = helpers::create_instance();

    // Alice creates group, Bob is member but not admin
    let group = create_group_data("Perms Test", &alice.pubkey, &[&bob.pubkey]);

    // Bob cannot add members
    assert!(add_group_member(&group, &carol.pubkey, &bob.pubkey).is_none());

    // Bob cannot remove members
    assert!(remove_group_member(&group, &alice.pubkey, &bob.pubkey).is_none());

    // Bob cannot update group
    assert!(update_group_data(
        &group,
        &GroupUpdate {
            name: Some("Hacked".to_string()),
            description: None,
            picture: None,
        },
        &bob.pubkey,
    )
    .is_none());

    // Bob cannot promote himself
    assert!(add_group_admin(&group, &bob.pubkey, &bob.pubkey).is_none());

    // Alice cannot remove herself (only admin)
    assert!(remove_group_admin(&group, &alice.pubkey, &alice.pubkey).is_none());

    // Alice cannot remove herself as member
    assert!(remove_group_member(&group, &alice.pubkey, &alice.pubkey).is_none());
}
