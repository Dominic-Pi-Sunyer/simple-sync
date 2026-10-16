# Architecture and security model

simple-sync has not been independently audited. This document describes what the code does and where its limits are.

## Architecture

```
 App (synchronous UI thread)              Worker thread (Tokio, 2 threads)
 ┌────────────────────┐   commands      ┌─────────────────────────────────────┐
 │ SyncEngine         │ ──channel────▶  │ iroh endpoint (QUIC, own random key)│
 │  enable / join /   │                 │  ├─ gossip topic  (edits, 1 MiB max)│
 │  disable           │ ◀──events────── │  └─ blobs (host serves the base file)│
 │  broadcast_mutation│                 │ Keys · ReplayGuard · edit log (host)│
 │  poll()  role()    │                 │ Loro CRDT (small shared metadata)   │
 └────────────────────┘                 └─────────────────────────────────────┘
```

- **Facade (`src/lib.rs`)**: a synchronous `SyncEngine`. The app calls `poll()` each frame and never touches async code. The lifecycle calls (`enable_share_mode`, `join_via_ticket`, `disable_share_mode`) wait for the worker, so apps should run them off their UI thread.
- **Worker (`src/worker.rs`)**: owns the network node and one session at a time. Every session gets a brand-new node identity.
- **Tickets (`src/ticket.rs`)**: the `ssync://` link carries the host's address, the document's BLAKE3 hash, the gossip topic id, the read key, the write verifying key and, for Edit links only, the write signing key.
- **Base file**: sent once over iroh-blobs and verified against the ticket's hash.
- **Edits**: small application commands over gossip. Each frame is `nonce || ChaCha20-Poly1305(read_key, SignedPayload)`, where the payload holds the author, a per-sender sequence number, the command bytes and an Ed25519 signature over `author || seq || payload` (`src/crypto.rs`).
- **Replay guard (`src/replay.rs`)**: each `(author, seq)` is applied once (a 65,536-entry window for each of up to 256 authors).
- **Host edit log**: the host keeps an ordered log of the session's mutations (up to 50,000 edits or 16 MiB) and replays it to each new peer after the base file.
- **CRDT (`src/state.rs`)**: only the small `set_shared` key-value metadata merges conflict-free (Loro). Application edits do not.

## Security model

There are no accounts and no servers. Access control is by capability:

| Holder | Can read | Can create valid edits |
|---|---|---|
| View-only link | yes (read key) | no (no signing key) |
| Edit link | yes | yes |
| Host | yes | yes, and serves the file |

What it protects:
- **Confidentiality**: relays and observers see only encrypted payloads. They do see metadata: node ids, IP addresses, timing and message sizes.
- **Forgery**: view-only peers cannot sign edits; unsigned or wrongly signed frames are dropped silently.
- **Replay**: a peer who holds a genuine captured frame (any peer can read frames) cannot make it apply twice.
- **Tampering**: edits are authenticated by the AEAD tag and the signature; the base file by BLAKE3.
- **Revocation**: `disable_share_mode` aborts tasks, wipes the keys and shuts the node down. Sharing again makes entirely new keys, so old links never work.
- **Hygiene**: keys are zeroized on drop, links are redacted in `Debug`, and lock poisoning is recovered instead of panicking.

## Trust boundaries

- Whoever holds a link is trusted with that link's rights. A link is a bearer password.
- Joiners trust the host: they open whatever file the link points to, so the file goes through the app's own parser.
- The network is untrusted: it can drop or delay messages and learn metadata, nothing else.

## Network exposure

Nothing is opened until a session is started or joined. Then one UDP port is bound and, with the default config, n0's public relay and DNS discovery are contacted. The router is **not** asked to forward a port unless `SyncConfig::port_mapping` is set. The ticket contains the host's IP addresses.

## Known limits

1. One signing key is shared by all editors: `author_peer_id` is display-only, an editor can pose as another, and one editor cannot be revoked without ending the session.
2. No forward secrecy: a link that leaks later exposes traffic captured earlier.
3. No conflict resolution or ordering guarantee beyond arrival order; index-based edits can diverge under simultaneous changes.
4. Metadata is visible to relays and to anyone you give a link to.
5. Soft resource limits: the event channel is unbounded, edits are capped at about 1 MiB, and the base file is held in memory.
6. The crypto composition is conventional but unreviewed by a third party.
7. `SyncEngine::__inject_raw_frame` is a public test hook that bypasses the role check.

## Suitable uses

Trusted collaborators and non-secret documents. Before treating it as secure for adversarial peers: per-editor keys, an independent crypto review and bounded channels.
