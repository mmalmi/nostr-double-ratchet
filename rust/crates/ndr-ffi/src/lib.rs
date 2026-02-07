//! UniFFI bindings for nostr-double-ratchet
//!
//! This crate provides FFI-friendly wrappers around the core nostr-double-ratchet
//! library for use in iOS and Android applications via UniFFI.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::Receiver;
use nostr_double_ratchet::{
    AppKeys, DeviceEntry, FileStorageAdapter, Invite, Session, SessionManager, SessionManagerEvent,
    SessionState, StorageAdapter,
};

mod error;
pub use error::NdrError;

/// Returns the version of the ndr-ffi crate.
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// FFI-friendly keypair with hex-encoded keys.
#[derive(uniffi::Record)]
pub struct FfiKeyPair {
    pub public_key_hex: String,
    pub private_key_hex: String,
}

/// Result of accepting an invite.
#[derive(uniffi::Record)]
pub struct InviteAcceptResult {
    pub session: Arc<SessionHandle>,
    pub response_event_json: String,
}

/// Result of sending a message.
#[derive(uniffi::Record)]
pub struct SendResult {
    pub outer_event_json: String,
    pub inner_event_json: String,
}

/// Result of decrypting a message.
#[derive(uniffi::Record)]
pub struct DecryptResult {
    pub plaintext: String,
    pub inner_event_json: String,
}

/// Event emitted by SessionManager for external publish/subscribe handling.
#[derive(uniffi::Record)]
pub struct PubSubEvent {
    pub kind: String,
    pub subid: Option<String>,
    pub filter_json: Option<String>,
    pub event_json: Option<String>,
    pub sender_pubkey_hex: Option<String>,
    pub content: Option<String>,
    pub event_id: Option<String>,
}

/// Generate a new keypair.
#[uniffi::export]
pub fn generate_keypair() -> FfiKeyPair {
    let keys = nostr::Keys::generate();
    FfiKeyPair {
        public_key_hex: keys.public_key().to_hex(),
        private_key_hex: keys.secret_key().to_secret_hex(),
    }
}

/// Derive a public key from a hex-encoded private key.
#[uniffi::export]
pub fn derive_public_key(privkey_hex: String) -> Result<String, NdrError> {
    let privkey = parse_private_key(&privkey_hex)?;
    let secret_key =
        nostr::SecretKey::from_slice(&privkey).map_err(|e| NdrError::InvalidKey(e.to_string()))?;
    Ok(nostr::Keys::new(secret_key).public_key().to_hex())
}

/// FFI-friendly device entry for AppKeys.
#[derive(uniffi::Record)]
pub struct FfiDeviceEntry {
    pub identity_pubkey_hex: String,
    pub created_at: u64,
}

/// Create a signed AppKeys event JSON for publishing to relays.
#[uniffi::export]
pub fn create_signed_app_keys_event(
    owner_pubkey_hex: String,
    owner_privkey_hex: String,
    devices: Vec<FfiDeviceEntry>,
) -> Result<String, NdrError> {
    let owner_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&owner_pubkey_hex)?;
    let owner_privkey = parse_private_key(&owner_privkey_hex)?;
    let owner_sk = nostr::SecretKey::from_slice(&owner_privkey)
        .map_err(|e| NdrError::InvalidKey(e.to_string()))?;

    let entries = devices
        .into_iter()
        .filter_map(|d| {
            let pk = nostr_double_ratchet::utils::pubkey_from_hex(&d.identity_pubkey_hex).ok()?;
            Some(DeviceEntry::new(pk, d.created_at))
        })
        .collect::<Vec<_>>();

    let app_keys = AppKeys::new(entries);
    let unsigned = app_keys.get_event(owner_pubkey);
    let keys = nostr::Keys::new(owner_sk);
    let signed = unsigned
        .sign_with_keys(&keys)
        .map_err(|e| NdrError::Serialization(e.to_string()))?;
    Ok(serde_json::to_string(&signed)?)
}

/// Parse an AppKeys event JSON and return the contained device entries.
#[uniffi::export]
pub fn parse_app_keys_event(event_json: String) -> Result<Vec<FfiDeviceEntry>, NdrError> {
    let event: nostr::Event = serde_json::from_str(&event_json)?;
    let app_keys = AppKeys::from_event(&event)?;
    Ok(app_keys
        .get_all_devices()
        .into_iter()
        .map(|d| FfiDeviceEntry {
            identity_pubkey_hex: hex::encode(d.identity_pubkey.to_bytes()),
            created_at: d.created_at,
        })
        .collect())
}

