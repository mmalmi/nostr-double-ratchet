use nostr_double_ratchet::group::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const ALICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BOB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CAROL: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[derive(Debug, Deserialize, Serialize)]
struct GroupVectors {
    description: String,
    create_group: CreateGroupVector,
    metadata_with_secret: String,
    metadata_without_secret: String,
    parse_vectors: Vec<ParseVector>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CreateGroupVector {
    input: CreateGroupInput,
    output: GroupData,
}

#[derive(Debug, Deserialize, Serialize)]
struct CreateGroupInput {
    name: String,
    creator: String,
    members: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ParseVector {
    description: String,
    input: String,
    expected: Option<GroupMetadata>,
}

fn get_test_vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .parent() // rust/
        .unwrap()
        .parent() // repo root
        .unwrap()
        .join("test-vectors")
}

#[test]
fn test_parse_typescript_group_vectors() {
    let vectors_path = get_test_vectors_path().join("ts-group-vectors.json");

    if !vectors_path.exists() {
        println!(
            "TypeScript group vectors not found at {:?}, skipping...",
            vectors_path
        );
        println!("Run `pnpm vitest run tests/Group.interop` in ts/ to generate them.");
        return;
    }

    let content = fs::read_to_string(&vectors_path).expect("Failed to read vectors");
    let vectors: GroupVectors = serde_json::from_str(&content).expect("Failed to parse vectors");

    println!("Loaded group vectors: {}", vectors.description);

    // Verify we can parse TS's metadata_with_secret
    let parsed = parse_group_metadata(&vectors.metadata_with_secret)
        .expect("Failed to parse TS metadata with secret");
    assert_eq!(parsed.id, vectors.create_group.output.id);
    assert_eq!(parsed.name, vectors.create_group.output.name);
    assert_eq!(parsed.members, vectors.create_group.output.members);
    assert_eq!(parsed.admins, vectors.create_group.output.admins);
    assert!(parsed.secret.is_some());
    assert_eq!(parsed.secret, vectors.create_group.output.secret);

    // Verify metadata_without_secret has no secret
    let parsed_no_secret = parse_group_metadata(&vectors.metadata_without_secret)
        .expect("Failed to parse TS metadata without secret");
    assert!(parsed_no_secret.secret.is_none());
    assert_eq!(parsed_no_secret.id, vectors.create_group.output.id);

    // Verify parse vectors
    for pv in &vectors.parse_vectors {
        let result = parse_group_metadata(&pv.input);
        match &pv.expected {
            None => {
                assert!(
                    result.is_none(),
                    "Expected None for '{}', got {:?}",
                    pv.description,
                    result
                );
            }
            Some(expected) => {
                let result = result
                    .unwrap_or_else(|| panic!("Expected Some for '{}', got None", pv.description));
                assert_eq!(
                    result.id, expected.id,
                    "id mismatch for '{}'",
                    pv.description
                );
                assert_eq!(
                    result.name, expected.name,
                    "name mismatch for '{}'",
                    pv.description
                );
                assert_eq!(
                    result.members, expected.members,
                    "members mismatch for '{}'",
                    pv.description
                );
                assert_eq!(
                    result.admins, expected.admins,
                    "admins mismatch for '{}'",
                    pv.description
                );
            }
        }
    }

    // Verify the created group structure
    assert_eq!(vectors.create_group.output.members, vec![ALICE, BOB, CAROL]);
    assert_eq!(vectors.create_group.output.admins, vec![ALICE]);

    println!("Successfully verified TypeScript group vectors!");
}

#[test]
fn test_generate_rust_group_vectors() {
    // Create a group with deterministic data
    let mut group = create_group_data("Interop Test Group", ALICE, &[BOB, CAROL]);
    // Override random fields for determinism
    group.id = "interop-test-group-id".to_string();
    group.created_at = 1700000000000;
    group.secret = Some("s".repeat(64));

    let metadata_with_secret = build_group_metadata_content(&group, false);
    let metadata_without_secret = build_group_metadata_content(&group, true);

    let parse_vectors = vec![
        ParseVector {
            description: "valid metadata with all fields".to_string(),
            input: serde_json::json!({
                "id": "g1",
                "name": "Test",
                "description": "A test group",
                "picture": "https://example.com/pic.jpg",
                "members": [ALICE, BOB],
                "admins": [ALICE],
                "secret": "x".repeat(64)
            })
            .to_string(),
            expected: Some(GroupMetadata {
                id: "g1".to_string(),
                name: "Test".to_string(),
                description: Some("A test group".to_string()),
                picture: Some("https://example.com/pic.jpg".to_string()),
                members: vec![ALICE.to_string(), BOB.to_string()],
                admins: vec![ALICE.to_string()],
                secret: Some("x".repeat(64)),
            }),
        },
        ParseVector {
            description: "valid metadata without optional fields".to_string(),
            input: serde_json::json!({
                "id": "g2",
                "name": "Minimal",
                "members": [ALICE],
                "admins": [ALICE]
            })
            .to_string(),
            expected: Some(GroupMetadata {
                id: "g2".to_string(),
                name: "Minimal".to_string(),
                description: None,
                picture: None,
                members: vec![ALICE.to_string()],
                admins: vec![ALICE.to_string()],
                secret: None,
            }),
        },
        ParseVector {
            description: "invalid - missing id".to_string(),
            input: serde_json::json!({
                "name": "Bad",
                "members": [ALICE],
                "admins": [ALICE]
            })
            .to_string(),
            expected: None,
        },
        ParseVector {
            description: "invalid - empty admins".to_string(),
            input: serde_json::json!({
                "id": "g3",
                "name": "Bad",
                "members": [ALICE],
                "admins": []
            })
            .to_string(),
            expected: None,
        },
        ParseVector {
            description: "invalid - not JSON".to_string(),
            input: "not json".to_string(),
            expected: None,
        },
    ];

    let vectors = GroupVectors {
        description: "Group test vectors generated by Rust".to_string(),
        create_group: CreateGroupVector {
            input: CreateGroupInput {
                name: "Interop Test Group".to_string(),
                creator: ALICE.to_string(),
                members: vec![BOB.to_string(), CAROL.to_string()],
            },
            output: group,
        },
        metadata_with_secret,
        metadata_without_secret,
        parse_vectors,
    };

    let output_path = get_test_vectors_path().join("rust-group-vectors.json");
    fs::create_dir_all(output_path.parent().unwrap()).ok();
    fs::write(
        &output_path,
        serde_json::to_string_pretty(&vectors).unwrap(),
    )
    .expect("Failed to write vectors");

    println!("Generated Rust group vectors at {:?}", output_path);

    // Self-verify: parse our own output
    let parsed = parse_group_metadata(&vectors.metadata_with_secret).unwrap();
    assert_eq!(parsed.id, "interop-test-group-id");
    assert_eq!(parsed.name, "Interop Test Group");
    assert_eq!(parsed.members, vec![ALICE, BOB, CAROL]);
    assert_eq!(parsed.secret, Some("s".repeat(64)));

    let parsed_no_secret = parse_group_metadata(&vectors.metadata_without_secret).unwrap();
    assert!(parsed_no_secret.secret.is_none());
}
