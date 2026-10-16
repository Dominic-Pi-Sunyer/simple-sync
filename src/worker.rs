//! Hidden background Tokio runtime owning the iroh endpoint, blob store and
//! gossip topic for the current share session. The host app never touches any
//! of this: it talks to the worker through channels (see `SyncEngine`).
//!
//! Every session (host or join) gets a brand-new iroh node key, so nothing
//! identifying survives a kill switch.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use ed25519_dalek::{SigningKey, VerifyingKey};
use futures_lite::StreamExt;
use iroh::{
    address_lookup::memory::MemoryLookup, endpoint::presets, protocol::Router, Endpoint,
    EndpointAddr, PublicKey, RelayMode, SecretKey,
};
use iroh_blobs::{store::mem::MemStore, BlobsProtocol, Hash};
use iroh_gossip::{
    api::{Event, GossipReceiver, GossipSender},
    net::{Gossip, GOSSIP_ALPN},
    proto::TopicId,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroize;

use crate::state::CrdtState;
use crate::ticket::SessionTicket;
use crate::{crypto, ShareLinks, SyncEvent, Wire};

/// Network configuration for a [`crate::SyncEngine`].
#[derive(Clone, Debug, Default)]
pub struct SyncConfig {
    /// `false` (default): n0 relays + DNS discovery, works across the internet.
    /// `true`: loopback only, no relay, no discovery. For tests and demos.
    pub local_only: bool,
}

impl SyncConfig {
    pub fn local_only() -> Self {
        Self { local_only: true }
    }
}

pub(crate) enum WorkerCommand {
    Enable {
        filename: String,
        bytes: Vec<u8>,
        reply: StdSender<Result<ShareLinks>>,
    },
    Disable {
        reply: StdSender<Result<()>>,
    },
    Join {
        ticket: SessionTicket,
        reply: StdSender<Result<()>>,
    },
    Mutation(Vec<u8>),
    CrdtSet {
        key: String,
        value: String,
    },
    /// Test hook: put raw bytes on the gossip topic, bypassing all checks.
    RawFrame(Vec<u8>),
}

/// State readable by the host app without blocking.
#[derive(Default)]
pub(crate) struct Shared {
    pub peers: AtomicUsize,
    pub peer_id: Mutex<String>,
}

impl Shared {
    fn reset(&self) {
        self.peers.store(0, Ordering::Relaxed);
        self.peer_id.lock().unwrap().clear();
    }
}

fn ae<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("{e}")
}

struct Material {
    read_key: [u8; 32],
    verifying: VerifyingKey,
    signing: Option<SigningKey>, // zeroized on drop by ed25519-dalek
}

impl Drop for Material {
    fn drop(&mut self) {
        self.read_key.zeroize();
    }
}

/// Session key material that can be wiped in place while tasks still hold a
/// reference. After `wipe()` every seal/open fails.
pub(crate) struct Keys(Mutex<Option<Material>>);

impl Keys {
    fn from_ticket(t: &SessionTicket) -> Result<Self> {
        Ok(Self(Mutex::new(Some(Material {
            read_key: t.read_key,
            verifying: VerifyingKey::from_bytes(&t.write_verifying_key).map_err(ae)?,
            signing: t.write_signing_key.as_ref().map(SigningKey::from_bytes),
        }))))
    }

    fn seal(&self, author: [u8; 32], payload: &[u8]) -> Result<Vec<u8>> {
        match &*self.0.lock().unwrap() {
            Some(m) => crypto::seal(&m.read_key, m.signing.as_ref(), author, payload),
            None => bail!("session keys wiped"),
        }
    }

    fn open(&self, frame: &[u8]) -> Result<([u8; 32], Vec<u8>)> {
        match &*self.0.lock().unwrap() {
            Some(m) => crypto::open(&m.read_key, &m.verifying, frame),
            None => bail!("session keys wiped"),
        }
    }

    fn can_write(&self) -> bool {
        matches!(&*self.0.lock().unwrap(), Some(m) if m.signing.is_some())
    }

    fn wipe(&self) {
        self.0.lock().unwrap().take(); // Material::drop + SigningKey zeroize
    }
}

