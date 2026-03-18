use crate::{
    pubsub::build_filter,
    pubsub::NostrPubSub,
    Error, EventCallback, Result, Session, SessionInitInput, SessionReceiveInput,
    SessionReceiveResult, SessionSendInput, SessionState, Unsubscribe, CHAT_MESSAGE_KIND,
    REACTION_KIND, RECEIPT_KIND, TYPING_KIND,
};
use nostr::PublicKey;
use nostr::{EventBuilder, Keys, Tag, UnsignedEvent};
use std::sync::{Arc, Mutex};

pub struct ManagedSession {
    pub session: Session,
    pub name: String,
    pub(crate) nostr_unsubscribe: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) nostr_next_unsubscribe: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) skipped_subscription: Arc<Mutex<Option<Unsubscribe>>>,
    pub(crate) internal_subscriptions: Arc<Mutex<Vec<EventCallback>>>,
    pub(crate) current_key_subid: Arc<Mutex<Option<String>>>,
    pub(crate) next_key_subid: Arc<Mutex<Option<String>>>,
    pub(crate) pubsub: Option<Arc<dyn NostrPubSub>>,
}

impl ManagedSession {
    pub fn new(state: SessionState, name: String) -> Self {
        Self::from_session(Session::new(state), name)
    }

    pub fn from_session(session: Session, name: String) -> Self {
        Self {
            session,
            name,
            nostr_unsubscribe: Arc::new(Mutex::new(None)),
            nostr_next_unsubscribe: Arc::new(Mutex::new(None)),
            skipped_subscription: Arc::new(Mutex::new(None)),
            internal_subscriptions: Arc::new(Mutex::new(Vec::new())),
            current_key_subid: Arc::new(Mutex::new(None)),
            next_key_subid: Arc::new(Mutex::new(None)),
            pubsub: None,
        }
    }

    pub fn init(
        their_ephemeral_nostr_public_key: PublicKey,
        our_ephemeral_nostr_private_key: [u8; 32],
        is_initiator: bool,
        shared_secret: [u8; 32],
        name: Option<String>,
    ) -> Result<Self> {
        let our_next_private_key = Keys::generate().secret_key().to_secret_bytes();
        let session = Session::init(SessionInitInput {
            their_ephemeral_nostr_public_key,
            our_ephemeral_nostr_private_key,
            our_next_nostr_private_key: our_next_private_key,
            is_initiator,
            shared_secret,
        })?;
        Ok(Self::from_session(
            session,
            name.unwrap_or_else(|| "session".to_string()),
        ))
    }

    pub fn set_event_tx(
        &mut self,
        event_tx: crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) {
        let pubsub: Arc<dyn NostrPubSub> = Arc::new(event_tx);
        self.pubsub = Some(pubsub);
    }

    pub fn set_pubsub(&mut self, pubsub: Arc<dyn NostrPubSub>) {
        self.pubsub = Some(pubsub);
    }

    pub fn subscribe_to_messages(&mut self) -> Result<()> {
        if let Some(ref pubsub) = self.pubsub {
            if let Some(current_pk) = self.session.state.their_current_nostr_public_key {
                let mut current_key_subid = self.current_key_subid.lock().unwrap();
                if current_key_subid.is_none() {
                    let filter = build_filter()
                        .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                        .authors(vec![current_pk])
                        .build();

                    let filter_json = serde_json::to_string(&filter)?;
                    let subid = format!("session-current-{}", uuid::Uuid::new_v4());

                    pubsub.subscribe(subid.clone(), filter_json)?;
                    *current_key_subid = Some(subid);
                }
            }

            if let Some(next_pk) = self.session.state.their_next_nostr_public_key {
                let mut next_key_subid = self.next_key_subid.lock().unwrap();
                if next_key_subid.is_none() {
                    let filter = build_filter()
                        .kinds(vec![crate::MESSAGE_EVENT_KIND as u64])
                        .authors(vec![next_pk])
                        .build();

                    let filter_json = serde_json::to_string(&filter)?;
                    let subid = format!("session-next-{}", uuid::Uuid::new_v4());

                    pubsub.subscribe(subid.clone(), filter_json)?;
                    *next_key_subid = Some(subid);
                }
            }
        }

        Ok(())
    }

    pub fn update_subscriptions(&mut self) -> Result<()> {
        if let Some(pubsub) = &self.pubsub {
            if let Some(old_subid) = self.current_key_subid.lock().unwrap().take() {
                let _ = pubsub.unsubscribe(old_subid);
            }
            if let Some(old_subid) = self.next_key_subid.lock().unwrap().take() {
                let _ = pubsub.unsubscribe(old_subid);
            }
        }

        self.subscribe_to_messages()
    }

    pub fn can_send(&self) -> bool {
        self.session.can_send()
    }

