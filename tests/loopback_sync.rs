//! Two-node loopback test: base file blob transfer, live encrypted mutations
//! in both directions, presence and CRDT convergence, entirely over 127.0.0.1
//! (no relay, no discovery).

mod common;

use common::{fake_pdf, EditAction, Peer};
use simple_sync::{ShareRole, SyncEvent};

#[test]
fn two_node_loopback_sync() {
    let mut a = Peer::new();
    let mut b = Peer::new();
    assert_eq!(a.engine.role(), ShareRole::Offline);
    assert_eq!(a.engine.connected_peers_count(), 0);

    let pdf = fake_pdf();
    let links = a.engine.enable_share_mode("contract.pdf", pdf.clone()).unwrap();
    assert!(links.edit_uri.starts_with("ssync://") && links.view_only_uri.starts_with("ssync://"));
    assert_eq!(b.engine.join_via_ticket(&links.edit_uri).unwrap(), ShareRole::JoinedCanEdit);

    // Base document arrives byte-identical.
    let (filename, bytes) = b.wait_base();
    assert_eq!(filename, "contract.pdf");
    assert_eq!(bytes, pdf);

    // Presence.
    let (host_id, joiner_id) = (a.engine.local_peer_id(), b.engine.local_peer_id());
    a.wait("PeerJoined(b)", |e| matches!(e, SyncEvent::PeerJoined { peer_id } if *peer_id == joiner_id));
    b.wait("PeerJoined(a)", |e| matches!(e, SyncEvent::PeerJoined { peer_id } if *peer_id == host_id));
    assert_eq!(a.engine.connected_peers_count(), 1);
    assert_eq!(b.engine.connected_peers_count(), 1);

    // Host -> joiner.
    let m1 = EditAction::Annotate { page: 3, text: "sign here".into() };
    a.engine.broadcast_mutation(&m1).unwrap();
    let (author, bytes) = b.wait_mutation("a->b");
    assert_eq!(author, host_id);
    assert_eq!(bincode::deserialize::<EditAction>(&bytes).unwrap(), m1);

    // Joiner -> host.
    let m2 = EditAction::RotatePage { page: 1, degrees: 90 };
    b.engine.broadcast_mutation(&m2).unwrap();
    let (author, bytes) = a.wait_mutation("b->a");
    assert_eq!(author, joiner_id);
    assert_eq!(bincode::deserialize::<EditAction>(&bytes).unwrap(), m2);

    // CRDT deltas converge.
    a.engine.set_shared("title", "Q3 Contract").unwrap();
    b.wait("CrdtUpdated(title)", |e| {
        matches!(e, SyncEvent::CrdtUpdated { json_state } if json_state.contains("Q3 Contract"))
    });
    b.engine.set_shared("reviewer", "Bob").unwrap();
    match a.wait("CrdtUpdated(both)", |e| {
        matches!(e, SyncEvent::CrdtUpdated { json_state } if json_state.contains("Bob"))
    }) {
        SyncEvent::CrdtUpdated { json_state } => {
            assert!(json_state.contains("Q3 Contract") && json_state.contains("reviewer"))
        }
        _ => unreachable!(),
    }

    for p in [&mut a, &mut b] {
        assert!(!p.seen(|e| matches!(e, SyncEvent::Error(_))), "unexpected error events");
    }
}

#[test]
fn bad_tickets_and_offline_use_are_rejected() {
    let mut p = Peer::new();
    assert!(p.engine.join_via_ticket("not-a-ticket").is_err());
    assert!(p.engine.join_via_ticket("ssync://AAAA").is_err());
    assert_eq!(p.engine.role(), ShareRole::Offline);
    assert!(p.engine.broadcast_mutation(&"hi").is_err());
    assert!(p.engine.set_shared("k", "v").is_err());
}
