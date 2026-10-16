# AGENTS.md — Simple Sync Integration Playbook

You are integrating `simple-sync` into a single-player codebase to add zero-server, end-to-end encrypted P2P multiplayer ("Share Mode") with Edit and View-Only links and a kill switch. Follow the steps in order, then "Wiring Share Mode UI", then the checklist at the end. Do NOT introduce `async` into synchronous UI code.

**The target repo's own rules come first.** If it has an `AGENTS.md`, `CLAUDE.md` or CONTRIBUTING file, read it before you start and run its quality gates (formatter, linter, tests, translation/asset/licence checks) before you call the work done. Adding a toolbar control, a dependency or UI strings routinely breaks checks you did not touch (translation coverage, asset hashes, layering rules, tests that look for "the first text field"). Fix those; never skip them.

## What you are promising the user

Say this plainly in your summary. Do not describe it as "like Google Docs" unless it is.

- Each edit is sent when it happens, as a whole command ("delete this paragraph"), not keystrokes. Peers apply commands in the order they arrive.
- There is **no merge of simultaneous edits** and no live cursors. If two people change the same thing at once, the result can differ between peers. Index-based commands ("delete annotation 3") can land on the wrong item.
- Only the commands you map are shared (Step 2). Everything else stays on one machine. The user must be told which.
- Anyone with a link can read the whole document and keep a copy. An Edit link can also change it. See "Privacy and exposure".

## Step 1: Add Dependencies
Add `simple-sync`, `serde` (with `derive`) and `bincode = "1.3"` to the crate that owns the UI. `bincode` must be 1.x: `RemoteMutation::mutation_bytes` is the bincode-1 encoding of what peers broadcast.

Depend on `simple-sync` in a way that works for every contributor and CI: a git dependency, a published crate, or a copy vendored inside the repo (`path = "vendor/simple-sync"`, keep its licence files). **Never** point at an absolute path on your own machine, and never at a scratch directory. Keep `simple-sync` out of any WASM build (`cfg(not(target_arch = "wasm32"))`): it needs sockets and threads.

## Step 2: Locate the Document & Command Boundaries, and map EVERY command
Audit the target codebase and identify:
1. **Document load/save:** where raw file bytes (e.g. a `Vec<u8>` PDF) are loaded into memory.
2. **Mutation dispatch:** the single method where user edits are applied to the document.
3. **The document identity.** If the app can have several documents open (tabs, windows), record which one is shared. Remote edits must be applied to *that* document whatever is on screen, and edits the user makes to *other* documents must NOT be broadcast.

Then make a **complete inventory of the command type** (every variant of the edit enum, and any edit that bypasses it). Write a table in your summary: variant, shared or not, and why. Rules:

- Share by serializing the app's own command type when you can (`#[derive(Serialize, Deserialize)]` on it and the types inside it). If that means deriving serde across many crates, do the commands users reach for first (annotations and comments, text edits, page operations, form values) and list the rest as "not shared yet".
- A mirror enum that covers only a handful of commands is a silent failure: the user edits something, nothing reaches the other side, nothing says so. **Never drop an unmapped command silently.** When a command is applied locally during a session but not shared, tell the user (a toast or status line, translated like any other UI string).
- Do not share commands that carry secrets or code: passwords and permission changes, scripts, anything that can run on the receiving peer.
- Do not share commands that refer to something local to one peer's copy of the file (an object number of an image you just embedded). They cannot mean the same thing on the other side.
- Large payloads (an inserted image) are fine up to `MAX_MUTATION_BYTES` (about 1 MiB). Larger ones make `broadcast_mutation` return an error; surface it.
- Add a test that asserts, for each shared command, that it survives the round trip (`to_wire(from_wire(cmd)) == cmd`) so a new variant cannot be forgotten.

## Step 3: Prevent Infinite Echo Loops
Split the mutation application method into two paths:
- `apply_local_mutation(cmd)` -> applies the edit locally exactly as the user made it, AND (if this is the shared document) calls `self.sync.broadcast_mutation(&cmd)`. Apply the user's original command; do not rebuild it from the serialized form, or you lose detail (positions, styles) for the local user. Log or show the `Err` when it fails; offline and view-only are not errors worth showing.
- `apply_remote_mutation(cmd)` -> applies the edit to the shared document WITHOUT broadcasting.

View-only peers must refuse local edits before they happen (disable tools), and the dispatcher must refuse them too.

## Step 4: Wire the `SyncEngine` Polling Loop
1. Add `sync: simple_sync::SyncEngine` to the main application state (`SyncEngine::new("your-app")?`). Creating it only starts an idle thread; no socket is opened until the user shares or joins.
2. In the main frame tick, drain `while let Some(event) = self.sync.poll()`:
   - `SyncEvent::BaseDocumentReady { bytes, .. }`: open `bytes` as a new document and remember it as the shared one.
   - `SyncEvent::RemoteMutation { mutation_bytes, .. }`: `bincode::deserialize` into the command type, call `apply_remote_mutation`, request a repaint. A message that does not decode is an error to show, never a panic.
   - `PeerJoined/PeerLeft`: repaint. `Error(msg)`: show it in a status line.