struct Session {
    router: Router,
    sender: GossipSender,
    keys: Arc<Keys>,
    author: [u8; 32],
    crdt: Arc<Mutex<CrdtState>>,
    tasks: Vec<JoinHandle<()>>,
}

pub(crate) struct Worker {
    cfg: SyncConfig,
    evt_tx: mpsc::UnboundedSender<SyncEvent>,
    shared: Arc<Shared>,
    session: Option<Session>,
}

impl Worker {
    pub(crate) fn new(
        cfg: SyncConfig,
        evt_tx: mpsc::UnboundedSender<SyncEvent>,
        shared: Arc<Shared>,
    ) -> Self {
        Self { cfg, evt_tx, shared, session: None }
    }

    pub(crate) async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<WorkerCommand>) {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                WorkerCommand::Enable { filename, bytes, reply } => {
                    let _ = reply.send(self.enable(filename, bytes).await);
                }
                WorkerCommand::Disable { reply } => {
                    // Keys are wiped and tasks aborted before we acknowledge; the
                    // (slower) network shutdown happens after the reply.
                    let router = self.teardown();
                    let _ = reply.send(Ok(()));
                    if let Some(r) = router {
                        let _ = r.shutdown().await;
                    }
                }
                WorkerCommand::Join { ticket, reply } => {
                    let _ = reply.send(self.join(ticket).await);
                }
                WorkerCommand::Mutation(bytes) => self.send_or_report(Wire::Mutation(bytes)).await,
                WorkerCommand::CrdtSet { key, value } => {
                    let delta = match &self.session {
                        Some(s) => s.crdt.lock().unwrap().set(&key, &value),
                        None => Err(anyhow!("no active session")),
                    };
                    match delta {
                        Ok(d) => self.send_or_report(Wire::Crdt(d)).await,
                        Err(e) => self.report(e),
                    }
                }
                WorkerCommand::RawFrame(bytes) => {
                    if let Some(s) = &self.session {
                        let _ = s.sender.broadcast(Bytes::from(bytes)).await;
                    }
                }
            }
        }
        // Engine dropped: tear the node down.
        if let Some(r) = self.teardown() {
            let _ = r.shutdown().await;
        }
    }

    /// Kill switch core: stop tasks, wipe keys, reset shared state.
    fn teardown(&mut self) -> Option<Router> {
        let s = self.session.take()?;
        for t in &s.tasks {
            t.abort();
        }
        s.keys.wipe();
        self.shared.reset();
        Some(s.router) // blob store + gossip topic die with the router
    }

    fn report(&self, e: anyhow::Error) {
        let _ = self.evt_tx.send(SyncEvent::Error(format!("{e:#}")));
    }

    async fn send_or_report(&self, wire: Wire) {
        let res = match &self.session {
            Some(s) => send_wire(&s.sender, &s.keys, s.author, &wire).await,
            None => Err(anyhow!("no active session")),
        };
        if let Err(e) = res {
            self.report(e);
        }
    }

    async fn bind(&self, secret: SecretKey, lookup: Option<MemoryLookup>) -> Result<Endpoint> {
        let mut b = if self.cfg.local_only {
            Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind_addr(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
                .map_err(ae)?
        } else {
            Endpoint::builder(presets::N0)
        };
        b = b.secret_key(secret);
        if let Some(l) = lookup {
            b = b.address_lookup(l);
        }
        b.bind().await.map_err(ae)
    }

    fn start_recv(
        &self,
        receiver: GossipReceiver,
        keys: &Arc<Keys>,
        author: [u8; 32],
        sender: &GossipSender,
        crdt: &Arc<Mutex<CrdtState>>,
        ready: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        tokio::spawn(recv_loop(
            receiver,
            RecvCtx {
                keys: keys.clone(),
                author,
                evt_tx: self.evt_tx.clone(),
                sender: sender.clone(),
                crdt: crdt.clone(),
                shared: self.shared.clone(),
            },
            ready,
        ))
    }

    async fn enable(&mut self, filename: String, bytes: Vec<u8>) -> Result<ShareLinks> {
        if self.session.is_some() {
            bail!("a session is already active; stop it first");
        }
        let b3 = blake3::hash(&bytes);
        let secret = SecretKey::generate();
        let endpoint = self.bind(secret.clone(), None).await?;

        let store = MemStore::new();
        let tag = store.blobs().add_bytes(Bytes::from(bytes)).await.map_err(ae)?;
        if tag.hash.as_bytes() != b3.as_bytes() {
            bail!("blob store hash does not match BLAKE3 of the base file");
        }
        let blobs = BlobsProtocol::new(&store, None);
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();

        if !self.cfg.local_only {
            // Give the endpoint a moment to learn its home relay for the ticket.
            let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
        }
        let addr = endpoint.addr();
        let mut ips: Vec<SocketAddr> =
            addr.ip_addrs().copied().filter(|a| !a.ip().is_unspecified()).collect();
        if ips.is_empty() {
            ips = endpoint.bound_sockets().into_iter().filter(|a| !a.ip().is_unspecified()).collect();
        }
        let mut host_addr = EndpointAddr::new(endpoint.id());
        for ip in ips {
            host_addr = host_addr.with_ip_addr(ip);
        }
        for r in addr.relay_urls() {
            host_addr = host_addr.with_relay_url(r.clone());
        }

        let ticket = SessionTicket::generate(host_addr, *b3.as_bytes(), &filename);
        let keys = Arc::new(Keys::from_ticket(&ticket)?);
        let topic = gossip
            .subscribe(TopicId::from_bytes(ticket.gossip_topic_id), vec![])
            .await
            .map_err(ae)?;
        let (sender, receiver) = topic.split();
        let crdt = Arc::new(Mutex::new(CrdtState::new()));
        let author = *secret.public().as_bytes();
        let (_ready_tx, ready_rx) = watch::channel(true);
        let recv = self.start_recv(receiver, &keys, author, &sender, &crdt, ready_rx);

        *self.shared.peer_id.lock().unwrap() = secret.public().to_string();
        self.shared.peers.store(0, Ordering::Relaxed);
        self.session = Some(Session { router, sender, keys, author, crdt, tasks: vec![recv] });
        Ok(ShareLinks { edit_uri: ticket.to_edit_uri(), view_only_uri: ticket.to_view_only_uri() })
    }

    async fn join(&mut self, t: SessionTicket) -> Result<()> {
        if self.session.is_some() {
            bail!("a session is already active; stop it first");
        }
        let keys = Arc::new(Keys::from_ticket(&t)?);
        let host_id = t.host_addr.id;
        let lookup = MemoryLookup::new();
        lookup.add_endpoint_info(t.host_addr.clone());
        let secret = SecretKey::generate();
        let endpoint = self.bind(secret.clone(), Some(lookup)).await?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        // Messages sent before the first neighbor connects are queued by gossip.
        let topic = gossip
            .subscribe(TopicId::from_bytes(t.gossip_topic_id), vec![host_id])
            .await
            .map_err(ae)?;
        let (sender, receiver) = topic.split();
        let crdt = Arc::new(Mutex::new(CrdtState::new()));
        let author = *secret.public().as_bytes();
        let (ready_tx, ready_rx) = watch::channel(false);
        let recv = self.start_recv(receiver, &keys, author, &sender, &crdt, ready_rx);

        // Fetch the base file; iroh-blobs verifies the BLAKE3 tree while streaming.
        let evt_tx = self.evt_tx.clone();
        let filename = t.filename.clone();
        let hash = Hash::from(t.doc_hash);
        let ep = endpoint.clone();
        let download = tokio::spawn(async move {
            let store = MemStore::new();
            let res: Result<Bytes> = async {
                store.downloader(&ep).download(hash, Some(host_id)).await.map_err(ae)?;
                store.blobs().get_bytes(hash).await.map_err(ae)
            }
            .await;
            match res {
                Ok(bytes) => {
                    let _ = evt_tx.send(SyncEvent::BaseDocumentReady { filename, bytes: bytes.to_vec() });
                    let _ = ready_tx.send(true);
                }
                Err(e) => {
                    let _ = evt_tx.send(SyncEvent::Error(format!("base file download failed: {e:#}")));
                }
            }
        });

        *self.shared.peer_id.lock().unwrap() = secret.public().to_string();
        self.shared.peers.store(0, Ordering::Relaxed);
        self.session =
            Some(Session { router, sender, keys, author, crdt, tasks: vec![recv, download] });
        Ok(())
    }
}

