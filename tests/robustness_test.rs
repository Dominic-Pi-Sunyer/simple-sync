//! Late joiners, replayed frames and large edits.

mod common;

use std::time::Duration;

use common::{fake_pdf, EditAction, Peer};
use ed25519_dalek::SigningKey;
use simple_sync::{crypto, SessionTicket, SyncEvent, Wire, MAX_MUTATION_BYTES};

fn edit(text: &str) -> EditAction {
    EditAction::Annotate {
        page: 1,
        text: text.into(),
    }
}

fn decode(bytes: &[u8]) -> EditAction {
    bincode::deserialize(bytes).unwrap()
}

/// A peer who joins after edits were made receives them, in order, after the base file.
#[test]
fn late_joiner_receives_earlier_edits_in_order() {
    let mut host = Peer::new();
    let links = host
        .engine
        .enable_share_mode("doc.pdf", fake_pdf())
        .unwrap();

    // Edits while nobody else is connected.
    for text in ["one", "two", "three"] {
        host.engine.broadcast_mutation(&edit(text)).unwrap();
    }

    let mut late = Peer::new();
    late.engine.join_via_ticket(&links.edit_uri).unwrap();
    late.wait_base();
    for text in ["one", "two", "three"] {
        assert_eq!(
            decode(&late.wait_mutation(&format!("replayed edit {text}")).1),
            edit(text)
        );
    }
}

/// Peers who already have an edit do not see it again when someone else joins.
#[test]
fn replay_to_a_new_joiner_does_not_duplicate_edits_for_existing_peers() {
    let mut host = Peer::new();
    let mut editor = Peer::new();
    let links = host
        .engine
        .enable_share_mode("doc.pdf", fake_pdf())
        .unwrap();
    editor.engine.join_via_ticket(&links.edit_uri).unwrap();
    editor.wait_base();
    host.wait_peers(1);

    host.engine.broadcast_mutation(&edit("once")).unwrap();
    assert_eq!(
        decode(&editor.wait_mutation("first delivery").1),
        edit("once")
    );

    let mut late = Peer::new();
    late.engine.join_via_ticket(&links.edit_uri).unwrap();
    late.wait_base();
    assert_eq!(
        decode(&late.wait_mutation("replay to late joiner").1),
        edit("once")
    );
    assert!(
        editor
            .mutations_within(Duration::from_millis(1500))
            .is_empty(),
        "the editor was handed the same edit twice"
    );
}

/// A peer holding a captured, genuinely signed frame (a View-Only peer can read every frame)
/// publishes it again: it must be applied once, not once per copy.
#[test]
fn a_republished_frame_is_applied_once() {
    let mut host = Peer::new();
    let mut viewer = Peer::new();
    let links = host
        .engine
        .enable_share_mode("doc.pdf", fake_pdf())
        .unwrap();
    viewer.engine.join_via_ticket(&links.view_only_uri).unwrap();
    viewer.wait_base();
    host.wait_peers(1);

    // A frame an editor could really have sent (the test holds the edit ticket's signing key).
    let t = SessionTicket::from_uri(&links.edit_uri).unwrap();
    let sk = SigningKey::from_bytes(&t.write_signing_key.unwrap());
    let payload = bincode::serialize(&Wire::Mutation(
        bincode::serialize(&edit("delete page 3")).unwrap(),
    ))
    .unwrap();
    let frame = crypto::seal(&t.read_key, Some(&sk), [9; 32], 1, &payload).unwrap();

    // Gossip drops byte-identical messages by itself, so a replaying peer (who holds the read key)
    // re-encrypts the same signed content: each copy has a fresh nonce and looks new on the wire.
    let inner = crypto::decrypt(&t.read_key, &frame).unwrap();
    host.drain();
    viewer.engine.__inject_raw_frame(frame);
    for _ in 0..3 {
        viewer
            .engine
            .__inject_raw_frame(crypto::encrypt(&t.read_key, &inner).unwrap());
    }
    let got = host.mutations_within(Duration::from_millis(1500));
    assert_eq!(
        got.len(),
        1,
        "the same signed edit was applied {} times",
        got.len()
    );
    assert_eq!(decode(&got[0]), edit("delete page 3"));
}

/// Edits well beyond iroh-gossip's 4 KiB default travel; edits beyond the limit fail clearly.
#[test]
fn large_edits_travel_and_oversized_ones_are_refused() {
    let mut host = Peer::new();
    let mut editor = Peer::new();
    let links = host
        .engine
        .enable_share_mode("doc.pdf", fake_pdf())
        .unwrap();
    editor.engine.join_via_ticket(&links.edit_uri).unwrap();
    editor.wait_base();
    host.wait_peers(1);

    let big = EditAction::Annotate {
        page: 1,
        text: "x".repeat(300 * 1024),
    };
    host.engine.broadcast_mutation(&big).unwrap();
    assert_eq!(decode(&editor.wait_mutation("300 KiB edit").1), big);

    let huge = EditAction::Annotate {
        page: 1,
        text: "x".repeat(MAX_MUTATION_BYTES + 10),
    };
    let err = host.engine.broadcast_mutation(&huge).unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
    assert!(!host.seen(|e| matches!(e, SyncEvent::Error(_))));
}
