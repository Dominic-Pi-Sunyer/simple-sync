# Recipe: hooking an append-only document engine (PdfCraft-style)

Document engines whose edits are a log of commands (`EditAction`, annotations, page ops) map directly onto `simple-sync`: the base PDF travels once as a blob, every subsequent edit is a small signed, encrypted mutation. For the toolbar itself see `share_mode_toolbar.rs`.

```rust
use serde::{Deserialize, Serialize};
use simple_sync::{ShareRole, SyncEvent};

#[derive(Clone, Serialize, Deserialize)]
pub enum EditAction { /* existing variants */ }

impl App {
    /// User-initiated edit: apply, then broadcast (when sharing).
    pub fn apply_local_mutation(&mut self, cmd: EditAction) {
        if self.sync.role() == ShareRole::JoinedViewOnly {
            return; // editing tools should already be disabled in the UI
        }
        self.apply(&cmd);                              // existing logic
        if self.sync.role() != ShareRole::Offline {
            if let Err(e) = self.sync.broadcast_mutation(&cmd) {
                self.status = format!("sync: {e}");
            }
        }
    }

    /// Edit received from a peer: apply only. NEVER broadcast here (echo loop).
    pub fn apply_remote_mutation(&mut self, cmd: EditAction) {
        self.apply(&cmd);
    }

    /// Call once per frame from the update loop.
    pub fn pump_sync(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.sync.poll() {
            match ev {
                SyncEvent::BaseDocumentReady { filename, bytes } => {
                    self.load_document(&filename, bytes);          // existing loader
                }
                SyncEvent::RemoteMutation { mutation_bytes, .. } => {
                    match bincode::deserialize::<EditAction>(&mutation_bytes) {
                        Ok(cmd) => self.apply_remote_mutation(cmd),
                        Err(e) => self.status = format!("bad remote edit: {e}"),
                    }
                }
                SyncEvent::Error(msg) => self.status = msg,
                SyncEvent::PeerJoined { .. } | SyncEvent::PeerLeft { .. } | SyncEvent::CrdtUpdated { .. } => {}
            }
            ctx.request_repaint();
        }
    }
}
```

## Starting a session without freezing the window
`enable_share_mode` and `join_via_ticket` block until the socket is bound and a relay address is known (seconds). Hand the engine to a thread and take it back:

```rust
fn begin_share(&mut self, filename: String, bytes: Vec<u8>) {
    let Some(mut engine) = self.sync.take() else { return };   // `sync: Option<SyncEngine>`
    self.shared_doc = self.active_doc_id();                    // the document being shared
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = engine.enable_share_mode(&filename, bytes).map_err(|e| e.to_string());
        let _ = tx.send((engine, result));
    });
    self.share_pending = Some(rx);   // each frame: try_recv, put the engine back, keep the links
}
```
While `share_pending` is `Some`: show a spinner, and refuse local edits (an edit now is in neither the shared file nor the host's edit log). `SyncEngine` is `Send`.

## Which document, and what is not shared
- Remember the shared document's id when sharing starts (host) and when `BaseDocumentReady` opens it (guest). `apply_remote_mutation` applies to that document even if another tab is showing; `apply_local_mutation` broadcasts only when the edit's document is the shared one.
- Map every command variant (see AGENTS.md, Step 2). For a command you do not share, show a notice ("This change isn't shared live yet") instead of dropping it silently.
- Apply the user's original command locally and send the serialized form to peers; do not round-trip the local edit through the wire form (positions and styles can be lost).

Notes:
- Undo/redo: route undo through `apply_local_mutation` with the inverse command so peers stay consistent.
- If an edit is not a pure function of prior edits (e.g. contains timestamps or random ids), generate those *before* broadcasting and include them in the command.
- After Stop Sharing the local document stays as-is; guests keep their copy of what they already received.
- Adapt `egui::Context` to your UI toolkit (or drop it).
