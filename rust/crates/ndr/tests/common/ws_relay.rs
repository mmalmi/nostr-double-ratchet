//! WebSocket-based Nostr relay for testing
//!
//! This implements enough of NIP-01 to support the WebRTC signaling tests.
//! It runs as a local server that nostr-sdk clients can connect to.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, RwLock};

/// Nostr event structure (simplified for testing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// Subscription filter
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NostrFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Arbitrary NIP-01 tag filters like "#p", "#d", "#l", etc.
    ///
    /// The value list matches on the 2nd element of a Nostr tag:
    /// filter {"#d":["x"]} matches event tag ["d","x",...].
    #[serde(flatten, default, skip_serializing_if = "HashMap::is_empty")]
    pub tag_filters: HashMap<String, Vec<String>>,
}

impl NostrFilter {
    pub fn matches(&self, event: &NostrEvent) -> bool {
        if let Some(ref ids) = self.ids {
            if !ids.contains(&event.id) {
                return false;
            }
        }
        if let Some(ref authors) = self.authors {
            if !authors.contains(&event.pubkey) {
                return false;
            }
        }
        if let Some(ref kinds) = self.kinds {
            if !kinds.contains(&event.kind) {
                return false;
            }
        }
        for (key, vals) in &self.tag_filters {
            let Some(tag_name) = key.strip_prefix('#') else {
                continue;
            };
            if tag_name.is_empty() || vals.is_empty() {
                continue;
            }

            let has_match = event.tags.iter().any(|t| {
                t.len() >= 2 && t[0] == tag_name && vals.iter().any(|v| v == &t[1])
            });
            if !has_match {
                return false;
            }
        }
        if let Some(since) = self.since {
            if event.created_at < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if event.created_at > until {
                return false;
            }
        }
        true
    }
}

/// Client subscription
struct Subscription {
    filters: Vec<NostrFilter>,
}

/// Shared relay state
struct RelayState {
    /// Stored events
    events: RwLock<Vec<NostrEvent>>,
    /// Broadcast channel for new events
    broadcast: broadcast::Sender<NostrEvent>,
}

impl RelayState {
    fn new() -> Self {
        let (broadcast, _) = broadcast::channel(10000);
        Self {
            events: RwLock::new(Vec::new()),
            broadcast,
        }
    }
}

/// WebSocket Nostr relay for testing
pub struct WsRelay {
    state: Arc<RelayState>,
    addr: Option<SocketAddr>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl WsRelay {
    /// Create a new WebSocket relay
    pub fn new() -> Self {
        Self {
            state: Arc::new(RelayState::new()),
            addr: None,
            shutdown_tx: None,
        }
    }

    /// Start the relay on a random available port
    pub async fn start(&mut self) -> Result<SocketAddr, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        self.addr = Some(addr);

        let state = self.state.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let app = Router::new().route("/", get(ws_handler)).with_state(state);

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_rx.recv().await;
                })
                .await
                .ok();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        Ok(addr)
    }

    /// Get the WebSocket URL for connecting
    pub fn url(&self) -> Option<String> {
        self.addr.map(|addr| format!("ws://{}", addr))
    }

    /// Stop the relay
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
    }

    /// Get event count (for testing)
    #[allow(dead_code)]
    pub async fn event_count(&self) -> usize {
        self.state.events.read().await.len()
    }

    /// Get all events (for testing)
    #[allow(dead_code)]
    pub async fn events(&self) -> Vec<NostrEvent> {
        self.state.events.read().await.clone()
    }

    /// Clear all events
    #[allow(dead_code)]
    pub async fn clear(&self) {
        self.state.events.write().await.clear();
    }
}