/// FFI wrapper for Invite.
#[derive(uniffi::Object)]
pub struct InviteHandle {
    inner: Mutex<Invite>,
}

#[uniffi::export]
impl InviteHandle {
    /// Create a new invite.
    #[uniffi::constructor]
    pub fn create_new(
        inviter_pubkey_hex: String,
        device_id: Option<String>,
        max_uses: Option<u32>,
    ) -> Result<Arc<Self>, NdrError> {
        let inviter = nostr_double_ratchet::utils::pubkey_from_hex(&inviter_pubkey_hex)?;
        let invite = Invite::create_new(inviter, device_id, max_uses.map(|n| n as usize))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(invite),
        }))
    }

    /// Parse an invite from a URL.
    #[uniffi::constructor]
    pub fn from_url(url: String) -> Result<Arc<Self>, NdrError> {
        let invite = Invite::from_url(&url)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(invite),
        }))
    }

    /// Parse an invite from a Nostr event JSON.
    #[uniffi::constructor]
    pub fn from_event_json(event_json: String) -> Result<Arc<Self>, NdrError> {
        let event: nostr::Event = serde_json::from_str(&event_json)?;
        let invite = Invite::from_event(&event)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(invite),
        }))
    }

    /// Deserialize an invite from JSON.
    #[uniffi::constructor]
    pub fn deserialize(json: String) -> Result<Arc<Self>, NdrError> {
        let invite = Invite::deserialize(&json)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(invite),
        }))
    }

    /// Convert the invite to a shareable URL.
    pub fn to_url(&self, root: String) -> Result<String, NdrError> {
        let invite = self.inner.lock().unwrap();
        Ok(invite.get_url(&root)?)
    }

    /// Convert the invite to a Nostr event JSON.
    pub fn to_event_json(&self) -> Result<String, NdrError> {
        let invite = self.inner.lock().unwrap();
        let event = invite.get_event()?;
        Ok(serde_json::to_string(&event)?)
    }

    /// Serialize the invite to JSON for persistence.
    pub fn serialize(&self) -> Result<String, NdrError> {
        let invite = self.inner.lock().unwrap();
        Ok(invite.serialize()?)
    }

    /// Accept the invite and create a session.
    pub fn accept(
        &self,
        invitee_pubkey_hex: String,
        invitee_privkey_hex: String,
        device_id: Option<String>,
    ) -> Result<InviteAcceptResult, NdrError> {
        let invite = self.inner.lock().unwrap();
        let invitee_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&invitee_pubkey_hex)?;
        let invitee_privkey = parse_private_key(&invitee_privkey_hex)?;

        let (session, response_event) =
            invite.accept(invitee_pubkey, invitee_privkey, device_id)?;
        let response_event_json = serde_json::to_string(&response_event)?;

        Ok(InviteAcceptResult {
            session: Arc::new(SessionHandle {
                inner: Mutex::new(session),
            }),
            response_event_json,
        })
    }

    /// Accept the invite as an owner and include the owner pubkey in the response payload.
    pub fn accept_with_owner(
        &self,
        invitee_pubkey_hex: String,
        invitee_privkey_hex: String,
        device_id: Option<String>,
        owner_pubkey_hex: Option<String>,
    ) -> Result<InviteAcceptResult, NdrError> {
        let invite = self.inner.lock().unwrap();
        let invitee_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&invitee_pubkey_hex)?;
        let invitee_privkey = parse_private_key(&invitee_privkey_hex)?;
        let owner_pubkey = match owner_pubkey_hex {
            Some(h) => Some(nostr_double_ratchet::utils::pubkey_from_hex(&h)?),
            None => None,
        };

        let (session, response_event) =
            invite.accept_with_owner(invitee_pubkey, invitee_privkey, device_id, owner_pubkey)?;
        let response_event_json = serde_json::to_string(&response_event)?;

        Ok(InviteAcceptResult {
            session: Arc::new(SessionHandle {
                inner: Mutex::new(session),
            }),
            response_event_json,
        })
    }

    /// Update the invite purpose (e.g. \"link\").
    pub fn set_purpose(&self, purpose: Option<String>) {
        let mut invite = self.inner.lock().unwrap();
        invite.purpose = purpose;
    }

    /// Update the owner pubkey embedded in invite URLs.
    pub fn set_owner_pubkey_hex(&self, owner_pubkey_hex: Option<String>) -> Result<(), NdrError> {
        let mut invite = self.inner.lock().unwrap();
        invite.owner_public_key = match owner_pubkey_hex {
            Some(h) => Some(nostr_double_ratchet::utils::pubkey_from_hex(&h)?),
            None => None,
        };
        Ok(())
    }

    /// Process an invite response event and create a session (inviter side).
    ///
    /// Returns `None` if the event is not a valid response for this invite.
    pub fn process_response(
        &self,
        event_json: String,
        inviter_privkey_hex: String,
    ) -> Result<Option<InviteProcessResult>, NdrError> {
        let invite = self.inner.lock().unwrap();
        let event: nostr::Event = serde_json::from_str(&event_json)?;
        let inviter_privkey = parse_private_key(&inviter_privkey_hex)?;

        let response = invite.process_invite_response(&event, inviter_privkey)?;
        let Some(response) = response else {
            return Ok(None);
        };

        Ok(Some(InviteProcessResult {
            session: Arc::new(SessionHandle {
                inner: Mutex::new(response.session),
            }),
            invitee_pubkey_hex: response.invitee_identity.to_hex(),
            device_id: response.device_id,
            owner_pubkey_hex: response.owner_public_key.map(|pk| pk.to_hex()),
        }))
    }

    /// Get the inviter's public key as hex.
    pub fn get_inviter_pubkey_hex(&self) -> String {
        let invite = self.inner.lock().unwrap();
        invite.inviter.to_hex()
    }

    /// Get the shared secret as hex.
    pub fn get_shared_secret_hex(&self) -> String {
        let invite = self.inner.lock().unwrap();
        hex::encode(invite.shared_secret)
    }
}