    pub fn send(&mut self, text: String) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();
        self.send_event(EventBuilder::text_note(text).build(dummy_keys.public_key()))
    }

    pub fn send_reaction(&mut self, message_id: &str, emoji: &str) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let event = EventBuilder::new(nostr::Kind::from(REACTION_KIND as u16), emoji)
            .tag(
                Tag::parse(&["e".to_string(), message_id.to_string()])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            )
            .build(dummy_keys.public_key());

        self.send_event(event)
    }

    pub fn send_reply(&mut self, text: String, reply_to: &str) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let event = EventBuilder::new(nostr::Kind::from(CHAT_MESSAGE_KIND as u16), &text)
            .tag(
                Tag::parse(&["e".to_string(), reply_to.to_string()])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            )
            .build(dummy_keys.public_key());

        self.send_event(event)
    }

    pub fn send_receipt(
        &mut self,
        receipt_type: &str,
        message_ids: &[&str],
    ) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();

        let mut builder = EventBuilder::new(nostr::Kind::from(RECEIPT_KIND as u16), receipt_type);
        for id in message_ids {
            builder = builder.tag(
                Tag::parse(&["e".to_string(), id.to_string()])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            );
        }

        self.send_event(builder.build(dummy_keys.public_key()))
    }

    pub fn send_typing(&mut self) -> Result<nostr::Event> {
        let dummy_keys = Keys::generate();
        let event =
            EventBuilder::new(nostr::Kind::from(TYPING_KIND as u16), "typing").build(dummy_keys.public_key());
        self.send_event(event)
    }

    pub fn send_event(&mut self, event: UnsignedEvent) -> Result<nostr::Event> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let output = self.session.send_event(SessionSendInput {
            inner_event: event,
            now_secs: now.as_secs(),
            now_ms: now.as_millis() as u64,
        })?;
        self.session = output.next;
        Ok(output.outer_event)
    }

    pub fn receive(&mut self, event: &nostr::Event) -> Result<Option<String>> {
        let replacement_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
        let before = self.session.state.clone();
        match self.session.receive_event(SessionReceiveInput {
            outer_event: event.clone(),
            replacement_next_nostr_private_key,
        }) {
            SessionReceiveResult::NotForThisSession { .. } => Ok(None),
            SessionReceiveResult::Decrypted {
                next,
                plaintext,
                ..
            } => {
                let should_update = session_keys_changed(&before, &next.state);
                self.session = next;
                if should_update {
                    let _ = self.update_subscriptions();
                }
                Ok(Some(plaintext))
            }
            SessionReceiveResult::InvalidRelevant { error, .. } => Err(error),
        }
    }

    pub fn close(&self) {
        if let Some(unsub) = self.nostr_unsubscribe.lock().unwrap().take() {
            unsub();
        }
        if let Some(unsub) = self.nostr_next_unsubscribe.lock().unwrap().take() {
            unsub();
        }
        if let Some(unsub) = self.skipped_subscription.lock().unwrap().take() {
            unsub();
        }
        self.internal_subscriptions.lock().unwrap().clear();

        if let Some(pubsub) = &self.pubsub {
            if let Some(subid) = self.current_key_subid.lock().unwrap().take() {
                let _ = pubsub.unsubscribe(subid);
            }
            if let Some(subid) = self.next_key_subid.lock().unwrap().take() {
                let _ = pubsub.unsubscribe(subid);
            }
        }
    }
}

fn session_keys_changed(before: &SessionState, after: &SessionState) -> bool {
    before.their_current_nostr_public_key != after.their_current_nostr_public_key
        || before.their_next_nostr_public_key != after.their_next_nostr_public_key
        || before.our_current_nostr_key.as_ref().map(|k| k.public_key)
            != after.our_current_nostr_key.as_ref().map(|k| k.public_key)
        || before.our_next_nostr_key.public_key != after.our_next_nostr_key.public_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SerializableKeyPair, SessionManagerEvent};

    #[test]
    fn subscribe_to_messages_is_idempotent_until_close() {
        let their_current = Keys::generate().public_key();
        let their_next = Keys::generate().public_key();
        let our_current = Keys::generate();
        let our_next = Keys::generate();

        let mut session = ManagedSession::new(
            SessionState {
                root_key: [0u8; 32],
                their_current_nostr_public_key: Some(their_current),
                their_next_nostr_public_key: Some(their_next),
                our_current_nostr_key: Some(SerializableKeyPair {
                    public_key: our_current.public_key(),
                    private_key: our_current.secret_key().to_secret_bytes(),
                }),
                our_next_nostr_key: SerializableKeyPair {
                    public_key: our_next.public_key(),
                    private_key: our_next.secret_key().to_secret_bytes(),
                },
                receiving_chain_key: Some([1u8; 32]),
                sending_chain_key: Some([2u8; 32]),
                sending_chain_message_number: 0,
                receiving_chain_message_number: 0,
                previous_sending_chain_message_count: 0,
                skipped_keys: std::collections::HashMap::new(),
            },
            "test".to_string(),
        );

        let (tx, rx) = crossbeam_channel::unbounded();
        session.set_event_tx(tx);

        session.subscribe_to_messages().unwrap();
        session.subscribe_to_messages().unwrap();

        let subscribe_count = rx
            .try_iter()
            .filter(|event| matches!(event, SessionManagerEvent::Subscribe { .. }))
            .count();
        assert_eq!(subscribe_count, 2);

        session.close();

        let unsubscribe_count = rx
            .try_iter()
            .filter(|event| matches!(event, SessionManagerEvent::Unsubscribe(_)))
            .count();
        assert_eq!(unsubscribe_count, 2);
    }
}
