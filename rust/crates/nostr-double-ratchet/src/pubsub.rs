use nostr::{Filter, Kind, PublicKey};

use crate::{Error, Result, SessionManagerEvent};

/// Nostr publish/subscribe interface used by the library.
///
/// This lets callers provide their own Nostr event system while keeping
/// the double-ratchet logic inside this crate.
pub trait NostrPubSub: Send + Sync {
    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()>;
    fn publish_signed(&self, event: nostr::Event) -> Result<()>;
    fn subscribe(&self, subid: String, filter_json: String) -> Result<()>;
    fn unsubscribe(&self, subid: String) -> Result<()>;
    fn decrypted_message(
        &self,
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    ) -> Result<()>;
    fn received_event(&self, event: nostr::Event) -> Result<()>;
}

impl NostrPubSub for crossbeam_channel::Sender<SessionManagerEvent> {
    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()> {
        self.send(SessionManagerEvent::Publish(event))
            .map_err(|_| Error::Storage("Failed to send publish".to_string()))
    }

    fn publish_signed(&self, event: nostr::Event) -> Result<()> {
        self.send(SessionManagerEvent::PublishSigned(event))
            .map_err(|_| Error::Storage("Failed to send publish".to_string()))
    }

    fn subscribe(&self, subid: String, filter_json: String) -> Result<()> {
        self.send(SessionManagerEvent::Subscribe { subid, filter_json })
            .map_err(|_| Error::Storage("Failed to send subscribe".to_string()))
    }

    fn unsubscribe(&self, subid: String) -> Result<()> {
        self.send(SessionManagerEvent::Unsubscribe(subid))
            .map_err(|_| Error::Storage("Failed to send unsubscribe".to_string()))
    }

    fn decrypted_message(
        &self,
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    ) -> Result<()> {
        self.send(SessionManagerEvent::DecryptedMessage {
            sender,
            sender_device,
            content,
            event_id,
        })
        .map_err(|_| Error::Storage("Failed to send decrypted message".to_string()))
    }

    fn received_event(&self, event: nostr::Event) -> Result<()> {
        self.send(SessionManagerEvent::ReceivedEvent(event))
            .map_err(|_| Error::Storage("Failed to send received event".to_string()))
    }
}

/// Convenience wrapper around a crossbeam channel sender that implements NostrPubSub.
#[derive(Clone)]
pub struct ChannelPubSub {
    tx: crossbeam_channel::Sender<SessionManagerEvent>,
}

impl ChannelPubSub {
    pub fn new(tx: crossbeam_channel::Sender<SessionManagerEvent>) -> Self {
        Self { tx }
    }

    pub fn sender(&self) -> &crossbeam_channel::Sender<SessionManagerEvent> {
        &self.tx
    }
}

impl NostrPubSub for ChannelPubSub {
    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()> {
        self.tx.publish(event)
    }

    fn publish_signed(&self, event: nostr::Event) -> Result<()> {
        self.tx.publish_signed(event)
    }

    fn subscribe(&self, subid: String, filter_json: String) -> Result<()> {
        self.tx.subscribe(subid, filter_json)
    }

    fn unsubscribe(&self, subid: String) -> Result<()> {
        self.tx.unsubscribe(subid)
    }

    fn decrypted_message(
        &self,
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    ) -> Result<()> {
        self.tx
            .decrypted_message(sender, sender_device, content, event_id)
    }

    fn received_event(&self, event: nostr::Event) -> Result<()> {
        self.tx.received_event(event)
    }
}

/// Event types emitted by SessionManager for external handling.
///
/// SessionManager sends these events to a channel. The receiver is responsible for:
/// - Publishing events to relays (Publish, PublishSigned)
/// - Subscribing to relay filters (Subscribe, Unsubscribe)
/// - Handling decrypted messages (DecryptedMessage, ReceivedEvent)
///
/// Handler for SessionManager events from a receiver channel.
/// Converts SessionManagerEvent variants into structured data for processing.
pub enum SessionEvent {
    Publish(nostr::UnsignedEvent),
    PublishSigned(nostr::Event),
    Subscribe {
        subid: String,
        filter_json: String,
    },
    Unsubscribe(String),
    DecryptedMessage {
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    },
    ReceivedEvent(nostr::Event),
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
        pub fn new(
            rx: crossbeam_channel::Receiver<crate::session_manager::SessionManagerEvent>,
        ) -> Self {
            Self { rx }
        }

        /// Try to receive the next session event (non-blocking)
        pub fn try_recv(&self) -> Option<SessionEvent> {
            self.rx.try_recv().ok().map(|event| match event {
                crate::session_manager::SessionManagerEvent::Publish(unsigned) => {
                    SessionEvent::Publish(unsigned)
                }
                crate::session_manager::SessionManagerEvent::PublishSigned(signed) => {
                    SessionEvent::PublishSigned(signed)
                }
                crate::session_manager::SessionManagerEvent::Subscribe { subid, filter_json } => {
                    SessionEvent::Subscribe { subid, filter_json }
                }
                crate::session_manager::SessionManagerEvent::Unsubscribe(subid) => {
                    SessionEvent::Unsubscribe(subid)
                }
                crate::session_manager::SessionManagerEvent::DecryptedMessage {
                    sender,
                    sender_device,
                    content,
                    event_id,
                } => SessionEvent::DecryptedMessage {
                    sender,
                    sender_device,
                    content,
                    event_id,
                },
                crate::session_manager::SessionManagerEvent::ReceivedEvent(event) => {
                    SessionEvent::ReceivedEvent(event)
                }
            })
        }
    }
}