/// FFI wrapper for Session.
#[derive(uniffi::Object)]
pub struct SessionHandle {
    inner: Mutex<Session>,
}

#[uniffi::export]
impl SessionHandle {
    /// Initialize a new session.
    #[uniffi::constructor]
    pub fn init(
        their_ephemeral_pubkey_hex: String,
        our_ephemeral_privkey_hex: String,
        is_initiator: bool,
        shared_secret_hex: String,
        name: Option<String>,
    ) -> Result<Arc<Self>, NdrError> {
        let their_pubkey =
            nostr_double_ratchet::utils::pubkey_from_hex(&their_ephemeral_pubkey_hex)?;
        let our_privkey = parse_private_key(&our_ephemeral_privkey_hex)?;
        let shared_secret = parse_secret(&shared_secret_hex)?;

        let session = Session::init(their_pubkey, our_privkey, is_initiator, shared_secret, name)?;

        Ok(Arc::new(Self {
            inner: Mutex::new(session),
        }))
    }

    /// Restore a session from serialized state JSON.
    #[uniffi::constructor]
    pub fn from_state_json(state_json: String) -> Result<Arc<Self>, NdrError> {
        let state: SessionState =
            nostr_double_ratchet::utils::deserialize_session_state(&state_json)?;
        let session = Session::new(state, "restored".to_string());

        Ok(Arc::new(Self {
            inner: Mutex::new(session),
        }))
    }

    /// Serialize the session state to JSON.
    pub fn state_json(&self) -> Result<String, NdrError> {
        let session = self.inner.lock().unwrap();
        Ok(nostr_double_ratchet::utils::serialize_session_state(
            &session.state,
        )?)
    }

    /// Check if the session is ready to send messages.
    pub fn can_send(&self) -> bool {
        let session = self.inner.lock().unwrap();
        session.can_send()
    }

    /// Send a text message.
    pub fn send_text(&self, text: String) -> Result<SendResult, NdrError> {
        let mut session = self.inner.lock().unwrap();
        let outer_event = session.send(text.clone())?;

        // Create inner event representation
        let inner_event = nostr::EventBuilder::text_note(text);
        let inner_event_json =
            serde_json::to_string(&inner_event.build(nostr::Keys::generate().public_key()))?;

        Ok(SendResult {
            outer_event_json: serde_json::to_string(&outer_event)?,
            inner_event_json,
        })
    }

