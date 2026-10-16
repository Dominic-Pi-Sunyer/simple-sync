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

Notes:
- Undo/redo: route undo through `apply_local_mutation` with the inverse command so peers stay consistent.
- If an edit is not a pure function of prior edits (e.g. contains timestamps or random ids), generate those *before* broadcasting and include them in the command.
- After Stop Sharing the local document stays as-is; guests keep their copy of what they already received.
- Adapt `egui::Context` to your UI toolkit (or drop it).
