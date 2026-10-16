# simple-sync

Zero-server, end-to-end encrypted P2P document sync for apps, with one-call **Share Mode**: Edit links, View-Only links and a kill switch. Built on [iroh](https://iroh.computer) (QUIC, blobs, gossip) and [Loro](https://loro.dev) CRDTs.

- **Synchronous facade.** `SyncEngine` runs its own Tokio runtime on a background thread; your 60fps render loop just calls `poll()`.
- **Capability tickets.** `ssync://…` links carry everything: host address, BLAKE3 document hash, gossip topic, read key and write verifying key. The Edit link additionally carries the write signing key. No accounts, no permission server.
- **Cryptographic read/write split.** Every gossip frame is ChaCha20-Poly1305 encrypted (read key) and Ed25519 signed (write key). View-Only peers can read but cannot produce frames anyone accepts; forged or undecryptable frames are dropped silently.
- **Verified base file.** The initial file streams over `iroh-blobs` and is verified against the ticket's BLAKE3 hash.
- **Kill switch.** `disable_share_mode()` wipes the session keys and tears down the node. Re-enabling creates new keys, topic and node identity, so old links never reconnect.

```rust
use simple_sync::{SyncEngine, SyncEvent, ShareRole};

let mut sync = SyncEngine::new("my-app")?;

// Host: Share Mode ON
let links = sync.enable_share_mode("doc.pdf", pdf_bytes)?;
// links.edit_uri / links.view_only_uri -> clipboard

// Guest (other machine)
let role = sync.join_via_ticket(&link)?;   // JoinedCanEdit | JoinedViewOnly

// Every frame
while let Some(ev) = sync.poll() {
    match ev {
        SyncEvent::BaseDocumentReady { bytes, .. } => { /* load bytes */ }
        SyncEvent::RemoteMutation { mutation_bytes, .. } => {
            let cmd: Command = bincode::deserialize(&mutation_bytes)?; /* apply, no re-broadcast */
        }
        _ => {}
    }
}
let pill = (sync.role(), sync.connected_peers_count());

// Local edit (Err("Session is View-Only") for view-only joiners)
sync.broadcast_mutation(&cmd)?;

// Kill switch
sync.disable_share_mode()?;
```

## Retrofitting an existing app
Point your coding agent at [`AGENTS.md`](AGENTS.md) (includes a "Wiring Share Mode UI" section). Snippets live in [`recipes/`](recipes): `share_mode_toolbar.rs` (UI state helper + egui example) and `pdfcraft_hook.md`.

## Layout
| Path | Purpose |
|---|---|
| `src/lib.rs` | Public synchronous facade (`SyncEngine`, `SyncEvent`, `ShareRole`, `ShareLinks`) |
| `src/ticket.rs` | `SessionTicket`, `SharePermission`, `ssync://` encode/decode |
| `src/crypto.rs` | `SignedEncryptedFrame`, seal/open, write verification |
| `src/state.rs` | Loro CRDT wrapper |
| `src/worker.rs` | Background iroh runtime and session lifecycle |
| `tests/` | Loopback sync, Share Mode (edit/view-only/kill switch) |

## Development
```sh
cargo test    # unit tests + loopback tests over 127.0.0.1 (no network needed)
```
`SyncConfig::local_only()` disables relays and discovery for tests; the default config uses n0's public relays and DNS discovery.

## Notes
- Versions: iroh 1.3, iroh-gossip 0.101, iroh-blobs 0.103, loro 1.16, ed25519-dalek 3.
- Lifecycle calls (`enable_share_mode`, `disable_share_mode`, `join_via_ticket`) wait briefly for the worker; everything else is non-blocking.
- All writers share one signing key, so `author_peer_id` is authenticated only as "an authorized writer".
- Revocation is forward-looking: it cuts off the network session and invalidates the links, but cannot un-share data a peer already received.
- The base file is held in memory, so this targets documents that fit in RAM.

License: MIT OR Apache-2.0