    /// Decrypt a received event.
    pub fn decrypt_event(&self, outer_event_json: String) -> Result<DecryptResult, NdrError> {
        let mut session = self.inner.lock().unwrap();
        let event: nostr::Event = serde_json::from_str(&outer_event_json)?;

        let plaintext = session.receive(&event)?.unwrap_or_default();

        // Try to parse inner event if it's JSON
        let inner_event_json = if plaintext.starts_with('{') {
            plaintext.clone()
        } else {
            // Wrap plain text in a simple structure
            serde_json::json!({
                "content": plaintext
            })
            .to_string()
        };

        Ok(DecryptResult {
            plaintext,
            inner_event_json,
        })
    }

    /// Check if an event is a double-ratchet message.
    pub fn is_dr_message(&self, event_json: String) -> bool {
        if let Ok(event) = serde_json::from_str::<nostr::Event>(&event_json) {
            event.kind == nostr::Kind::Custom(nostr_double_ratchet::MESSAGE_EVENT_KIND as u16)
        } else {
            false
        }
    }
}

/// FFI wrapper for SessionManager.
#[derive(uniffi::Object)]
pub struct SessionManagerHandle {
    inner: Mutex<SessionManager>,
    event_rx: Mutex<Receiver<SessionManagerEvent>>,
}

#[uniffi::export]
impl SessionManagerHandle {
    /// Create a new session manager with an internal event queue.
    #[uniffi::constructor]
    pub fn new(
        our_pubkey_hex: String,
        our_identity_privkey_hex: String,
        device_id: String,
        owner_pubkey_hex: Option<String>,
    ) -> Result<Arc<Self>, NdrError> {
        let our_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&our_pubkey_hex)?;
        let our_identity_key = parse_private_key(&our_identity_privkey_hex)?;
        let owner_pubkey = match owner_pubkey_hex {
            Some(h) => nostr_double_ratchet::utils::pubkey_from_hex(&h)?,
            None => our_pubkey,
        };

        let (tx, rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
        let manager = SessionManager::new(
            our_pubkey,
            our_identity_key,
            device_id,
            owner_pubkey,
            tx,
            None,
            None,
        );

        Ok(Arc::new(Self {
            inner: Mutex::new(manager),
            event_rx: Mutex::new(rx),
        }))
    }

    /// Create a new session manager with file-backed storage.
    #[uniffi::constructor]
    pub fn new_with_storage_path(
        our_pubkey_hex: String,
        our_identity_privkey_hex: String,
        device_id: String,
        storage_path: String,
        owner_pubkey_hex: Option<String>,
    ) -> Result<Arc<Self>, NdrError> {
        let our_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&our_pubkey_hex)?;
        let our_identity_key = parse_private_key(&our_identity_privkey_hex)?;
        let owner_pubkey = match owner_pubkey_hex {
            Some(h) => nostr_double_ratchet::utils::pubkey_from_hex(&h)?,
            None => our_pubkey,
        };

        let storage = FileStorageAdapter::new(std::path::PathBuf::from(storage_path))
            .map_err(NdrError::from)?;

        let (tx, rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
        let manager = SessionManager::new(
            our_pubkey,
            our_identity_key,
            device_id,
            owner_pubkey,
            tx,
            Some(Arc::new(storage) as Arc<dyn StorageAdapter>),
            None,
        );

        Ok(Arc::new(Self {
            inner: Mutex::new(manager),
            event_rx: Mutex::new(rx),
        }))
    }

    /// Initialize the session manager (loads state, creates device invite, subscribes).
    pub fn init(&self) -> Result<(), NdrError> {
        let manager = self.inner.lock().unwrap();
        manager.init()?;
        Ok(())
    }

