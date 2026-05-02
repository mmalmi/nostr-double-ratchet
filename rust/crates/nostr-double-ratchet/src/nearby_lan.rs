use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{
    decode_nearby_frame_json, encode_nearby_frame_json, nearby_frame_body_len_from_header,
    NEARBY_FRAME_HEADER_BYTES,
};

pub const IRIS_NEARBY_SERVICE_TYPE: &str = "_iris-chat._tcp.local.";

const CONNECT_TIMEOUT_MS: u64 = 750;
const HELLO_NAME: &str = "iris";

#[derive(Debug, thiserror::Error)]
pub enum NearbyLanError {
    #[error("nearby bind address must be loopback or private LAN")]
    InvalidBindAddress,

    #[error("nearby TCP error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nearby JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("nearby mDNS error: {0}")]
    Mdns(String),
}

#[derive(Debug, Clone)]
pub struct NearbyLanConfig {
    pub peer_id: String,
    pub bind_addr: Option<SocketAddr>,
    pub explicit_peers: Vec<SocketAddr>,
    pub service_type: String,
    pub service_name: String,
    pub mdns_port: Option<u16>,
}

impl NearbyLanConfig {
    pub fn new(peer_id: impl Into<String>) -> Self {
        let peer_id = peer_id.into();
        let service_name = format!("iris-{}", short_hash(&peer_id));
        Self {
            peer_id,
            bind_addr: None,
            explicit_peers: Vec::new(),
            service_type: IRIS_NEARBY_SERVICE_TYPE.to_string(),
            service_name,
            mdns_port: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NearbyLanIncoming {
    pub event: nostr::Event,
    pub remote_peer_id: Option<String>,
}

pub struct NearbyLanService {
    peer_id: String,
    endpoint: SocketAddr,
    explicit_peers: Vec<SocketAddr>,
    known_peers: Arc<Mutex<HashSet<SocketAddr>>>,
    incoming_rx: mpsc::UnboundedReceiver<NearbyLanIncoming>,
    tasks: Vec<JoinHandle<()>>,
    mdns: Option<ServiceDaemon>,
    service_type: String,
    service_fullname: String,
    browser_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventEnvelope {
    v: u8,
    #[serde(rename = "type")]
    kind: String,
    peer_id: String,
    event_json: String,
}

impl NearbyLanService {
    pub async fn start(config: NearbyLanConfig) -> Result<Self, NearbyLanError> {
        let bind_addr = config.bind_addr.unwrap_or_else(default_private_bind_addr);
        if !is_allowed_nearby_bind(&bind_addr) {
            return Err(NearbyLanError::InvalidBindAddress);
        }

        let listener = TcpListener::bind(bind_addr).await?;
        let endpoint = listener.local_addr()?;
        let explicit_peers = config
            .explicit_peers
            .iter()
            .copied()
            .filter(is_allowed_nearby_peer)
            .collect::<Vec<_>>();

        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let known_peers = Arc::new(Mutex::new(HashSet::new()));

        let (mdns, service_fullname, browser_thread) = start_mdns(
            &config,
            endpoint,
            Arc::clone(&known_peers),
            incoming_tx.clone(),
        )?;
        let tasks = vec![spawn_accept_loop(
            listener,
            incoming_tx,
            config.peer_id.clone(),
        )];

        Ok(Self {
            peer_id: config.peer_id,
            endpoint,
            explicit_peers,
            known_peers,
            incoming_rx,
            tasks,
            mdns: Some(mdns),
            service_type: config.service_type,
            service_fullname,
            browser_thread: Some(browser_thread),
        })
    }

    pub async fn recv(&mut self) -> Option<NearbyLanIncoming> {
        self.incoming_rx.recv().await
    }

    pub async fn publish_event(&self, event: &nostr::Event) -> usize {
        let peers = self.peers();
        if peers.is_empty() {
            return 0;
        }

        let Some(frame) = encode_event_frame(&self.peer_id, event) else {
            return 0;
        };

        let mut delivered = 0;
        for peer in peers {
            if send_frame_to_peer(peer, &frame).await {
                delivered += 1;
            }
        }
        delivered
    }

    pub fn peers(&self) -> Vec<SocketAddr> {
        let mut peers = self.explicit_peers.clone();
        if let Ok(known) = self.known_peers.lock() {
            peers.extend(known.iter().copied());
        }
        let mut seen = HashSet::new();
        peers
            .into_iter()
            .filter(|peer| is_allowed_nearby_peer(peer) && *peer != self.endpoint)
            .filter(|peer| seen.insert(*peer))
            .collect()
    }

    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }
}

impl Drop for NearbyLanService {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Some(mdns) = &self.mdns {
            let _ = mdns.stop_browse(&self.service_type);
            let _ = mdns.unregister(&self.service_fullname);
            let _ = mdns.shutdown();
        }
        let _ = self.browser_thread.take();
    }
}

pub fn is_allowed_nearby_peer(addr: &SocketAddr) -> bool {
    is_local_or_private_ip(addr.ip())
}

fn is_allowed_nearby_bind(addr: &SocketAddr) -> bool {
    !addr.ip().is_unspecified() && is_local_or_private_ip(addr.ip())
}

fn start_mdns(
    config: &NearbyLanConfig,
    endpoint: SocketAddr,
    known_peers: Arc<Mutex<HashSet<SocketAddr>>>,
    incoming_tx: mpsc::UnboundedSender<NearbyLanIncoming>,
) -> Result<(ServiceDaemon, String, std::thread::JoinHandle<()>), NearbyLanError> {
    let mdns = match config.mdns_port {
        Some(port) => ServiceDaemon::new_with_port(port),
        None => ServiceDaemon::new(),
    }
    .map_err(|error| NearbyLanError::Mdns(error.to_string()))?;

    let mut properties = HashMap::new();
    properties.insert("peer_id".to_string(), config.peer_id.clone());
    properties.insert("protocol".to_string(), "iris-nearby-v1".to_string());

    let host_name = format!("{}.local.", sanitize_service_label(&config.service_name));
    let service = ServiceInfo::new(
        &config.service_type,
        &config.service_name,
        &host_name,
        endpoint.ip().to_string(),
        endpoint.port(),
        Some(properties),
    )
    .map_err(|error| NearbyLanError::Mdns(error.to_string()))?;
    let service_fullname = service.get_fullname().to_string();
    mdns.register(service)
        .map_err(|error| NearbyLanError::Mdns(error.to_string()))?;

    let receiver = mdns
        .browse(&config.service_type)
        .map_err(|error| NearbyLanError::Mdns(error.to_string()))?;
    let self_peer_id = config.peer_id.clone();
    let thread_known_peers = Arc::clone(&known_peers);
    let runtime_handle = tokio::runtime::Handle::current();
    let thread = std::thread::Builder::new()
        .name("iris-nearby-mdns".to_string())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        if service.get_property_val_str("peer_id") == Some(self_peer_id.as_str()) {
                            continue;
                        }
                        let port = service.get_port();
                        for scoped_ip in service.get_addresses() {
                            let addr = SocketAddr::new(scoped_ip.to_ip_addr(), port);
                            if !is_allowed_nearby_peer(&addr) {
                                continue;
                            }
                            let inserted = thread_known_peers
                                .lock()
                                .map(|mut peers| peers.insert(addr))
                                .unwrap_or(false);
                            if inserted {
                                let incoming_tx = incoming_tx.clone();
                                let peer_id = self_peer_id.clone();
                                runtime_handle.spawn(async move {
                                    connect_and_read_peer(addr, peer_id, incoming_tx).await;
                                });
                            }
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, _) | ServiceEvent::SearchStopped(_) => {}
                    ServiceEvent::SearchStarted(_) | ServiceEvent::ServiceFound(_, _) => {}
                    _ => {}
                }
            }
        })
        .map_err(|error| NearbyLanError::Mdns(error.to_string()))?;

    Ok((mdns, service_fullname, thread))
}