async fn send_wire(sender: &GossipSender, keys: &Keys, author: [u8; 32], wire: &Wire) -> Result<()> {
    let frame = keys.seal(author, &bincode::serialize(wire)?)?;
    sender.broadcast(Bytes::from(frame)).await.map_err(ae)
}

struct RecvCtx {
    keys: Arc<Keys>,
    author: [u8; 32],
    evt_tx: mpsc::UnboundedSender<SyncEvent>,
    sender: GossipSender,
    crdt: Arc<Mutex<CrdtState>>,
    shared: Arc<Shared>,
}

impl RecvCtx {
    fn emit(&self, e: SyncEvent) {
        let _ = self.evt_tx.send(e);
    }

    fn on_message(&self, content: &[u8]) {
        // Undecryptable or unauthorized frames are dropped silently.
        let Ok((from, data)) = self.keys.open(content) else { return };
        if from == self.author {
            return;
        }
        let author_peer_id = PublicKey::from_bytes(&from)
            .map(|k| k.to_string())
            .unwrap_or_else(|_| data_encoding::HEXLOWER.encode(&from));
        match bincode::deserialize::<Wire>(&data) {
            Ok(Wire::Mutation(mutation_bytes)) => {
                self.emit(SyncEvent::RemoteMutation { author_peer_id, mutation_bytes })
            }
            Ok(Wire::Crdt(bytes)) => {
                let merged = {
                    let c = self.crdt.lock().unwrap();
                    c.apply(&bytes).map(|_| c.json())
                };
                match merged {
                    Ok(json_state) => self.emit(SyncEvent::CrdtUpdated { json_state }),
                    Err(e) => self.emit(SyncEvent::Error(format!("{e:#}"))),
                }
            }
            Err(e) => self.emit(SyncEvent::Error(format!("malformed message: {e}"))),
        }
    }