    /// Send a text message to a recipient.
    pub fn send_text(
        &self,
        recipient_pubkey_hex: String,
        text: String,
        expires_at_seconds: Option<u64>,
    ) -> Result<Vec<String>, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        let options = expires_at_seconds.map(|expires_at| nostr_double_ratchet::SendOptions {
            expires_at: Some(expires_at),
        });
        Ok(manager.send_text(recipient, text, options)?)
    }

    /// Send a text message and return both the stable inner (rumor) id and the
    /// list of outer message event ids that were published.
    pub fn send_text_with_inner_id(
        &self,
        recipient_pubkey_hex: String,
        text: String,
        expires_at_seconds: Option<u64>,
    ) -> Result<SendTextResult, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        let options = expires_at_seconds.map(|expires_at| nostr_double_ratchet::SendOptions {
            expires_at: Some(expires_at),
        });
        let (inner_id, outer_event_ids) = manager.send_text_with_inner_id(recipient, text, options)?;
        Ok(SendTextResult {
            inner_id,
            outer_event_ids,
        })
    }

    /// Send an arbitrary inner rumor event to a recipient, returning stable inner id + outer ids.
    ///
    /// This is used for group chats where we need custom kinds/tags (e.g. group metadata kind 40,
    /// group-tagged chat messages kind 14, reactions kind 7, typing kind 25).
    ///
    /// The caller controls the inner rumor tags via `tags_json` (JSON array of string arrays).
    /// For group fan-out, do NOT include recipient-specific tags like `["p", <recipient>]` so
    /// the inner rumor id stays stable across all recipients.
    pub fn send_event_with_inner_id(
        &self,
        recipient_pubkey_hex: String,
        kind: u32,
        content: String,
        tags_json: String,
        created_at_seconds: Option<u64>,
    ) -> Result<SendTextResult, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;

        // Parse tags from JSON (array of string arrays).
        let tags_vec: Vec<Vec<String>> = if tags_json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&tags_json)?
        };

        // Try to reuse an explicit ms tag as the created_at source when the caller didn't
        // provide created_at_seconds (helps keep ids stable across repeated fan-out calls).
        let mut ms_value: Option<u64> = None;
        for t in tags_vec.iter() {
            if t.first().map(|s| s.as_str()) != Some("ms") {
                continue;
            }
            if let Some(v) = t.get(1) {
                ms_value = v.parse::<u64>().ok();
                break;
            }
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let now_s = now.as_secs();
        let now_ms = now.as_millis() as u64;

        let created_at_s = created_at_seconds
            .or_else(|| ms_value.map(|ms| ms / 1000))
            .unwrap_or(now_s);

        let mut tags: Vec<nostr::Tag> = Vec::with_capacity(tags_vec.len() + 1);
        let mut has_ms = false;

        for t in tags_vec {
            if t.first().map(|s| s.as_str()) == Some("ms") {
                has_ms = true;
            }
            tags.push(
                nostr::Tag::parse(&t)
                    .map_err(|e| NdrError::InvalidEvent(e.to_string()))?,
            );
        }

        if !has_ms {
            tags.push(
                nostr::Tag::parse(&["ms".to_string(), now_ms.to_string()])
                    .map_err(|e| NdrError::InvalidEvent(e.to_string()))?,
            );
        }

        let kind_u16: u16 = kind
            .try_into()
            .map_err(|_| NdrError::InvalidEvent("kind out of range".into()))?;

        let manager = self.inner.lock().unwrap();
        let owner_pubkey = manager.get_owner_pubkey();

        let mut event = nostr::EventBuilder::new(nostr::Kind::from(kind_u16), &content)
            .tags(tags)
            .custom_created_at(nostr::Timestamp::from(created_at_s))
            .build(owner_pubkey);

        event.ensure_id();
        let inner_id = event
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();

        let outer_event_ids = manager.send_event(recipient, event)?;

        Ok(SendTextResult {
            inner_id,
            outer_event_ids,
        })
    }

    /// Send a delivery/read receipt for messages.
    pub fn send_receipt(
        &self,
        recipient_pubkey_hex: String,
        receipt_type: String,
        message_ids: Vec<String>,
        expires_at_seconds: Option<u64>,
    ) -> Result<Vec<String>, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        let options = expires_at_seconds.map(|expires_at| nostr_double_ratchet::SendOptions {
            expires_at: Some(expires_at),
        });
        Ok(manager.send_receipt(recipient, &receipt_type, message_ids, options)?)
    }

    /// Send a typing indicator.
    pub fn send_typing(
        &self,
        recipient_pubkey_hex: String,
        expires_at_seconds: Option<u64>,
    ) -> Result<Vec<String>, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        let options = expires_at_seconds.map(|expires_at| nostr_double_ratchet::SendOptions {
            expires_at: Some(expires_at),
        });
        Ok(manager.send_typing(recipient, options)?)
    }

    /// Send an emoji reaction (kind 7) to a specific message id.
    pub fn send_reaction(
        &self,
        recipient_pubkey_hex: String,
        message_id: String,
        emoji: String,
        expires_at_seconds: Option<u64>,
    ) -> Result<Vec<String>, NdrError> {
        let recipient = nostr_double_ratchet::utils::pubkey_from_hex(&recipient_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        let options = expires_at_seconds.map(|expires_at| nostr_double_ratchet::SendOptions {
            expires_at: Some(expires_at),
        });
        Ok(manager.send_reaction(recipient, message_id, emoji, options)?)
    }

    /// Import a session state for a peer.
    pub fn import_session_state(
        &self,
        peer_pubkey_hex: String,
        state_json: String,
        device_id: Option<String>,
    ) -> Result<(), NdrError> {
        let peer_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&peer_pubkey_hex)?;
        let state: SessionState =
            nostr_double_ratchet::utils::deserialize_session_state(&state_json)?;
        let manager = self.inner.lock().unwrap();
        manager.import_session_state(peer_pubkey, device_id, state)?;
        Ok(())
    }

    /// Export the active session state for a peer.
    pub fn get_active_session_state(
        &self,
        peer_pubkey_hex: String,
    ) -> Result<Option<String>, NdrError> {
        let peer_pubkey = nostr_double_ratchet::utils::pubkey_from_hex(&peer_pubkey_hex)?;
        let manager = self.inner.lock().unwrap();
        if let Some(state) = manager.export_active_session_state(peer_pubkey)? {
            Ok(Some(nostr_double_ratchet::utils::serialize_session_state(
                &state,
            )?))
        } else {
            Ok(None)
        }
    }

    /// Process a received Nostr event JSON.
    pub fn process_event(&self, event_json: String) -> Result<(), NdrError> {
        let event: nostr::Event = serde_json::from_str(&event_json)?;
        let manager = self.inner.lock().unwrap();
        manager.process_received_event(event);
        Ok(())
    }

    /// Drain pending pubsub events from the internal queue.
    pub fn drain_events(&self) -> Result<Vec<PubSubEvent>, NdrError> {
        let rx = self.event_rx.lock().unwrap();
        let mut events = Vec::new();

        loop {
            match rx.try_recv() {
                Ok(event) => {
                    let pubsub_event = match event {
                        SessionManagerEvent::Publish(unsigned) => PubSubEvent {
                            kind: "publish".to_string(),
                            subid: None,
                            filter_json: None,
                            event_json: Some(serde_json::to_string(&unsigned)?),
                            sender_pubkey_hex: None,
                            content: None,
                            event_id: None,
                        },
                        SessionManagerEvent::PublishSigned(signed) => PubSubEvent {
                            kind: "publish_signed".to_string(),
                            subid: None,
                            filter_json: None,
                            event_json: Some(serde_json::to_string(&signed)?),
                            sender_pubkey_hex: None,
                            content: None,
                            event_id: None,
                        },
                        SessionManagerEvent::Subscribe { subid, filter_json } => PubSubEvent {
                            kind: "subscribe".to_string(),
                            subid: Some(subid),
                            filter_json: Some(filter_json),
                            event_json: None,
                            sender_pubkey_hex: None,
                            content: None,
                            event_id: None,
                        },
                        SessionManagerEvent::Unsubscribe(subid) => PubSubEvent {
                            kind: "unsubscribe".to_string(),
                            subid: Some(subid),
                            filter_json: None,
                            event_json: None,
                            sender_pubkey_hex: None,
                            content: None,
                            event_id: None,
                        },
                        SessionManagerEvent::DecryptedMessage {
                            sender,
                            content,
                            event_id,
                        } => PubSubEvent {
                            kind: "decrypted_message".to_string(),
                            subid: None,
                            filter_json: None,
                            event_json: None,
                            sender_pubkey_hex: Some(sender.to_hex()),
                            content: Some(content),
                            event_id,
                        },
                        SessionManagerEvent::ReceivedEvent(event) => PubSubEvent {
                            kind: "received_event".to_string(),
                            subid: None,
                            filter_json: None,
                            event_json: Some(serde_json::to_string(&event)?),
                            sender_pubkey_hex: None,
                            content: None,
                            event_id: None,
                        },
                    };
                    events.push(pubsub_event);
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }

        Ok(events)
    }

    /// Get our device id.
    pub fn get_device_id(&self) -> String {
        let manager = self.inner.lock().unwrap();
        manager.get_device_id().to_string()
    }

    /// Get our public key as hex.
    pub fn get_our_pubkey_hex(&self) -> String {
        let manager = self.inner.lock().unwrap();
        manager.get_our_pubkey().to_hex()
    }

    /// Get owner public key as hex.
    pub fn get_owner_pubkey_hex(&self) -> String {
        let manager = self.inner.lock().unwrap();
        manager.get_owner_pubkey().to_hex()
    }

    /// Get total active sessions.
    pub fn get_total_sessions(&self) -> u64 {
        let manager = self.inner.lock().unwrap();
        manager.get_total_sessions() as u64
    }
}

/// Parse a hex-encoded private key.
fn parse_private_key(hex_str: &str) -> Result<[u8; 32], NdrError> {
    let bytes = hex::decode(hex_str).map_err(|_| NdrError::InvalidKey("Invalid hex".into()))?;
    if bytes.len() != 32 {
        return Err(NdrError::InvalidKey("Private key must be 32 bytes".into()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Parse a hex-encoded secret.
fn parse_secret(hex_str: &str) -> Result<[u8; 32], NdrError> {
    let bytes = hex::decode(hex_str).map_err(|_| NdrError::Serialization("Invalid hex".into()))?;
    if bytes.len() != 32 {
        return Err(NdrError::Serialization("Secret must be 32 bytes".into()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

uniffi::setup_scaffolding!();

/// Result of processing an invite response.
#[derive(uniffi::Record)]
pub struct InviteProcessResult {
    pub session: Arc<SessionHandle>,
    pub invitee_pubkey_hex: String,
    pub device_id: Option<String>,
    pub owner_pubkey_hex: Option<String>,
}

/// Result of sending a text message including stable inner id.
#[derive(uniffi::Record)]
pub struct SendTextResult {
    pub inner_id: String,
    pub outer_event_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        let v = version();
        assert!(!v.is_empty());
        assert!(v.contains('.'));
    }

    #[test]
    fn test_keypair_generate_formats_hex() {
        let kp = generate_keypair();
        assert_eq!(kp.public_key_hex.len(), 64);
        assert_eq!(kp.private_key_hex.len(), 64);
        // Verify they're valid hex
        assert!(hex::decode(&kp.public_key_hex).is_ok());
        assert!(hex::decode(&kp.private_key_hex).is_ok());
    }

    #[test]
    fn test_derive_public_key_matches_generate() {
        let kp = generate_keypair();
        let pubkey = derive_public_key(kp.private_key_hex.clone()).unwrap();
        assert_eq!(pubkey, kp.public_key_hex);
    }

    #[test]
    fn test_invite_url_roundtrip() {
        let kp = generate_keypair();
        let invite = InviteHandle::create_new(kp.public_key_hex.clone(), None, None).unwrap();

        let url = invite.to_url("https://example.com".to_string()).unwrap();
        assert!(url.starts_with("https://example.com"));

        let restored = InviteHandle::from_url(url).unwrap();
        assert_eq!(
            invite.get_inviter_pubkey_hex(),
            restored.get_inviter_pubkey_hex()
        );
        assert_eq!(
            invite.get_shared_secret_hex(),
            restored.get_shared_secret_hex()
        );
    }

    #[test]
    fn test_invite_serialize_roundtrip() {
        let kp = generate_keypair();
        let invite =
            InviteHandle::create_new(kp.public_key_hex.clone(), Some("device1".into()), Some(5))
                .unwrap();

        let json = invite.serialize().unwrap();
        let restored = InviteHandle::deserialize(json).unwrap();

        assert_eq!(
            invite.get_inviter_pubkey_hex(),
            restored.get_inviter_pubkey_hex()
        );
        assert_eq!(
            invite.get_shared_secret_hex(),
            restored.get_shared_secret_hex()
        );
    }

    #[test]
    fn test_invite_accept_returns_session_and_event() {
        let inviter_kp = generate_keypair();
        let invitee_kp = generate_keypair();

        let invite =
            InviteHandle::create_new(inviter_kp.public_key_hex.clone(), None, None).unwrap();
        let url = invite.to_url("https://example.com".to_string()).unwrap();

        let invite_copy = InviteHandle::from_url(url).unwrap();
        let result = invite_copy
            .accept(
                invitee_kp.public_key_hex.clone(),
                invitee_kp.private_key_hex.clone(),
                None,
            )
            .unwrap();

        assert!(!result.response_event_json.is_empty());
        assert!(result.session.can_send());
    }

    #[test]
    fn test_invite_process_response_yields_working_session_pair() {
        let alice_kp = generate_keypair();
        let bob_kp = generate_keypair();

        let invite = InviteHandle::create_new(alice_kp.public_key_hex.clone(), None, None).unwrap();
        let accept = invite
            .accept(
                bob_kp.public_key_hex.clone(),
                bob_kp.private_key_hex.clone(),
                None,
            )
            .unwrap();

        let processed = invite
            .process_response(
                accept.response_event_json.clone(),
                alice_kp.private_key_hex.clone(),
            )
            .unwrap()
            .unwrap();

        // Bob sends first (initiator), Alice receives first (non-initiator)
        let bob_send = accept.session.send_text("hi".to_string()).unwrap();
        let alice_decrypt = processed
            .session
            .decrypt_event(bob_send.outer_event_json.clone())
            .unwrap();
        assert!(alice_decrypt.plaintext.contains("hi"));

        // After receiving, Alice should be able to send.
        assert!(processed.session.can_send());

        let alice_reply = processed.session.send_text("ok".to_string()).unwrap();
        let bob_decrypt = accept
            .session
            .decrypt_event(alice_reply.outer_event_json)
            .unwrap();
        assert!(bob_decrypt.plaintext.contains("ok"));
    }

    #[test]
    fn test_session_send_receive() {
        // Setup: create invite and accept it to get two linked sessions
        let alice_kp = generate_keypair();
        let bob_kp = generate_keypair();

        // Alice creates invite
        let invite = InviteHandle::create_new(alice_kp.public_key_hex.clone(), None, None).unwrap();
        let invite_json = invite.serialize().unwrap();

        // Bob accepts invite
        let bob_invite = InviteHandle::deserialize(invite_json).unwrap();
        let accept_result = bob_invite
            .accept(
                bob_kp.public_key_hex.clone(),
                bob_kp.private_key_hex.clone(),
                None,
            )
            .unwrap();

        let bob_session = accept_result.session;

        // Alice processes the response to create her session
        // For this test, we use the session's shared state to verify
        assert!(bob_session.can_send());

        // Bob sends a message
        let send_result = bob_session.send_text("Hello Alice!".to_string()).unwrap();
        assert!(!send_result.outer_event_json.is_empty());
    }

    #[test]
    fn test_session_state_roundtrip() {
        let alice_kp = generate_keypair();
        let bob_kp = generate_keypair();

        let invite = InviteHandle::create_new(alice_kp.public_key_hex.clone(), None, None).unwrap();
        let invite_json = invite.serialize().unwrap();

        let bob_invite = InviteHandle::deserialize(invite_json).unwrap();
        let accept_result = bob_invite
            .accept(
                bob_kp.public_key_hex.clone(),
                bob_kp.private_key_hex.clone(),
                None,
            )
            .unwrap();

        let session = accept_result.session;
        let state_json = session.state_json().unwrap();

        // Restore session from state
        let restored = SessionHandle::from_state_json(state_json).unwrap();
        assert_eq!(session.can_send(), restored.can_send());
    }

    #[test]
    fn test_is_dr_message() {
        let kp = generate_keypair();
        let invite = InviteHandle::create_new(kp.public_key_hex.clone(), None, None).unwrap();

        let bob_kp = generate_keypair();
        let accept_result = invite
            .accept(
                bob_kp.public_key_hex.clone(),
                bob_kp.private_key_hex.clone(),
                None,
            )
            .unwrap();

        let session = accept_result.session;
        let send_result = session.send_text("test".to_string()).unwrap();

        // DR message should return true
        assert!(session.is_dr_message(send_result.outer_event_json));

        // Non-DR message should return false
        let non_dr_event = serde_json::json!({
            "id": "0000000000000000000000000000000000000000000000000000000000000000",
            "pubkey": "0000000000000000000000000000000000000000000000000000000000000000",
            "created_at": 0,
            "kind": 1,
            "tags": [],
            "content": "test",
            "sig": "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        });
        assert!(!session.is_dr_message(non_dr_event.to_string()));
    }
}