fn spawn_accept_loop(
    listener: TcpListener,
    incoming_tx: mpsc::UnboundedSender<NearbyLanIncoming>,
    peer_id: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok((stream, remote_addr)) = listener.accept().await else {
                break;
            };
            if !is_allowed_nearby_peer(&remote_addr) {
                continue;
            }
            let incoming_tx = incoming_tx.clone();
            let peer_id = peer_id.clone();
            tokio::spawn(async move {
                read_peer_stream(stream, peer_id, incoming_tx, true).await;
            });
        }
    })
}

async fn connect_and_read_peer(
    addr: SocketAddr,
    peer_id: String,
    incoming_tx: mpsc::UnboundedSender<NearbyLanIncoming>,
) {
    let connect = tokio::time::timeout(
        Duration::from_millis(CONNECT_TIMEOUT_MS),
        TcpStream::connect(addr),
    )
    .await;
    let Ok(Ok(stream)) = connect else {
        return;
    };
    read_peer_stream(stream, peer_id, incoming_tx, true).await;
}

async fn read_peer_stream(
    mut stream: TcpStream,
    peer_id: String,
    incoming_tx: mpsc::UnboundedSender<NearbyLanIncoming>,
    send_hello: bool,
) {
    let _ = stream.set_nodelay(true);
    if send_hello {
        if let Some(frame) = encode_hello_frame(&peer_id) {
            let _ = stream.write_all(&frame).await;
        }
    }

    loop {
        let frame = match read_nearby_frame_async(&mut stream).await {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let Some(incoming) = decode_incoming_frame(&frame, &peer_id) else {
            continue;
        };
        let _ = incoming_tx.send(incoming);
    }
}

async fn read_nearby_frame_async(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; NEARBY_FRAME_HEADER_BYTES];
    if let Err(error) = stream.read_exact(&mut header).await {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(error);
    }

    let body_len = nearby_frame_body_len_from_header(&header)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad frame"))?;
    let mut frame = Vec::with_capacity(NEARBY_FRAME_HEADER_BYTES + body_len);
    frame.extend_from_slice(&header);
    let start = frame.len();
    frame.resize(start + body_len, 0);
    stream.read_exact(&mut frame[start..]).await?;
    Ok(Some(frame))
}

