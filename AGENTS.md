# AGENTS.md — Simple Sync Integration Playbook

You are integrating `simple-sync` into a single-player codebase to add zero-server, end-to-end encrypted P2P multiplayer ("Share Mode") with Edit and View-Only links and a kill switch. Follow these exact 5 steps in order, then the "Wiring Share Mode UI" section. Do NOT introduce `async` into synchronous UI code.

## Step 1: Add Dependencies
Add `simple-sync`, `serde` (with `derive`), and `bincode = "1.3"` to the workspace/crate `Cargo.toml`.
(`bincode` must be 1.x: `RemoteMutation::mutation_bytes` is the bincode-1 encoding of what peers broadcast.)

## Step 2: Locate the Document & Command Boundaries
Audit the target codebase and identify two things:
1. **Document Load/Save:** Where raw file bytes (e.g., `Vec<u8>` of a PDF) are loaded into memory.
2. **Mutation Dispatch:** The central enum or method where user edits (annotations, text inserts, page rotations, undoable commands) are applied to the document state.

Make the command type `#[derive(Serialize, Deserialize)]`.

## Step 3: Prevent Infinite Echo Loops
Split the mutation application method into two paths:
- `apply_local_mutation(&mut self, cmd: Command)` -> Applies the edit locally AND calls `self.sync.broadcast_mutation(&cmd)` (ignore/log the `Err` when offline or view-only; the local edit still applies when offline).
- `apply_remote_mutation(&mut self, cmd: Command)` -> Applies the edit locally WITHOUT broadcasting.

## Step 4: Wire the `SyncEngine` Polling Loop
1. Add `sync: simple_sync::SyncEngine` to the main application state struct (`SyncEngine::new("your-app")?`).
2. In the main frame tick / update loop, drain `while let Some(event) = self.sync.poll()`:
   - On `SyncEvent::BaseDocumentReady { bytes, .. }`: Load `bytes` into the app's document parser as the active file.
   - On `SyncEvent::RemoteMutation { mutation_bytes, .. }`: `bincode::deserialize` into the app's `Command` enum, call `apply_remote_mutation`, and request a UI repaint.
   - On `SyncEvent::PeerJoined/PeerLeft`: request a repaint (the badge reads `sync.connected_peers_count()`). On `SyncEvent::Error(msg)`: show it in a status line.

## Step 5: Add the Share Mode UI & Verification Test
1. Wire the toolbar as described in **Wiring Share Mode UI** below.
2. Write an automated headless test in `tests/p2p_sync_test.rs` that initializes two app instances (`app_a` and `app_b`), enables Share Mode on `app_a` with a dummy document, joins from `app_b` via `links.edit_uri`, dispatches one edit on `app_a`, polls `app_b`, and asserts their document states match. Add a second joiner using `links.view_only_uri` and assert its local edit attempt is refused. Run `cargo test` until green.
   Use `SyncEngine::with_config(name, SyncConfig::local_only())` in tests so no relay/DNS is needed; poll in a loop with a timeout (see `tests/common/mod.rs` and `tests/share_mode_test.rs` in simple-sync for working templates).

## Wiring Share Mode UI
Copy `recipes/share_mode_toolbar.rs` (framework-agnostic state helper + egui example) into the target app and bind these 4 controls. The role comes from `self.sync.role()`; render a different row per role:

| Role | Show |
|---|---|
| `Offline` | **[Share Document]** button and **[Join via Link]** text input |
| `Hosting` | `🟢 Live ({N} Peers)`, **[Copy Edit Link]**, **[Copy View-Only Link]**, red **[Stop Sharing]** |
| `JoinedCanEdit` | `🟢 Live ({N} Peers)` and **[Leave]** |
| `JoinedViewOnly` | `🔒 View Only` badge and **[Leave]** |

Bindings:
1. **[Share Document]** → `let links = self.sync.enable_share_mode(&filename, current_file_bytes.clone())?;` and keep `links` in UI state (it is only returned once). Role becomes `Hosting`.
2. **[Copy Edit Link]** / **[Copy View-Only Link]** → copy `links.edit_uri` / `links.view_only_uri` to the clipboard. Anyone holding the edit link can edit; the view-only link can only read.
3. **[Join via Link]** → `self.sync.join_via_ticket(input.trim())?` (returns `JoinedCanEdit` or `JoinedViewOnly` depending on the link). Show the returned `Err` text in the status line. The document arrives via `SyncEvent::BaseDocumentReady`.
4. **[Stop Sharing]** (the kill switch; also **[Leave]** for joiners) → `self.sync.disable_share_mode()?; links = None;`. This wipes the session keys and tears down the network node. Sharing again generates brand-new keys, so previously handed-out links are dead for good.
5. **View-only enforcement in the host app:** when `role() == ShareRole::JoinedViewOnly`, disable every editing tool (menus, shortcuts, drag handles) — use `local_editing_allowed(&self.sync)` from the recipe. `broadcast_mutation` also refuses with `Session is View-Only`, and view-only peers cannot forge edits cryptographically, but the UI should not let the user make edits that will silently diverge from everyone else.
6. Status pill: read `self.sync.connected_peers_count()` each frame (cheap, non-blocking). Never call the blocking lifecycle methods (`enable_share_mode`, `disable_share_mode`, `join_via_ticket`) per frame, only from click handlers.

## Gotchas
- Lifecycle methods block briefly (socket bind / teardown). Call them from a button handler, never inside an async runtime. `broadcast_mutation`, `set_shared`, `poll`, `role` and `connected_peers_count` never block.
- Links contain the session read key: treat them like passwords. Edit links also contain the write key.
- Remote events on a joiner are held back until `BaseDocumentReady` fires, so a remote mutation never arrives before the document is loaded.
- All writers share one signing key: `author_peer_id` identifies a peer for display, but is only authenticated as "an authorized writer".
- Optional shared metadata (title, cursor colors, ...) can use `set_shared(key, value)`; merges surface as `SyncEvent::CrdtUpdated`.