    async fn catch_up_neighbor(&self) {
        if !self.keys.can_write() {
            return; // view-only peers never publish
        }
        let snapshot = {
            let c = self.crdt.lock().unwrap();
            if c.is_empty() { None } else { c.snapshot().ok() }
        };
        if let Some(s) = snapshot {
            if let Err(e) = send_wire(&self.sender, &self.keys, self.author, &Wire::Crdt(s)).await {
                self.emit(SyncEvent::Error(format!("{e:#}")));
            }
        }
    }
}

async fn recv_loop(mut rx: GossipReceiver, ctx: RecvCtx, mut ready: watch::Receiver<bool>) {
    // Joiners buffer gossip until the base document has been delivered, so the
    // host app never sees a mutation for a document it has not loaded yet.
    if ready.wait_for(|r| *r).await.is_err() {
        return;
    }
    let mut neighbors: HashSet<PublicKey> = HashSet::new();
    while let Some(ev) = rx.next().await {
        match ev {
            Ok(Event::Received(msg)) => ctx.on_message(&msg.content),
            Ok(Event::NeighborUp(id)) => {
                neighbors.insert(id);
                ctx.shared.peers.store(neighbors.len(), Ordering::Relaxed);
                ctx.emit(SyncEvent::PeerJoined { peer_id: id.to_string() });
                ctx.catch_up_neighbor().await;
            }
            Ok(Event::NeighborDown(id)) => {
                neighbors.remove(&id);
                ctx.shared.peers.store(neighbors.len(), Ordering::Relaxed);
                ctx.emit(SyncEvent::PeerLeft { peer_id: id.to_string() });
            }
            Ok(Event::Lagged) => {
                ctx.emit(SyncEvent::Error("gossip receiver lagged; some updates were dropped".into()))
            }
            Err(e) => {
                ctx.emit(SyncEvent::Error(format!("gossip closed: {e}")));
                break;
            }
        }
    }
}
