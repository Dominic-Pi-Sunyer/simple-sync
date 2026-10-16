#![allow(dead_code)] // each test crate uses a different subset

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use simple_sync::{SyncConfig, SyncEngine, SyncEvent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EditAction {
    Annotate { page: u32, text: String },
    RotatePage { page: u32, degrees: i32 },
}

pub struct Peer {
    pub engine: SyncEngine,
    pub backlog: Vec<SyncEvent>,
}

impl Peer {
    pub fn new() -> Self {
        Self {
            engine: SyncEngine::with_config("test", SyncConfig::local_only()).unwrap(),
            backlog: Vec::new(),
        }
    }

    fn pump(&mut self) {
        while let Some(e) = self.engine.poll() {
            self.backlog.push(e);
        }
    }

    /// Wait for the first not-yet-consumed event matching `pred`; others stay queued.
    pub fn wait(&mut self, what: &str, pred: impl Fn(&SyncEvent) -> bool) -> SyncEvent {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.pump();
            if let Some(i) = self.backlog.iter().position(|e| pred(e)) {
                return self.backlog.remove(i);
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; backlog: {:?}", self.backlog);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_base(&mut self) -> (String, Vec<u8>) {
        match self.wait("BaseDocumentReady", |e| matches!(e, SyncEvent::BaseDocumentReady { .. })) {
            SyncEvent::BaseDocumentReady { filename, bytes } => (filename, bytes),
            _ => unreachable!(),
        }
    }

    pub fn wait_mutation(&mut self, what: &str) -> (String, Vec<u8>) {
        match self.wait(what, |e| matches!(e, SyncEvent::RemoteMutation { .. })) {
            SyncEvent::RemoteMutation { author_peer_id, mutation_bytes } => (author_peer_id, mutation_bytes),
            _ => unreachable!(),
        }
    }

    pub fn wait_peers(&self, n: usize) {
        wait_until(&format!("{n} connected peer(s)"), || self.engine.connected_peers_count() == n);
    }

    /// Collect events for `dur`, then report every RemoteMutation seen (consumed).
    pub fn mutations_within(&mut self, dur: Duration) -> Vec<Vec<u8>> {
        std::thread::sleep(dur);
        self.pump();
        let mut out = Vec::new();
        self.backlog.retain(|e| match e {
            SyncEvent::RemoteMutation { mutation_bytes, .. } => {
                out.push(mutation_bytes.clone());
                false
            }
            _ => true,
        });
        out
    }

    pub fn drain(&mut self) {
        self.pump();
        self.backlog.clear();
    }

    /// Has any not-yet-consumed event matching `pred` arrived?
    pub fn seen(&mut self, pred: impl Fn(&SyncEvent) -> bool) -> bool {
        self.pump();
        self.backlog.iter().any(|e| pred(e))
    }
}

pub fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn fake_pdf() -> Vec<u8> {
    // ~600 KiB of deterministic, non-trivial bytes so blob streaming spans many chunks.
    let mut v = b"%PDF-1.7\n".to_vec();
    let mut x: u32 = 0x1234_5678;
    for _ in 0..600 * 1024 {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        v.push((x >> 24) as u8);
    }
    v.extend_from_slice(b"\n%%EOF\n");
    v
}