3. **Do not call the blocking lifecycle methods on the UI thread.** `enable_share_mode`, `join_via_ticket` and `disable_share_mode` wait for the worker (the first two can take several seconds: they bind a socket and wait for a relay address). Move the engine to a worker thread for the call, hand it back over a channel, show a spinner meanwhile, and refuse local edits while it is starting (an edit made then is in neither the shared file nor the edit log). See `recipes/pdfcraft_hook.md`.

## Step 5: Add the Share Mode UI & Tests
1. Wire the toolbar (below).
2. Tests (headless, `SyncEngine::with_config(name, SyncConfig::local_only())` so no relay or DNS is needed; poll with a timeout; see `tests/common/mod.rs`, `tests/share_mode_test.rs`, `tests/robustness_test.rs`):
   - Host shares, joiner joins with the edit link, base document arrives, one edit on the host arrives and the documents match; and the joiner's edit reaches the host.
   - A second joiner with the view-only link has its local edit refused.
   - **Late joiner:** the host edits *before* anyone joins; a peer who joins afterwards ends up with the same document.
   - **Each shared command kind** produces the same result on both sides (not just one example).
   - An edit made in a tab other than the shared one is not sent; a remote edit lands on the shared document while another tab is showing.
   - Kill switch returns to `Offline`.
   Prove a new test can fail: break the thing it tests and watch it go red before you trust it.
3. Run the whole existing test suite, not only your new tests.

## Wiring Share Mode UI
Copy `recipes/share_mode_toolbar.rs` (framework-agnostic state helper + egui example) and bind these controls. The role comes from `self.sync.role()`:

| Role | Show |
|---|---|
| `Offline` | **[Share Document]** button and **[Join via Link]** text input |
| `Hosting` | `🟢 Live ({N} Peers)`, **[Copy Edit Link]**, **[Copy View-Only Link]**, red **[Stop Sharing]** |
| `JoinedCanEdit` | `🟢 Live ({N} Peers)` and **[Leave]** |
| `JoinedViewOnly` | `🔒 View Only` badge and **[Leave]** |

Bindings:
1. **[Share Document]** → save the document's current bytes (including unsaved edits) and call `enable_share_mode(&filename, bytes)` off the UI thread. Keep the returned `links` in UI state: they are returned once.
2. **[Copy Edit Link]** / **[Copy View-Only Link]** → clipboard. Say what each allows (see below).
3. **[Join via Link]** → `join_via_ticket(input.trim())` off the UI thread. The role follows the link. The document arrives later as `BaseDocumentReady`. Show errors in the status line.
4. **[Stop Sharing]** / **[Leave]** → `disable_share_mode()`; forget the shared document and the links. This wipes the session keys; a new share gets entirely new keys, so old links are dead.
5. **View-only enforcement:** when `role() == JoinedViewOnly`, disable every editing tool. `broadcast_mutation` refuses too, and view-only peers cannot sign edits, but the UI must not let the user make local edits that silently diverge.
6. The status pill reads `connected_peers_count()` each frame (non-blocking). Never call the lifecycle methods per frame.

## Privacy and exposure (tell the user, and don't weaken the defaults)
- A link is a password. It contains the read key; an Edit link also contains the write key. It also contains the host's IP addresses.
- Anyone with either link can read the whole document and keep a copy. Stopping sharing cuts off future access; it cannot un-share what a peer already has.
- While sharing or joining the app opens one UDP port on the machine and contacts n0's public relay and discovery servers (they see a random session id and IP addresses, not the document). Nothing is opened when not sharing.
- `SyncConfig::default()` does **not** ask the router to forward a port (UPnP / NAT-PMP / PCP). Set `port_mapping: true` only if the user wants it for better direct connectivity.
- All writers share one signing key: `author_peer_id` is for display, authenticated only as "an authorized writer". One editor can pose as another and cannot be revoked alone.

## Behaviour you can rely on
- **Late joiners:** the host keeps an ordered log of every mutation and replays it to each new peer after the base file. Peers that already have an edit drop the duplicate. If the log fills (50,000 edits or 16 MiB) the host reports a `SyncEvent::Error` and later joiners miss those edits.
- **Replays:** every frame carries a per-sender sequence number inside the signed bytes; receivers apply each `(author, seq)` once. A View-Only peer (who can read frames) cannot re-publish a captured edit to have it applied again.
- **Order:** frames from one sender arrive in order in normal operation, but gossip does not guarantee it. Design commands so that a reordered pair does not corrupt the document, or accept the limitation and say so.
- **Remote events on a joiner are held back until `BaseDocumentReady`**, so a mutation never arrives before the document is loaded.
- **Limits:** a gossip frame is at most `MAX_FRAME_BYTES` (1 MiB); `broadcast_mutation` refuses edits over `MAX_MUTATION_BYTES`. The base file is held in memory, so this targets documents that fit in RAM. Peers must run the same simple-sync version (the ticket version changes when the wire format does).
- A joiner that cannot reach the host reports why (timed out, refused) within 30 seconds, as a `SyncEvent::Error`.
- Optional shared metadata (title, cursor colors) can use `set_shared(key, value)`; merges surface as `SyncEvent::CrdtUpdated`.

## Final checklist (put the answers in your summary)
1. Files you changed.
2. The command table: which edits are shared, which are not, and why.
3. Gates run in the target repo and their results (include anything you could not run).
4. What the user was told in the UI when an edit is not shared.
5. Friction or gaps you found in this playbook.
