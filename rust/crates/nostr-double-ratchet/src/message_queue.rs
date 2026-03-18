use crate::Result;
use nostr::UnsignedEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub id: String,
    pub target_key: String,
    pub event: UnsignedEvent,
    pub created_at: u64,
}

#[derive(Clone)]
pub struct MessageQueue {
    entries: Arc<Mutex<HashMap<String, QueueEntry>>>,
    prefix: String,
}

impl MessageQueue {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            prefix: prefix.into(),
        }
    }

    pub fn key(&self, id: &str) -> String {
        format!("{}{}", self.prefix, id)
    }

    fn event_id_or_random(event: &UnsignedEvent) -> String {
        event
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    pub fn add(&self, target_key: &str, event: &UnsignedEvent, created_at: u64) -> Result<QueueEntry> {
        let id = format!("{}/{}", Self::event_id_or_random(event), target_key);
        let entry = QueueEntry {
            id: id.clone(),
            target_key: target_key.to_string(),
            event: event.clone(),
            created_at,
        };
        self.entries.lock().unwrap().insert(id, entry.clone());
        Ok(entry)
    }

    pub fn import_entries(&self, entries: impl IntoIterator<Item = QueueEntry>) {
        let mut stored = self.entries.lock().unwrap();
        for entry in entries {
            stored.insert(entry.id.clone(), entry);
        }
    }

    pub fn get_for_target(&self, target_key: &str) -> Result<Vec<QueueEntry>> {
        let mut out: Vec<QueueEntry> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.target_key == target_key)
            .cloned()
            .collect();
        out.sort_by_key(|entry| entry.created_at);
        Ok(out)
    }

    pub fn remove_for_target(&self, target_key: &str) -> Result<Vec<QueueEntry>> {
        let mut stored = self.entries.lock().unwrap();
        let ids: Vec<String> = stored
            .iter()
            .filter_map(|(id, entry)| (entry.target_key == target_key).then_some(id.clone()))
            .collect();
        let removed = ids
            .into_iter()
            .filter_map(|id| stored.remove(&id))
            .collect();
        Ok(removed)
    }

    pub fn remove_by_target_and_event_id(
        &self,
        target_key: &str,
        event_id: &str,
    ) -> Result<Option<QueueEntry>> {
        self.remove(&format!("{}/{}", event_id, target_key))
    }

    pub fn remove(&self, id: &str) -> Result<Option<QueueEntry>> {
        Ok(self.entries.lock().unwrap().remove(id))
    }

    pub fn remove_expired(&self, now_ms: u64, max_age_ms: u64) -> Result<Vec<QueueEntry>> {
        let mut stored = self.entries.lock().unwrap();
        let ids: Vec<String> = stored
            .iter()
            .filter_map(|(id, entry)| {
                (now_ms.saturating_sub(entry.created_at) > max_age_ms).then_some(id.clone())
            })
            .collect();
        let removed = ids
            .into_iter()
            .filter_map(|id| stored.remove(&id))
            .collect();
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Timestamp};

    fn make_rumor(content: &str, created_at: u64) -> UnsignedEvent {
        let mut event = EventBuilder::new(Kind::TextNote, content)
            .custom_created_at(Timestamp::from(created_at))
            .build(Keys::generate().public_key());
        event.ensure_id();
        event
    }

    #[test]
    fn add_and_get_for_target_returns_sorted_entries() {
        let queue = MessageQueue::new("v1/test-queue/");
        let event1 = make_rumor("first", 1);
        let event2 = make_rumor("second", 2);
        queue.import_entries(vec![
            QueueEntry {
                id: format!("{}/{}", event2.id.as_ref().unwrap(), "device-a"),
                target_key: "device-a".to_string(),
                event: event2.clone(),
                created_at: 200,
            },
            QueueEntry {
                id: format!("{}/{}", event1.id.as_ref().unwrap(), "device-a"),
                target_key: "device-a".to_string(),
                event: event1.clone(),
                created_at: 100,
            },
        ]);

        let entries = queue.get_for_target("device-a").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event.id, event1.id);
        assert_eq!(entries[1].event.id, event2.id);
    }

    #[test]
    fn remove_by_target_and_event_id_only_removes_matching_entry() {
        let queue = MessageQueue::new("v1/test-queue/");
        let event = make_rumor("hello", 1);
        let event_id = event.id.as_ref().unwrap().to_string();

        queue.add("device-a", &event, 100).unwrap();
        queue.add("device-b", &event, 200).unwrap();
        queue
            .remove_by_target_and_event_id("device-a", &event_id)
            .unwrap();

        assert!(queue.get_for_target("device-a").unwrap().is_empty());
        assert_eq!(queue.get_for_target("device-b").unwrap().len(), 1);
    }

    #[test]
    fn different_prefixes_do_not_interfere() {
        let queue_a = MessageQueue::new("v1/message-queue/");
        let queue_b = MessageQueue::new("v1/discovery-queue/");
        let event1 = make_rumor("a", 1);
        let event2 = make_rumor("b", 2);

        queue_a.add("target-1", &event1, 100).unwrap();
        queue_b.add("target-1", &event2, 200).unwrap();

        let entries_a = queue_a.get_for_target("target-1").unwrap();
        let entries_b = queue_b.get_for_target("target-1").unwrap();
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_b.len(), 1);
        assert_eq!(entries_a[0].event.id, event1.id);
        assert_eq!(entries_b[0].event.id, event2.id);
    }

    #[test]
    fn remove_expired_returns_removed_entries() {
        let queue = MessageQueue::new("v1/message-queue/");
        let old_event = make_rumor("old", 1);
        let new_event = make_rumor("new", 2);

        queue.add("target-1", &old_event, 100).unwrap();
        queue.add("target-1", &new_event, 250).unwrap();

        let removed = queue.remove_expired(400, 200).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].event.id, old_event.id);

        let remaining = queue.get_for_target("target-1").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event.id, new_event.id);
    }
}
