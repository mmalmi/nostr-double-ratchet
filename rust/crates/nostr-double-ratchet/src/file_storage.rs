use crate::{Result, StorageAdapter};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct FileStorageAdapter {
    base_path: PathBuf,
}

impl FileStorageAdapter {
    pub fn new(base_path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_path)
            .map_err(|e| crate::Error::Storage(format!("Failed to create directory: {}", e)))?;
        Ok(Self { base_path })
    }

    fn key_to_path(&self, key: &str) -> PathBuf {
        let sanitized = key.replace(['/', '\\', ':'], "_");
        self.base_path.join(format!("{}.json", sanitized))
    }
}

impl StorageAdapter for FileStorageAdapter {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let path = self.key_to_path(key);

        if !path.exists() {
            return Ok(None);
        }

        match fs::read_to_string(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::Error::Storage(format!("Failed to read file: {}", e))),
        }
    }

    fn put(&self, key: &str, value: String) -> Result<()> {
        let path = self.key_to_path(key);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| crate::Error::Storage(format!("Failed to create parent dir: {}", e)))?;
        }

        fs::write(&path, value)
            .map_err(|e| crate::Error::Storage(format!("Failed to write file: {}", e)))?;

        Ok(())
    }

    fn del(&self, key: &str) -> Result<()> {
        let path = self.key_to_path(key);

        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::Error::Storage(format!("Failed to delete file: {}", e))),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();

        let entries = fs::read_dir(&self.base_path)
            .map_err(|e| crate::Error::Storage(format!("Failed to read directory: {}", e)))?;

        for entry in entries {
            let entry = entry
                .map_err(|e| crate::Error::Storage(format!("Failed to read dir entry: {}", e)))?;

            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();

            if !file_name_str.ends_with(".json") {
                continue;
            }

            let key = file_name_str
                .strip_suffix(".json")
                .unwrap_or(&file_name_str)
                .to_string();

            if key.starts_with(prefix) || prefix.is_empty() {
                keys.push(key);
            }
        }

        Ok(keys)
    }
}

pub struct DebouncedFileStorage {
    adapter: FileStorageAdapter,
    pending_writes: Mutex<HashMap<String, String>>,
    last_flush: Mutex<std::time::Instant>,
    flush_interval: std::time::Duration,
}

impl DebouncedFileStorage {
    pub fn new(base_path: PathBuf, flush_interval_ms: u64) -> Result<Self> {
        Ok(Self {
            adapter: FileStorageAdapter::new(base_path)?,
            pending_writes: Mutex::new(HashMap::new()),
            last_flush: Mutex::new(std::time::Instant::now()),
            flush_interval: std::time::Duration::from_millis(flush_interval_ms),
        })
    }

    pub fn flush(&self) -> Result<()> {
        let mut pending = self.pending_writes.lock().unwrap();
        for (key, value) in pending.drain() {
            self.adapter.put(&key, value)?;
        }
        *self.last_flush.lock().unwrap() = std::time::Instant::now();
        Ok(())
    }

    fn maybe_flush(&self) -> Result<()> {
        let last = *self.last_flush.lock().unwrap();
        let pending_count = self.pending_writes.lock().unwrap().len();

        if last.elapsed() >= self.flush_interval && pending_count > 0 {
            self.flush()?;
        }
        Ok(())
    }
}

impl StorageAdapter for DebouncedFileStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let pending = self.pending_writes.lock().unwrap();
        if let Some(value) = pending.get(key) {
            return Ok(Some(value.clone()));
        }
        drop(pending);
        self.adapter.get(key)
    }

    fn put(&self, key: &str, value: String) -> Result<()> {
        self.pending_writes.lock().unwrap().insert(key.to_string(), value);
        self.maybe_flush()
    }

    fn del(&self, key: &str) -> Result<()> {
        self.pending_writes.lock().unwrap().remove(key);
        self.adapter.del(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = self.adapter.list(prefix)?;
        let pending = self.pending_writes.lock().unwrap();

        for key in pending.keys() {
            if key.starts_with(prefix) && !keys.contains(key) {
                keys.push(key.clone());
            }
        }

        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredUserRecord;
    use tempfile::TempDir;

    #[test]
    fn test_file_storage_adapter_basic() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = FileStorageAdapter::new(temp_dir.path().to_path_buf()).unwrap();

        assert!(adapter.get("test-key").unwrap().is_none());

        adapter.put("test-key", "test-value".to_string()).unwrap();
        assert_eq!(adapter.get("test-key").unwrap(), Some("test-value".to_string()));

        adapter.del("test-key").unwrap();
        assert!(adapter.get("test-key").unwrap().is_none());
    }

    #[test]
    fn test_file_storage_adapter_list() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = FileStorageAdapter::new(temp_dir.path().to_path_buf()).unwrap();

        adapter.put("user_alice", "data1".to_string()).unwrap();
        adapter.put("user_bob", "data2".to_string()).unwrap();
        adapter.put("invite_charlie", "data3".to_string()).unwrap();

        let user_keys = adapter.list("user_").unwrap();
        assert_eq!(user_keys.len(), 2);
        assert!(user_keys.contains(&"user_alice".to_string()));
        assert!(user_keys.contains(&"user_bob".to_string()));

        let all_keys = adapter.list("").unwrap();
        assert_eq!(all_keys.len(), 3);
    }

    #[test]
    fn test_file_storage_adapter_json() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = FileStorageAdapter::new(temp_dir.path().to_path_buf()).unwrap();

        let user_record = StoredUserRecord {
            user_id: "test-user".to_string(),
            devices: vec![],
        };

        let json = serde_json::to_string(&user_record).unwrap();
        adapter.put("user/test", json.clone()).unwrap();

        let retrieved = adapter.get("user/test").unwrap().unwrap();
        let parsed: StoredUserRecord = serde_json::from_str(&retrieved).unwrap();

        assert_eq!(parsed.user_id, "test-user");
    }

    #[test]
    fn test_debounced_storage() {
        let temp_dir = TempDir::new().unwrap();
        let storage = DebouncedFileStorage::new(
            temp_dir.path().to_path_buf(),
            1000,
        ).unwrap();

        storage.put("key1", "value1".to_string()).unwrap();

        assert_eq!(storage.get("key1").unwrap(), Some("value1".to_string()));

        assert!(storage.pending_writes.lock().unwrap().contains_key("key1"));

        storage.flush().unwrap();

        assert!(storage.pending_writes.lock().unwrap().is_empty());
        assert_eq!(storage.adapter.get("key1").unwrap(), Some("value1".to_string()));
    }
}
