use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};

type Subscriptions = Arc<Mutex<HashMap<String, Vec<Value>>>>;
type Events = Arc<Mutex<Vec<Value>>>;
type Clients = Arc<Mutex<HashMap<SocketAddr, tokio::sync::mpsc::UnboundedSender<Message>>>>;

pub struct LocalRelay {
    subscriptions: Subscriptions,
    events: Events,
    addr: SocketAddr,
}

impl LocalRelay {
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let subscriptions: Subscriptions = Arc::new(Mutex::new(HashMap::new()));
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

        let subs_clone = subscriptions.clone();
        let events_clone = events.clone();
        let clients_clone = clients.clone();

        tokio::spawn(async move {
            Self::run_server(listener, subs_clone, events_clone, clients_clone).await;
        });

        LocalRelay {
            subscriptions,
            events,
            addr,
        }
    }

    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.addr.port())
    }

    pub fn reset(&self) {
        self.subscriptions.lock().unwrap().clear();
        self.events.lock().unwrap().clear();
        eprintln!("🔄 Local relay reset");
    }

    async fn run_server(
        listener: TcpListener,
        subscriptions: Subscriptions,
        events: Events,
        clients: Clients,
    ) {
        while let Ok((stream, addr)) = listener.accept().await {
            let subs = subscriptions.clone();
            let evs = events.clone();
            let cls = clients.clone();

            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(stream, addr, subs, evs, cls).await {
                    eprintln!("❌ Local relay error: {}", e);
                }
            });
        }
    }

    async fn handle_connection(
        stream: TcpStream,
        addr: SocketAddr,
        subscriptions: Subscriptions,
        events: Events,
        clients: Clients,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ws_stream = accept_async(stream).await?;
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        clients.lock().unwrap().insert(addr, tx);

        eprintln!("🔌 Local relay: Client connected {}", addr);

        // Spawn sender task
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if ws_sender.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Client's active subscriptions
        let mut client_subs: HashMap<String, Vec<Value>> = HashMap::new();

        while let Some(msg) = ws_receiver.next().await {
            let msg = msg?;

            if let Message::Text(text) = msg {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    if let Some(arr) = val.as_array() {
                        if arr.is_empty() {
                            continue;
                        }

                        let msg_type = arr[0].as_str().unwrap_or("");

                        match msg_type {
                            "REQ" => {
                                if arr.len() >= 3 {
                                    let sub_id = arr[1].as_str().unwrap_or("").to_string();
                                    let filters = arr[2..].to_vec();

                                    eprintln!("📥 Local relay REQ {} filters: {:?}", sub_id, filters.len());
                                    for filter in &filters {
                                        eprintln!("   Filter: {}", serde_json::to_string(filter).unwrap());
                                    }

                                    client_subs.insert(sub_id.clone(), filters.clone());
                                    subscriptions.lock().unwrap().insert(sub_id.clone(), filters.clone());

                                    // Send matching stored events
                                    let stored = events.lock().unwrap();
                                    for event in stored.iter() {
                                        if Self::event_matches_filters(event, &filters) {
                                            let kind = event["kind"].as_u64().unwrap_or(0);
                                            let id = event["id"].as_str().unwrap_or("unknown");
                                            eprintln!("📨 Local relay: Sending stored kind {} id {}", kind, &id[..16]);

                                            let event_msg = json!(["EVENT", sub_id, event]);
                                            if let Some(tx) = clients.lock().unwrap().get(&addr) {
                                                let _ = tx.send(Message::Text(event_msg.to_string()));
                                            }
                                        }
                                    }

                                    // Send EOSE
                                    let eose_msg = json!(["EOSE", sub_id]);
                                    if let Some(tx) = clients.lock().unwrap().get(&addr) {
                                        let _ = tx.send(Message::Text(eose_msg.to_string()));
                                    }
                                    eprintln!("✅ Local relay: EOSE sent for {}", sub_id);
                                }
                            }
                            "EVENT" => {
                                if arr.len() >= 2 {
                                    let event = &arr[1];

                                    let event_id = event["id"].as_str().unwrap_or("unknown");
                                    let kind = event["kind"].as_u64().unwrap_or(0);

                                    eprintln!("📤 Local relay: Received EVENT kind {} id {}", kind, &event_id[..16]);

                                    // Show current subscriptions
                                    let all_subs = subscriptions.lock().unwrap();
                                    eprintln!("   Current subscriptions: {}", all_subs.len());
                                    for (sub_id, filters) in all_subs.iter() {
                                        for filter in filters {
                                            if let Some(kinds) = filter["kinds"].as_array() {
                                                eprintln!("     {} -> kinds: {:?}", &sub_id[..20], kinds);
                                            }
                                        }
                                    }
                                    drop(all_subs);

                                    // Store event
                                    events.lock().unwrap().push(event.clone());

                                    // Send OK
                                    let ok_msg = json!(["OK", event_id, true, ""]);
                                    if let Some(tx) = clients.lock().unwrap().get(&addr) {
                                        let _ = tx.send(Message::Text(ok_msg.to_string()));
                                    }

                                    // Broadcast to all clients with matching subscriptions
                                    let all_subs = subscriptions.lock().unwrap();
                                    let all_clients = clients.lock().unwrap();

                                    for (client_addr, client_tx) in all_clients.iter() {
                                        for (sub_id, filters) in all_subs.iter() {
                                            if Self::event_matches_filters(event, filters) {
                                                eprintln!("📨 Local relay: Broadcasting kind {} to {} sub {}", kind, client_addr, sub_id);
                                                let event_msg = json!(["EVENT", sub_id, event]);
                                                let _ = client_tx.send(Message::Text(event_msg.to_string()));
                                            }
                                        }
                                    }
                                }
                            }
                            "CLOSE" => {
                                if arr.len() >= 2 {
                                    let sub_id = arr[1].as_str().unwrap_or("");
                                    client_subs.remove(sub_id);
                                    eprintln!("🔴 Local relay: CLOSE {}", sub_id);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        clients.lock().unwrap().remove(&addr);
        eprintln!("🔌 Local relay: Client disconnected {}", addr);

        Ok(())
    }

    fn event_matches_filters(event: &Value, filters: &[Value]) -> bool {
        for filter in filters {
            if Self::event_matches_filter(event, filter) {
                return true;
            }
        }
        false
    }

    fn event_matches_filter(event: &Value, filter: &Value) -> bool {
        // Check kinds
        if let Some(kinds) = filter["kinds"].as_array() {
            let event_kind = event["kind"].as_u64().unwrap_or(0);
            if !kinds.iter().any(|k| k.as_u64() == Some(event_kind)) {
                eprintln!("   ❌ Kind {} not in filter {:?}", event_kind, kinds);
                return false;
            } else {
                eprintln!("   ✓ Kind {} matches filter {:?}", event_kind, kinds);
            }
        }

        // Check authors
        if let Some(authors) = filter["authors"].as_array() {
            let event_pubkey = event["pubkey"].as_str().unwrap_or("");
            if !authors.iter().any(|a| {
                let author = a.as_str().unwrap_or("");
                event_pubkey.starts_with(author) || author.starts_with(event_pubkey) || event_pubkey == author
            }) {
                eprintln!("   ❌ Author {} not in filter", &event_pubkey[..16]);
                return false;
            }
        }

        // Check #p tags
        if let Some(p_values) = filter["#p"].as_array() {
            eprintln!("   🔍 Checking #p filter: {:?}", p_values);
            let event_tags = event["tags"].as_array();
            if event_tags.is_none() {
                eprintln!("   ❌ Event has no tags");
                return false;
            }
            eprintln!("   Event has {} tags", event_tags.unwrap().len());

            let has_matching_p = event_tags.unwrap().iter().any(|tag| {
                if let Some(tag_arr) = tag.as_array() {
                    if tag_arr.len() >= 2 {
                        if tag_arr[0].as_str() == Some("p") {
                            let tag_value = tag_arr[1].as_str().unwrap_or("");
                            return p_values.iter().any(|p| {
                                let filter_p = p.as_str().unwrap_or("");
                                tag_value == filter_p || tag_value.starts_with(filter_p) || filter_p.starts_with(tag_value)
                            });
                        }
                    }
                }
                false
            });

            if !has_matching_p {
                eprintln!("   ❌ No matching p tag");
                return false;
            }
        }

        eprintln!("   ✅ Event matches filter");
        true
    }
}
