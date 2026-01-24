use nostr::{Filter, PublicKey, Kind};
use crate::Result;

/// Bidirectional interface for Nostr pub/sub operations.
/// Allows components (Invite, Session) to manage their own subscriptions.
pub trait NostrPubSub: Send + Sync {
    /// Subscribe to events matching the filter. Returns subscription ID.
    fn subscribe(&self, filter: Filter) -> Result<String>;

    /// Unsubscribe from a subscription by ID.
    fn unsubscribe(&self, sub_id: &str) -> Result<()>;

    /// Publish an unsigned event (to be signed by main identity key).
    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()>;

    /// Publish an already-signed event (e.g., with ephemeral keys).
    fn publish_signed(&self, event: nostr::Event) -> Result<()>;
}

/// Event types emitted by SessionManager for external handling.
///
/// SessionManager sends these events to a channel. The receiver is responsible for:
/// - Publishing events to relays (Publish, PublishSigned)
/// - Subscribing to relay filters (Subscribe, Unsubscribe)
/// - Handling decrypted messages (DecryptedMessage, ReceivedEvent)

/// Handler for SessionManager events from a receiver channel.
/// Converts SessionManagerEvent variants into structured data for processing.
pub enum SessionEvent {
    Publish(nostr::UnsignedEvent),
    PublishSigned(nostr::Event),
    Subscribe(String),
    Unsubscribe(String),
    DecryptedMessage {
        sender: PublicKey,
        content: String,
        event_id: Option<String>,
    },
    ReceivedEvent(nostr::Event),
}

/// Channel-based implementation of NostrPubSub for backward compatibility.
/// Bridges the old event_tx channel pattern to the new trait.
pub struct ChannelPubSub {
    event_tx: crossbeam_channel::Sender<crate::SessionManagerEvent>,
}

impl ChannelPubSub {
    pub fn new(event_tx: crossbeam_channel::Sender<crate::SessionManagerEvent>) -> Self {
        Self { event_tx }
    }
}

impl NostrPubSub for ChannelPubSub {
    fn subscribe(&self, filter: Filter) -> Result<String> {
        let filter_json = serde_json::to_string(&filter)?;
        let sub_id = format!("sub-{}", uuid::Uuid::new_v4());
        self.event_tx
            .send(crate::SessionManagerEvent::Subscribe(filter_json))
            .map_err(|_| crate::Error::Storage("Failed to send subscribe".to_string()))?;
        Ok(sub_id)
    }

    fn unsubscribe(&self, sub_id: &str) -> Result<()> {
        self.event_tx
            .send(crate::SessionManagerEvent::Unsubscribe(sub_id.to_string()))
            .map_err(|_| crate::Error::Storage("Failed to send unsubscribe".to_string()))?;
        Ok(())
    }

    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()> {
        self.event_tx
            .send(crate::SessionManagerEvent::Publish(event))
            .map_err(|_| crate::Error::Storage("Failed to send publish".to_string()))?;
        Ok(())
    }

    fn publish_signed(&self, event: nostr::Event) -> Result<()> {
        self.event_tx
            .send(crate::SessionManagerEvent::PublishSigned(event))
            .map_err(|_| crate::Error::Storage("Failed to send publish_signed".to_string()))?;
        Ok(())
    }
}

/// Helper to build filters for this crate
pub fn build_filter() -> FilterBuilder {
    FilterBuilder::new()
}

pub struct FilterBuilder {
    kinds: Vec<Kind>,
    authors: Vec<PublicKey>,
    pubkeys: Vec<PublicKey>,
}

impl FilterBuilder {
    pub fn new() -> Self {
        Self {
            kinds: Vec::new(),
            authors: Vec::new(),
            pubkeys: Vec::new(),
        }
    }

    pub fn kinds(mut self, kinds: Vec<u64>) -> Self {
        self.kinds = kinds.into_iter().map(|k| Kind::from(k as u16)).collect();
        self
    }

    pub fn authors(mut self, authors: Vec<PublicKey>) -> Self {
        self.authors = authors;
        self
    }

    pub fn pubkeys(mut self, pubkeys: Vec<PublicKey>) -> Self {
        self.pubkeys = pubkeys;
        self
    }

    pub fn build(self) -> Filter {
        let mut filter = Filter::new();
        if !self.kinds.is_empty() {
            filter = filter.kinds(self.kinds);
        }
        if !self.authors.is_empty() {
            filter = filter.authors(self.authors);
        }
        if !self.pubkeys.is_empty() {
            filter = filter.pubkeys(self.pubkeys);
        }
        filter
    }
}

impl Default for FilterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Test utilities for receiving and processing SessionManager events.
pub mod test_utils {
    use super::SessionEvent;

    /// Receives SessionManagerEvent from a channel and converts to structured events.
    pub struct SessionEventReceiver {
        rx: crossbeam_channel::Receiver<crate::session_manager::SessionManagerEvent>,
    }

    impl SessionEventReceiver {
        pub fn new(rx: crossbeam_channel::Receiver<crate::session_manager::SessionManagerEvent>) -> Self {
            Self { rx }
        }

        /// Try to receive the next session event (non-blocking)
        pub fn try_recv(&self) -> Option<SessionEvent> {
            self.rx.try_recv().ok().map(|event| match event {
                crate::session_manager::SessionManagerEvent::Publish(unsigned) => SessionEvent::Publish(unsigned),
                crate::session_manager::SessionManagerEvent::PublishSigned(signed) => SessionEvent::PublishSigned(signed),
                crate::session_manager::SessionManagerEvent::Subscribe(filter) => SessionEvent::Subscribe(filter),
                crate::session_manager::SessionManagerEvent::Unsubscribe(subid) => SessionEvent::Unsubscribe(subid),
                crate::session_manager::SessionManagerEvent::DecryptedMessage { sender, content, event_id } => {
                    SessionEvent::DecryptedMessage { sender, content, event_id }
                }
                crate::session_manager::SessionManagerEvent::ReceivedEvent(event) => SessionEvent::ReceivedEvent(event),
            })
        }
    }
}
