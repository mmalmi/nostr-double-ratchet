use crate::{Result, StorageAdapter};
use nostr::UnsignedEvent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    storage: Arc<dyn StorageAdapter>,
    prefix: String,
}

impl MessageQueue {
    pub fn new(storage: Arc<dyn StorageAdapter>, prefix: impl Into<String>) -> Self {
        Self {
            storage,
            prefix: prefix.into(),
        }
    }

    fn key(&self, id: &str) -> String {
        format!("{}{}", self.prefix, id)
    }

    fn event_id_or_random(event: &UnsignedEvent) -> String {
        event
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    pub fn add(&self, target_key: &str, event: &UnsignedEvent) -> Result<String> {
        let id = format!("{}/{}", Self::event_id_or_random(event), target_key);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let entry = QueueEntry {
            id: id.clone(),
            target_key: target_key.to_string(),
            event: event.clone(),
            created_at,
        };
        self.storage
            .put(&self.key(&id), serde_json::to_string(&entry)?)?;
        Ok(id)
    }

    pub fn get_for_target(&self, target_key: &str) -> Result<Vec<QueueEntry>> {
        let keys = self.storage.list(&self.prefix)?;
        let mut out = Vec::new();
        for key in keys {
            let Some(raw) = self.storage.get(&key)? else {
                continue;
            };
            let Ok(entry) = serde_json::from_str::<QueueEntry>(&raw) else {
                continue;
            };
            if entry.target_key == target_key {
                out.push(entry);
            }
        }
        out.sort_by_key(|entry| entry.created_at);
        Ok(out)
    }

    pub fn remove_for_target(&self, target_key: &str) -> Result<()> {
        let keys = self.storage.list(&self.prefix)?;
        for key in keys {
            let Some(raw) = self.storage.get(&key)? else {
                continue;
            };
            let Ok(entry) = serde_json::from_str::<QueueEntry>(&raw) else {
                continue;
            };
            if entry.target_key == target_key {
                let _ = self.storage.del(&key);
            }
        }
        Ok(())
    }

    pub fn remove_by_target_and_event_id(&self, target_key: &str, event_id: &str) -> Result<()> {
        self.remove(&format!("{}/{}", event_id, target_key))
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.storage.del(&self.key(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStorage;
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
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let queue = MessageQueue::new(storage.clone(), "v1/test-queue/");
        let event1 = make_rumor("first", 1);
        let event2 = make_rumor("second", 2);

        let id_late = format!("{}/{}", event2.id.as_ref().unwrap(), "device-a".to_string());
        let id_early = format!("{}/{}", event1.id.as_ref().unwrap(), "device-a".to_string());
        storage
            .put(
                &format!("v1/test-queue/{}", id_late),
                serde_json::to_string(&QueueEntry {
                    id: id_late,
                    target_key: "device-a".to_string(),
                    event: event2.clone(),
                    created_at: 200,
                })
                .unwrap(),
            )
            .unwrap();
        storage
            .put(
                &format!("v1/test-queue/{}", id_early),
                serde_json::to_string(&QueueEntry {
                    id: id_early,
                    target_key: "device-a".to_string(),
                    event: event1.clone(),
                    created_at: 100,
                })
                .unwrap(),
            )
            .unwrap();

        let entries = queue.get_for_target("device-a").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event.id, event1.id);
        assert_eq!(entries[1].event.id, event2.id);
    }

    #[test]
    fn remove_by_target_and_event_id_only_removes_matching_entry() {
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let queue = MessageQueue::new(storage, "v1/test-queue/");
        let event = make_rumor("hello", 1);
        let event_id = event.id.as_ref().unwrap().to_string();

        queue.add("device-a", &event).unwrap();
        queue.add("device-b", &event).unwrap();
        queue
            .remove_by_target_and_event_id("device-a", &event_id)
            .unwrap();

        assert!(queue.get_for_target("device-a").unwrap().is_empty());
        assert_eq!(queue.get_for_target("device-b").unwrap().len(), 1);
    }

    #[test]
    fn different_prefixes_do_not_interfere() {
        let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
        let queue_a = MessageQueue::new(storage.clone(), "v1/message-queue/");
        let queue_b = MessageQueue::new(storage, "v1/discovery-queue/");
        let event1 = make_rumor("a", 1);
        let event2 = make_rumor("b", 2);

        queue_a.add("target-1", &event1).unwrap();
        queue_b.add("target-1", &event2).unwrap();

        let entries_a = queue_a.get_for_target("target-1").unwrap();
        let entries_b = queue_b.get_for_target("target-1").unwrap();
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_b.len(), 1);
        assert_eq!(entries_a[0].event.id, event1.id);
        assert_eq!(entries_b[0].event.id, event2.id);
    }
}
