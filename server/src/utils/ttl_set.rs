use std::sync::Arc;

use dashmap::DashMap;
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

    pub fn insert(&self, key: String, ttl: tokio::time::Duration) {
        let expiration = tokio::time::Instant::now() + ttl;
        self.map.insert(key, expiration);
    }

    pub fn contains(&self, key: &str) -> bool {
        let now = time::Instant::now();

        if let Some(exp) = self.map.get(key) {
            let is_fresh = *exp > now;
            drop(exp);
            if !is_fresh {
                self.map.remove(key);
            }
            return is_fresh;
        }

        false
    }
}

impl Drop for TtlSet {
    fn drop(&mut self) {
        self.janitor.abort();
    }
}
