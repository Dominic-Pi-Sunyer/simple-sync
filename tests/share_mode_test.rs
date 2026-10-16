//! Share Mode: capability tickets, edit vs view-only enforcement, kill switch.

mod common;

use std::time::Duration;

use common::{fake_pdf, wait_until, EditAction, Peer};
use ed25519_dalek::SigningKey;
use simple_sync::crypto::{self, SignedPayload};
use simple_sync::{SessionTicket, SharePermission, ShareRole, SyncEvent, Wire};

fn edit(text: &str) -> EditAction {
    EditAction::Annotate { page: 1, text: text.into() }
}

fn decode(bytes: &[u8]) -> EditAction {
    bincode::deserialize(bytes).unwrap()
}

#[test]
fn ticket_roundtrip() {
    let mut host = Peer::new();
    let pdf = fake_pdf();
    let links = host.engine.enable_share_mode("contract.pdf", pdf.clone()).unwrap();
    assert_eq!(host.engine.role(), ShareRole::Hosting);

    let edit_t = SessionTicket::from_uri(&links.edit_uri).unwrap();
    let view_t = SessionTicket::from_uri(&links.view_only_uri).unwrap();

    assert_eq!(edit_t.permission(), SharePermission::ReadWrite);
    assert_eq!(view_t.permission(), SharePermission::ReadOnly);
    assert!(edit_t.write_signing_key.is_some());
    assert!(view_t.write_signing_key.is_none());

    // Same session, differing only in the signing capability.
    assert_eq!(edit_t.host_addr, view_t.host_addr);
    assert_eq!(edit_t.gossip_topic_id, view_t.gossip_topic_id);
    assert_eq!(edit_t.read_key, view_t.read_key);
    assert_eq!(edit_t.write_verifying_key, view_t.write_verifying_key);
    assert_eq!(edit_t.doc_hash, *blake3::hash(&pdf).as_bytes());
    assert_eq!(edit_t.filename, "contract.pdf");
    assert!(edit_t.host_addr.ip_addrs().next().is_some(), "host must be dialable");

    // Re-serializing yields identical links.
    assert_eq!(edit_t.to_edit_uri(), links.edit_uri);
    assert_eq!(edit_t.to_view_only_uri(), links.view_only_uri);
    assert_eq!(view_t.to_view_only_uri(), links.view_only_uri);
}

#[test]
fn edit_vs_view_only_enforcement() {
    let mut host = Peer::new();
    let mut editor = Peer::new();
    let mut viewer = Peer::new();
    let pdf = fake_pdf();

    let links = host.engine.enable_share_mode("contract.pdf", pdf.clone()).unwrap();
    assert_eq!(editor.engine.join_via_ticket(&links.edit_uri).unwrap(), ShareRole::JoinedCanEdit);
    assert_eq!(viewer.engine.join_via_ticket(&links.view_only_uri).unwrap(), ShareRole::JoinedViewOnly);
    assert_eq!(editor.engine.role(), ShareRole::JoinedCanEdit);
    assert_eq!(viewer.engine.role(), ShareRole::JoinedViewOnly);

    // Both receive the verified base PDF.
    for p in [&mut editor, &mut viewer] {
        let (name, bytes) = p.wait_base();
        assert_eq!(name, "contract.pdf");
        assert_eq!(bytes, pdf);
    }
    host.wait_peers(2);

    // Editor -> host and viewer.
    let editor_id = editor.engine.local_peer_id();
    assert!(!editor_id.is_empty());
    let m1 = edit("from editor");
    editor.engine.broadcast_mutation(&m1).unwrap();
    for (name, p) in [("host", &mut host), ("viewer", &mut viewer)] {
        let (author, bytes) = p.wait_mutation(&format!("editor mutation at {name}"));
        assert_eq!(author, editor_id);
        assert_eq!(decode(&bytes), m1);
    }

    // Host can write too, and the editor sees it.
    let m_host = edit("from host");
    host.engine.broadcast_mutation(&m_host).unwrap();
    assert_eq!(decode(&editor.wait_mutation("host mutation at editor").1), m_host);
    viewer.wait_mutation("host mutation at viewer");

    // Viewer is blocked locally, before the network...
    let err = viewer.engine.broadcast_mutation(&edit("nope")).unwrap_err();
    assert!(err.to_string().contains("View-Only"), "{err}");
    assert!(viewer.engine.set_shared("k", "v").is_err());

    // ...and frames it forges are rejected by crypto.rs.
    let t = SessionTicket::from_uri(&links.view_only_uri).unwrap();
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&t.write_verifying_key).unwrap();
    let evil_payload = bincode::serialize(&Wire::Mutation(bincode::serialize(&edit("FORGED")).unwrap())).unwrap();
    let viewer_author = [3u8; 32];

    let unsigned = SignedPayload {
        author: viewer_author,
        payload: evil_payload.clone(),
        signature: vec![0u8; 64],
    };
    let unsigned_frame = crypto::encrypt(&t.read_key, &bincode::serialize(&unsigned).unwrap()).unwrap();
    let self_signed = {
        let own = SigningKey::from_bytes(&crypto::random_key());
        crypto::seal(&t.read_key, Some(&own), viewer_author, &evil_payload).unwrap()
    };
    assert!(crypto::seal(&t.read_key, None, viewer_author, &evil_payload).is_err());
    for frame in [&unsigned_frame, &self_signed] {
        assert!(crypto::open(&t.read_key, &vk, frame).is_err());
    }

    // Same forged frames pushed onto the real gossip topic never surface at peers.
    host.drain();
    editor.drain();
    viewer.engine.__inject_raw_frame(unsigned_frame);
    viewer.engine.__inject_raw_frame(self_signed);
    viewer.engine.__inject_raw_frame(b"garbage that is not even a frame".to_vec());
    let m2 = edit("legit after forgery");
    editor.engine.broadcast_mutation(&m2).unwrap();
    assert_eq!(decode(&host.wait_mutation("legit mutation").1), m2);
    let extra_host = host.mutations_within(Duration::from_millis(800));
    let extra_editor = editor.mutations_within(Duration::from_millis(100));
    assert!(extra_host.is_empty() && extra_editor.is_empty(), "forged frame was delivered");

    // No error events either: forged frames are dropped silently.
    for p in [&mut host, &mut editor] {
        assert!(!p.seen(|e| matches!(e, SyncEvent::Error(_))), "forged frames must be dropped silently");
    }
}