fn decode_incoming_frame(frame: &[u8], self_peer_id: &str) -> Option<NearbyLanIncoming> {
    let json = decode_nearby_frame_json(frame)?;
    let value: Value = serde_json::from_str(&json).ok()?;
    if value.get("v")?.as_u64()? != 1 {
        return None;
    }
    let remote_peer_id = value
        .get("peer_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|peer_id| !peer_id.is_empty())
        .map(str::to_string);
    if remote_peer_id.as_deref() == Some(self_peer_id) {
        return None;
    }
    if value.get("type")?.as_str()? != "event" {
        return None;
    }
    let event_json = value.get("event_json")?.as_str()?;
    let event: nostr::Event = nostr::JsonUtil::from_json(event_json).ok()?;
    if event.verify().is_err() {
        return None;
    }
    Some(NearbyLanIncoming {
        event,
        remote_peer_id,
    })
}

fn encode_hello_frame(peer_id: &str) -> Option<Vec<u8>> {
    let envelope = serde_json::json!({
        "v": 1,
        "type": "hello",
        "peer_id": peer_id,
        "nonce": uuid::Uuid::new_v4().to_string(),
        "name": HELLO_NAME,
    });
    encode_nearby_frame_json(&serde_json::to_string(&envelope).ok()?)
}

fn encode_event_frame(peer_id: &str, event: &nostr::Event) -> Option<Vec<u8>> {
    let envelope = EventEnvelope {
        v: 1,
        kind: "event".to_string(),
        peer_id: peer_id.to_string(),
        event_json: nostr::JsonUtil::as_json(event),
    };
    encode_nearby_frame_json(&serde_json::to_string(&envelope).ok()?)
}

async fn send_frame_to_peer(peer: SocketAddr, frame: &[u8]) -> bool {
    let connect = tokio::time::timeout(
        Duration::from_millis(CONNECT_TIMEOUT_MS),
        TcpStream::connect(peer),
    )
    .await;
    let Ok(Ok(mut stream)) = connect else {
        return false;
    };
    if stream.write_all(frame).await.is_err() {
        return false;
    }
    let _ = stream.shutdown().await;
    true
}

fn default_private_bind_addr() -> SocketAddr {
    SocketAddr::new(
        discover_private_ipv4()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        0,
    )
}

fn discover_private_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(224, 0, 0, 251), 5353)).ok()?;
    let SocketAddr::V4(addr) = socket.local_addr().ok()? else {
        return None;
    };
    let ip = *addr.ip();
    if is_local_or_private_ip(IpAddr::V4(ip)) && !ip.is_loopback() {
        Some(ip)
    } else {
        None
    }
}

fn is_local_or_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback() || is_ipv6_unique_local(ip) || is_ipv6_unicast_link_local(ip)
        }
    }
}

fn is_ipv6_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn sanitize_service_label(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .take(40)
        .collect::<String>();
    if out.is_empty() {
        out = "iris".to_string();
    }
    out
}

fn short_hash<T: Hash>(value: &T) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_public_peer_addresses() {
        let public_addr: SocketAddr = "8.8.8.8:9000".parse().unwrap();
        let private_addr: SocketAddr = "192.168.1.10:9000".parse().unwrap();
        assert!(!is_allowed_nearby_peer(&public_addr));
        assert!(is_allowed_nearby_peer(&private_addr));
    }

    #[test]
    fn event_frame_decodes_verified_event() {
        let keys = nostr::Keys::generate();
        let event = nostr::EventBuilder::new(nostr::Kind::TextNote, "nearby")
            .build(keys.public_key())
            .sign_with_keys(&keys)
            .unwrap();
        let frame = encode_event_frame("peer-a", &event).unwrap();
        let incoming = decode_incoming_frame(&frame, "peer-b").unwrap();
        assert_eq!(incoming.remote_peer_id.as_deref(), Some("peer-a"));
        assert_eq!(incoming.event.id, event.id);
    }
}
