use crate::{
    pubsub::build_filter, utils::pubkey_from_hex, SessionManagerEvent, MESSAGE_EVENT_KIND,
};
use nostr::{Filter, PublicKey};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const DIRECT_MESSAGE_RUNTIME_SUBSCRIPTION_ID: &str = "ndr-runtime-messages";
const DIRECT_MESSAGE_RUNTIME_SUBSCRIPTION_PREFIX: &str = "ndr-runtime-messages-";

#[derive(Debug, Default, Clone)]
pub struct DirectMessageSubscriptionTracker {
    authors_by_subid: HashMap<String, Vec<PublicKey>>,
    author_ref_counts: HashMap<PublicKey, usize>,
}

impl DirectMessageSubscriptionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_session_event(&mut self, event: &SessionManagerEvent) -> Vec<PublicKey> {
        match event {
            SessionManagerEvent::Subscribe { subid, filter_json } => {
                self.register_subscription(subid, filter_json)
            }
            SessionManagerEvent::Unsubscribe(subid) => {
                self.unregister_subscription(subid);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn register_subscription(
        &mut self,
        subid: impl AsRef<str>,
        filter_json: impl AsRef<str>,
    ) -> Vec<PublicKey> {
        let subid = subid.as_ref().trim();
        if subid.is_empty() {
            return Vec::new();
        }

        self.unregister_subscription(subid);

        let authors = direct_message_subscription_authors(subid, filter_json.as_ref());
        if authors.is_empty() {
            return Vec::new();
        }

        let mut added = Vec::new();
        for author in &authors {
            let ref_count = self.author_ref_counts.entry(*author).or_insert(0);
            if *ref_count == 0 {
                added.push(*author);
            }
            *ref_count += 1;
        }
        self.authors_by_subid.insert(subid.to_string(), authors);
        added.sort_by_key(|pubkey| pubkey.to_hex());
        added
    }

    pub fn unregister_subscription(&mut self, subid: impl AsRef<str>) {
        let subid = subid.as_ref().trim();
        if subid.is_empty() {
            return;
        }

        let Some(previous_authors) = self.authors_by_subid.remove(subid) else {
            return;
        };

        for author in previous_authors {
            let next_count = self
                .author_ref_counts
                .get(&author)
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            if next_count == 0 {
                self.author_ref_counts.remove(&author);
            } else {
                self.author_ref_counts.insert(author, next_count);
            }
        }
    }

    pub fn tracked_authors(&self) -> Vec<PublicKey> {
        let mut authors: Vec<PublicKey> = self.author_ref_counts.keys().copied().collect();
        authors.sort_by_key(|pubkey| pubkey.to_hex());
        authors
    }
}

pub fn build_direct_message_backfill_filter(
    authors: impl IntoIterator<Item = PublicKey>,
    since_seconds: u64,
    limit: usize,
) -> Filter {
    let mut unique_authors = Vec::new();
    let mut seen_authors = HashSet::new();
    for author in authors {
        if seen_authors.insert(author) {
            unique_authors.push(author);
        }
    }

    build_filter()
        .kinds(vec![MESSAGE_EVENT_KIND as u64])
        .authors(unique_authors)
        .since(since_seconds)
        .limit(limit)
        .build()
}

pub fn direct_message_subscription_authors(
    subid: impl AsRef<str>,
    filter_json: impl AsRef<str>,
) -> Vec<PublicKey> {
    let subid = subid.as_ref().trim();
    if subid.is_empty()
        || (subid != DIRECT_MESSAGE_RUNTIME_SUBSCRIPTION_ID
            && !subid.starts_with(DIRECT_MESSAGE_RUNTIME_SUBSCRIPTION_PREFIX))
    {
        return Vec::new();
    }

    let Ok(decoded) = serde_json::from_str::<Value>(filter_json.as_ref()) else {
        return Vec::new();
    };
    let Some(decoded_filter) = decoded.as_object() else {
        return Vec::new();
    };

    let Some(kinds) = decoded_filter.get("kinds").and_then(Value::as_array) else {
        return Vec::new();
    };
    if !kinds
        .iter()
        .any(|kind| kind.as_u64() == Some(MESSAGE_EVENT_KIND as u64))
    {
        return Vec::new();
    }

    let Some(authors) = decoded_filter.get("authors").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut parsed = Vec::new();
    let mut seen = HashSet::new();
    for author in authors {
        let Some(author_hex) = author.as_str() else {
            continue;
        };
        let normalized = author_hex.trim().to_lowercase();
        if normalized.len() != 64 {
            continue;
        }
        let Ok(pubkey) = pubkey_from_hex(&normalized) else {
            continue;
        };
        if seen.insert(pubkey) {
            parsed.push(pubkey);
        }
    }
    parsed
}
