use std::sync::Arc;

use dashmap::{DashMap, Entry, OccupiedEntry, VacantEntry};
use tokio::time;

pub struct TtlSet {
    map: Arc<DashMap<String, tokio::time::Instant>>,
    janitor: tokio::task::JoinHandle<()>,
}

impl TtlSet {
    pub fn new() -> Self {
        let map = Arc::new(DashMap::new());
        let weak = Arc::downgrade(&map);
        let janitor = tokio::spawn(async move {
            let mut ticker = time::interval(time::Duration::from_secs(1));
            loop {
                ticker.tick().await;
                let Some(map) = weak.upgrade() else { break };
                let now = time::Instant::now();
                map.retain(|_, &mut expiration| expiration > now);
            }
        });

        TtlSet { map, janitor }
    }

    pub fn insert(&self, key: &str, ttl: tokio::time::Duration) -> bool {
        let now = time::Instant::now();

        let entry = self.map.entry(key.to_string());
        if let Entry::Occupied(entry) = &entry {
            let is_fresh = *entry.get() > now;
            if is_fresh {
                return false;
            }
        }

        entry.insert(now + ttl);
        true
    }
}

impl Drop for TtlSet {
    fn drop(&mut self) {
        self.janitor.abort();
    }
}
