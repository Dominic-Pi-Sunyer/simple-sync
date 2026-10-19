//! Conflict-free replicated key/value map (`"shared"`) for delta and snapshot merging.
//!
//! Gives apps a tiny shared key/value map (`"shared"`) whose concurrent edits
//! merge deterministically on every peer, independent of delivery order.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::lock;

const ROOT: &str = "shared";

#[derive(Clone, Default, Serialize, Deserialize)]
struct MapDelta {
    entries: BTreeMap<String, (u64, String)>,
}

pub struct CrdtState {
    map: Mutex<MapDelta>,
}

impl Default for CrdtState {
    fn default() -> Self {
        Self::new()
    }
}

impl CrdtState {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(MapDelta::default()),
        }
    }

    /// Set `key = value` in the shared map; returns the delta to broadcast.
    pub fn set(&self, key: &str, value: &str) -> Result<Vec<u8>> {
        let mut guard = lock(&self.map);
        let next_ver = guard
            .entries
            .get(key)
            .map_or(1, |(v, _)| v.saturating_add(1));
        let entry = (next_ver, value.to_string());
        guard.entries.insert(key.to_string(), entry.clone());
        let mut delta = MapDelta::default();
        delta.entries.insert(key.to_string(), entry);
        bincode::serialize(&delta).map_err(|e| anyhow!("crdt export: {e}"))
    }

    /// Merge a remote delta or snapshot. Idempotent and order-independent.
    pub fn apply(&self, bytes: &[u8]) -> Result<()> {
        let incoming: MapDelta =
            bincode::deserialize(bytes).map_err(|e| anyhow!("crdt import: {e}"))?;
        let mut guard = lock(&self.map);
        for (k, v) in incoming.entries {
            if guard.entries.get(&k).is_none_or(|cur| cur < &v) {
                guard.entries.insert(k, v);
            }
        }
        Ok(())
    }

    /// All known operations, for bringing a late joiner up to date.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        let guard = lock(&self.map);
        bincode::serialize(&*guard).map_err(|e| anyhow!("crdt export: {e}"))
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.map).entries.is_empty()
    }

    /// Whole document as JSON, e.g. `{"shared":{"title":"Hi"}}`.
    pub fn json(&self) -> String {
        let guard = lock(&self.map);
        let shared: BTreeMap<&str, &str> = guard
            .entries
            .iter()
            .map(|(k, (_, v))| (k.as_str(), v.as_str()))
            .collect();
        let mut root = BTreeMap::new();
        root.insert(ROOT, shared);
        serde_json::to_string(&root).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_converge_in_any_order() {
        let a = CrdtState::new();
        let b = CrdtState::new();
        let d1 = a.set("title", "A").unwrap();
        let d2 = b.set("author", "B").unwrap();
        a.apply(&d2).unwrap();
        b.apply(&d1).unwrap();
        b.apply(&d1).unwrap(); // idempotent
        assert_eq!(a.json(), b.json());
        assert!(a.json().contains("title") && a.json().contains("author"));
    }

    #[test]
    fn concurrent_writes_to_same_key_converge() {
        let a = CrdtState::new();
        let b = CrdtState::new();
        let d1 = a.set("k", "from-a").unwrap();
        let d2 = b.set("k", "from-b").unwrap();
        a.apply(&d2).unwrap();
        b.apply(&d1).unwrap();
        assert_eq!(a.json(), b.json());
    }

    #[test]
    fn snapshot_catches_up_late_joiner() {
        let a = CrdtState::new();
        a.set("x", "1").unwrap();
        a.set("y", "2").unwrap();
        let late = CrdtState::new();
        assert!(late.is_empty());
        late.apply(&a.snapshot().unwrap()).unwrap();
        assert_eq!(a.json(), late.json());
    }
}