#[test]
fn kill_switch_revokes_old_links() {
    let mut host = Peer::new();
    let mut editor = Peer::new();
    let pdf = fake_pdf();

    let links1 = host.engine.enable_share_mode("contract.pdf", pdf.clone()).unwrap();
    editor.engine.join_via_ticket(&links1.edit_uri).unwrap();
    assert_eq!(editor.wait_base().1, pdf);
    host.wait_peers(1);

    // Sanity: live before the kill switch.
    editor.engine.broadcast_mutation(&edit("before")).unwrap();
    host.wait_mutation("pre-kill mutation");

    // --- Kill switch ---
    let old_peer_id = host.engine.local_peer_id();
    host.engine.disable_share_mode().unwrap();
    assert_eq!(host.engine.role(), ShareRole::Offline);
    assert_eq!(host.engine.connected_peers_count(), 0);
    assert_eq!(host.engine.local_peer_id(), "");
    assert!(host.engine.broadcast_mutation(&edit("offline")).is_err());
    // Idempotent.
    host.engine.disable_share_mode().unwrap();

    // The old subscriber notices the host is gone.
    wait_until("editor to lose its only neighbor", || editor.engine.connected_peers_count() == 0);

    // --- Re-enable: entirely new keys, topic and node identity ---
    let links2 = host.engine.enable_share_mode("contract.pdf", pdf.clone()).unwrap();
    assert_ne!(links2.edit_uri, links1.edit_uri);
    assert_ne!(links2.view_only_uri, links1.view_only_uri);
    let (t1, t2) = (
        SessionTicket::from_uri(&links1.edit_uri).unwrap(),
        SessionTicket::from_uri(&links2.edit_uri).unwrap(),
    );
    assert_ne!(t1.read_key, t2.read_key);
    assert_ne!(t1.gossip_topic_id, t2.gossip_topic_id);
    assert_ne!(t1.write_verifying_key, t2.write_verifying_key);
    assert_ne!(t1.write_signing_key, t2.write_signing_key);
    assert_ne!(t1.host_addr.id, t2.host_addr.id);
    assert_ne!(host.engine.local_peer_id(), old_peer_id);

    // The old editor keeps broadcasting on the old session; the new session never sees it.
    host.drain();
    editor.drain();
    editor.engine.broadcast_mutation(&edit("after kill, old link")).unwrap();
    assert!(host.mutations_within(Duration::from_secs(2)).is_empty(), "old link still reaches new session");

    // And nothing from the new session reaches the old editor.
    host.engine.broadcast_mutation(&edit("new session only")).unwrap();
    assert!(editor.mutations_within(Duration::from_secs(1)).is_empty(), "new session leaked to old link");

    // A fresh peer using the old link cannot fetch the document...
    let mut intruder = Peer::new();
    intruder.engine.join_via_ticket(&links1.edit_uri).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !intruder.seen(|e| matches!(e, SyncEvent::BaseDocumentReady { .. })),
        "old link must not yield the document"
    );
    assert_eq!(host.engine.connected_peers_count(), 0);

    // ...while the new link works.
    let mut newcomer = Peer::new();
    assert_eq!(newcomer.engine.join_via_ticket(&links2.edit_uri).unwrap(), ShareRole::JoinedCanEdit);
    assert_eq!(newcomer.wait_base().1, pdf);
    host.wait_peers(1);
}

#[test]
fn joiner_can_leave_and_rejoin_roles_reset() {
    let mut host = Peer::new();
    let mut viewer = Peer::new();
    let links = host.engine.enable_share_mode("a.pdf", b"%PDF tiny".to_vec()).unwrap();
    assert!(host.engine.enable_share_mode("a.pdf", vec![]).is_err(), "double enable");
    viewer.engine.join_via_ticket(&links.view_only_uri).unwrap();
    assert!(viewer.engine.join_via_ticket(&links.edit_uri).is_err(), "double join");
    viewer.wait_base();
    viewer.engine.disable_share_mode().unwrap();
    assert_eq!(viewer.engine.role(), ShareRole::Offline);
    assert_eq!(viewer.engine.join_via_ticket(&links.edit_uri).unwrap(), ShareRole::JoinedCanEdit);
}
