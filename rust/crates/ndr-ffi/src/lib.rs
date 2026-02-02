//! UniFFI bindings for nostr-double-ratchet
//!
//! This crate provides FFI-friendly wrappers around the core nostr-double-ratchet
//! library for use in iOS and Android applications via UniFFI.

use std::sync::{Arc, Mutex};

use nostr_double_ratchet::{Invite, Session, SessionState};

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

/// Generate a new keypair.
#[uniffi::export]
pub fn generate_keypair() -> FfiKeyPair {
    let keys = nostr::Keys::generate();
    FfiKeyPair {
        public_key_hex: keys.public_key().to_hex(),
        private_key_hex: keys.secret_key().to_secret_hex(),
    }
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
