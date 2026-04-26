use nostr::Keys;
use nostr_double_ratchet::{
    build_direct_message_backfill_filter, direct_message_subscription_authors,
    DirectMessageSubscriptionTracker, SessionManagerEvent, MESSAGE_EVENT_KIND,
};

#[test]
fn tracker_returns_only_new_direct_message_authors() {
    let alice = Keys::generate().public_key();
    let bob = Keys::generate().public_key();

    let mut tracker = DirectMessageSubscriptionTracker::new();

    let added = tracker.register_subscription(
        "ndr-runtime-messages-1",
        format!(
            r#"{{"kinds":[1060],"authors":["{}","{}"]}}"#,
            alice.to_hex().to_uppercase(),
            bob.to_hex()
        ),
    );
    let mut expected_authors = vec![alice, bob];
    expected_authors.sort_by_key(|pubkey| pubkey.to_hex());
    assert_eq!(added, expected_authors);
    assert_eq!(tracker.tracked_authors(), expected_authors);

    let duplicate = tracker.register_subscription(
        "ndr-runtime-messages-2",
        format!(r#"{{"kinds":[1060],"authors":["{}"]}}"#, bob.to_hex()),
    );
    assert!(duplicate.is_empty());
    assert_eq!(tracker.tracked_authors(), expected_authors);

    tracker.unregister_subscription("ndr-runtime-messages-1");
    assert_eq!(tracker.tracked_authors(), vec![bob]);
}

#[test]
fn tracker_can_apply_session_manager_events() {
    let alice = Keys::generate().public_key();
    let mut tracker = DirectMessageSubscriptionTracker::new();

    let added = tracker.apply_session_event(&SessionManagerEvent::Subscribe {
        subid: "ndr-runtime-messages-1".to_string(),
        filter_json: format!(r#"{{"kinds":[1060],"authors":["{}"]}}"#, alice.to_hex()),
    });
    assert_eq!(added, vec![alice]);

    let removed = tracker.apply_session_event(&SessionManagerEvent::Unsubscribe(
        "ndr-runtime-messages-1".to_string(),
    ));
    assert!(removed.is_empty());
    assert!(tracker.tracked_authors().is_empty());
}

#[test]
fn helper_ignores_non_session_or_invalid_filters() {
    assert!(direct_message_subscription_authors(
        "group-sender-event-1",
        r#"{"kinds":[1060],"authors":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
    )
    .is_empty());

    assert!(direct_message_subscription_authors(
        "ndr-runtime-messages-1",
        r#"{"kinds":[1],"authors":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
    )
    .is_empty());

    assert!(direct_message_subscription_authors(
        "session-next-1",
        r#"{"kinds":[1060],"authors":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
    )
    .is_empty());

    assert!(direct_message_subscription_authors("ndr-runtime-messages-1", "not json").is_empty());
}

#[test]
fn build_direct_message_backfill_filter_targets_message_kind_and_authors() {
    let alice = Keys::generate().public_key();
    let bob = Keys::generate().public_key();

    let filter = build_direct_message_backfill_filter(vec![alice, bob, alice], 1234, 200);
    let json = serde_json::to_value(filter).expect("serialize filter");

    assert_eq!(
        json.get("kinds").and_then(|value| value.as_array()),
        Some(&vec![serde_json::json!(MESSAGE_EVENT_KIND)])
    );
    assert_eq!(
        json.get("authors")
            .and_then(|value| value.as_array())
            .map(|authors| authors.len()),
        Some(2)
    );
    assert_eq!(
        json.get("since").and_then(|value| value.as_u64()),
        Some(1234)
    );
    assert_eq!(
        json.get("limit").and_then(|value| value.as_u64()),
        Some(200)
    );
}
