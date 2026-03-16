use nostr::{Filter, Kind, PublicKey};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

struct FilterSubscription {
    canonical_subid: String,
    refcount: usize,
}

#[derive(Default)]
struct SubscriptionState {
    by_subid: HashMap<String, String>,
    by_filter: HashMap<String, FilterSubscription>,
}

/// Wrapper that coalesces identical filters so duplicate sessions do not fan out
/// redundant relay subscriptions.
#[derive(Clone)]
pub struct DedupingPubSub {
    inner: Arc<dyn NostrPubSub>,
    subscriptions: Arc<Mutex<SubscriptionState>>,
}

impl DedupingPubSub {
    pub fn new(inner: Arc<dyn NostrPubSub>) -> Self {
        Self {
            inner,
            subscriptions: Arc::new(Mutex::new(SubscriptionState::default())),
        }
    }
}

impl NostrPubSub for DedupingPubSub {
    fn publish(&self, event: nostr::UnsignedEvent) -> Result<()> {
        self.inner.publish(event)
    }

    fn publish_signed(&self, event: nostr::Event) -> Result<()> {
        self.inner.publish_signed(event)
    }

    fn subscribe(&self, subid: String, filter_json: String) -> Result<()> {
        let mut subscriptions = self.subscriptions.lock().unwrap();

        if subscriptions
            .by_subid
            .get(&subid)
            .is_some_and(|existing| existing == &filter_json)
        {
            return Ok(());
        }

        if let Some(existing_filter) = subscriptions.by_subid.remove(&subid) {
            let mut unsubscribe_subid = None;
            let mut remove_filter = false;
            if let Some(entry) = subscriptions.by_filter.get_mut(&existing_filter) {
                if entry.refcount > 1 {
                    entry.refcount -= 1;
                } else {
                    unsubscribe_subid = Some(entry.canonical_subid.clone());
                    remove_filter = true;
                }
            }
            if remove_filter {
                subscriptions.by_filter.remove(&existing_filter);
            }

            if let Some(unsubscribe_subid) = unsubscribe_subid {
                self.inner.unsubscribe(unsubscribe_subid)?;
            }
        }

        if let Some(entry) = subscriptions.by_filter.get_mut(&filter_json) {
            entry.refcount += 1;
            subscriptions.by_subid.insert(subid, filter_json);
            return Ok(());
        }

        self.inner.subscribe(subid.clone(), filter_json.clone())?;
        subscriptions.by_filter.insert(
            filter_json.clone(),
            FilterSubscription {
                canonical_subid: subid.clone(),
                refcount: 1,
            },
        );
        subscriptions.by_subid.insert(subid, filter_json);
        Ok(())
    }

    fn unsubscribe(&self, subid: String) -> Result<()> {
        let mut subscriptions = self.subscriptions.lock().unwrap();

        let Some(filter_json) = subscriptions.by_subid.remove(&subid) else {
            return self.inner.unsubscribe(subid);
        };

        let mut unsubscribe_subid = None;
        let mut remove_filter = false;
        if let Some(entry) = subscriptions.by_filter.get_mut(&filter_json) {
            if entry.refcount > 1 {
                entry.refcount -= 1;
            } else {
                unsubscribe_subid = Some(entry.canonical_subid.clone());
                remove_filter = true;
            }
        }
        if remove_filter {
            subscriptions.by_filter.remove(&filter_json);
        }

        if let Some(unsubscribe_subid) = unsubscribe_subid {
            self.inner.unsubscribe(unsubscribe_subid)?;
        }

        Ok(())
    }

    fn decrypted_message(
        &self,
        sender: PublicKey,
        sender_device: Option<PublicKey>,
        content: String,
        event_id: Option<String>,
    ) -> Result<()> {
        self.inner
            .decrypted_message(sender, sender_device, content, event_id)
    }

    fn received_event(&self, event: nostr::Event) -> Result<()> {
        self.inner.received_event(event)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingPubSub {
        subscribes: Mutex<Vec<(String, String)>>,
        unsubscribes: Mutex<Vec<String>>,
    }

    impl NostrPubSub for RecordingPubSub {
        fn publish(&self, _event: nostr::UnsignedEvent) -> Result<()> {
            Ok(())
        }

        fn publish_signed(&self, _event: nostr::Event) -> Result<()> {
            Ok(())
        }

        fn subscribe(&self, subid: String, filter_json: String) -> Result<()> {
            self.subscribes.lock().unwrap().push((subid, filter_json));
            Ok(())
        }

        fn unsubscribe(&self, subid: String) -> Result<()> {
            self.unsubscribes.lock().unwrap().push(subid);
            Ok(())
        }

        fn decrypted_message(
            &self,
            _sender: PublicKey,
            _sender_device: Option<PublicKey>,
            _content: String,
            _event_id: Option<String>,
        ) -> Result<()> {
            Ok(())
        }

        fn received_event(&self, _event: nostr::Event) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn deduping_pubsub_coalesces_identical_filters_until_last_unsubscribe() {
        let inner = Arc::new(RecordingPubSub::default());
        let pubsub = DedupingPubSub::new(inner.clone());
        let filter_json = "{\"authors\":[\"abc\"],\"kinds\":[1060]}".to_string();

        pubsub
            .subscribe("session-a".to_string(), filter_json.clone())
            .unwrap();
        pubsub
            .subscribe("session-b".to_string(), filter_json)
            .unwrap();

        let subscribes = inner.subscribes.lock().unwrap();
        assert_eq!(subscribes.len(), 1);
        assert_eq!(subscribes[0].0, "session-a");
        drop(subscribes);

        pubsub.unsubscribe("session-b".to_string()).unwrap();
        assert!(inner.unsubscribes.lock().unwrap().is_empty());

        pubsub.unsubscribe("session-a".to_string()).unwrap();
        let unsubscribes = inner.unsubscribes.lock().unwrap();
        assert_eq!(unsubscribes.as_slice(), ["session-a"]);
    }
}
