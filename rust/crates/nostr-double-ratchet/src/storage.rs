use crate::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub trait StorageAdapter: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn put(&self, key: &str, value: String) -> Result<()>;
    fn del(&self, key: &str) -> Result<()>;
    fn list(&self, prefix: &str) -> Result<Vec<String>>;
}

#[derive(Clone)]
pub struct InMemoryStorage {
    store: Arc<Mutex<HashMap<String, String>>>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageAdapter for InMemoryStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.store.lock().unwrap().get(key).cloned())
    }

    fn put(&self, key: &str, value: String) -> Result<()> {
        self.store.lock().unwrap().insert(key.to_string(), value);
        Ok(())
    }

    fn del(&self, key: &str) -> Result<()> {
        self.store.lock().unwrap().remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}