impl Default for WsRelay {
    fn default() -> Self {
        Self::new()
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RelayState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<RelayState>) {
    let (mut sender, mut receiver) = socket.split();

    // Per-client subscriptions
    let subscriptions: Arc<RwLock<HashMap<String, Subscription>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Channel for sending messages to client
    let (tx, mut rx) = mpsc::channel::<String>(1000);

    // Subscribe to broadcast
    let mut broadcast_rx = state.broadcast.subscribe();
    let subscriptions_clone = subscriptions.clone();
    let tx_clone = tx.clone();

    // Task to forward broadcast events to matching subscriptions
    let broadcast_task = tokio::spawn(async move {
        while let Ok(event) = broadcast_rx.recv().await {
            let subs = subscriptions_clone.read().await;
            for (sub_id, sub) in subs.iter() {
                let matches = sub.filters.iter().any(|f| f.matches(&event));
                if matches {
                    let msg = serde_json::json!(["EVENT", sub_id, event]);
                    let _ = tx_clone.send(msg.to_string()).await;
                }
            }
        }
    });

    // Task to send messages to client
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages
    while let Some(Ok(msg)) = receiver.next().await {
        if let Message::Text(text) = msg {
            if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                if parsed.is_empty() {
                    continue;
                }

                let msg_type = parsed[0].as_str().unwrap_or("");

                match msg_type {
                    "EVENT" => {
                        if parsed.len() >= 2 {
                            if let Ok(event) =
                                serde_json::from_value::<NostrEvent>(parsed[1].clone())
                            {
                                let event_id = event.id.clone();

                                // Store event
                                state.events.write().await.push(event.clone());

                                // Broadcast to all subscribers
                                let _ = state.broadcast.send(event);

                                // Send OK
                                let ok_msg = serde_json::json!(["OK", event_id, true, ""]);
                                let _ = tx.send(ok_msg.to_string()).await;
                            }
                        }
                    }
                    "REQ" => {
                        if parsed.len() >= 3 {
                            let sub_id = parsed[1].as_str().unwrap_or("").to_string();
                            let mut filters = Vec::new();

                            for v in parsed.iter().skip(2) {
                                if let Ok(filter) = serde_json::from_value::<NostrFilter>(v.clone())
                                {
                                    filters.push(filter);
                                }
                            }

                            // Store subscription
                            subscriptions.write().await.insert(
                                sub_id.clone(),
                                Subscription {
                                    filters: filters.clone(),
                                },
                            );

                            // Send matching stored events.
                            // Registering the subscription first avoids a race where an event
                            // published during REQ handling could be missed by both backlog replay
                            // and live broadcast fanout.
                            let events = state.events.read().await;
                            for event in events.iter() {
                                if filters.iter().any(|f| f.matches(event)) {
                                    let msg = serde_json::json!(["EVENT", &sub_id, event]);
                                    let _ = tx.send(msg.to_string()).await;
                                }
                            }
                            drop(events);

                            // Send EOSE
                            let eose_msg = serde_json::json!(["EOSE", &sub_id]);
                            let _ = tx.send(eose_msg.to_string()).await;
                        }
                    }
                    "CLOSE" => {
                        if parsed.len() >= 2 {
                            let sub_id = parsed[1].as_str().unwrap_or("");
                            subscriptions.write().await.remove(sub_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Clean up
    broadcast_task.abort();
    send_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_relay_starts_and_stops() {
        let mut relay = WsRelay::new();
        let addr = relay.start().await.expect("Failed to start relay");
        assert!(relay.url().is_some());
        println!("Relay started at: {}", addr);
        relay.stop().await;
    }

    #[test]
    fn test_filter_matches_arbitrary_tag_filters() {
        let event = NostrEvent {
            id: "e1".to_string(),
            pubkey: "pk1".to_string(),
            created_at: 123,
            kind: 30078,
            tags: vec![
                vec!["d".to_string(), "double-ratchet/app-keys".to_string()],
                vec!["l".to_string(), "double-ratchet/invites".to_string()],
                vec!["p".to_string(), "someone".to_string()],
            ],
            content: "".to_string(),
            sig: "sig".to_string(),
        };

        let filter: NostrFilter =
            serde_json::from_str("{\"#d\":[\"double-ratchet/app-keys\"]}").unwrap();
        assert!(filter.matches(&event));

        let filter: NostrFilter =
            serde_json::from_str("{\"#l\":[\"double-ratchet/invites\"]}").unwrap();
        assert!(filter.matches(&event));

        let filter: NostrFilter = serde_json::from_str("{\"#p\":[\"someone\"]}").unwrap();
        assert!(filter.matches(&event));

        let filter: NostrFilter = serde_json::from_str("{\"#d\":[\"nope\"]}").unwrap();
        assert!(!filter.matches(&event));
    }
}
